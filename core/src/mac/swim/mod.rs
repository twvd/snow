//! Sander-Wozniak Integrated Machine
//!
//! Floppy drive controller consisting of two different controllers:
//! Integrated Wozniak Machine, Integrated Sander Machine.

pub mod chain;
pub mod dcd;
pub mod device;
pub mod drive;
pub mod ism;
pub mod iwm;
pub mod stream;

use std::cell::Cell;
use std::collections::VecDeque;

use anyhow::{Context, Result, bail};
use chain::Chain;
use ism::{IsmError, IsmSetup, IsmStatus};
use rand::Rng;

use device::SwimBusDevice;
use iwm::{IwmMode, IwmStatus};
use serde::{Deserialize, Serialize};
use snow_floppy::flux::FluxTicks;

use crate::bus::native_clock::{C16M_FREQUENCY, NativeClock};
use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::devices::{BusOps, DeviceOps, MadeDevice};
use crate::mac::swim::ism::IsmFifoEntry;
use crate::tickable::{Tickable, Ticks};
use crate::types::LatchingEvent;

/// Time from one access to the SWIM to the next an overclocked CPU is held
/// to, in ns. Any closer, and the polling loops of the Sony driver run out
/// before the drive has answered.
const OVERCLOCK_ACCESS_INTERVAL_NS: Ticks = 1500;

/// The motor timer: "/ENBLx will be held active for 2^23 +/- 100 FCLK
/// periods" (SWIM Chip User's Reference, IWM mode register bit 2)
const MOTOR_TIMER: Ticks = 1 << 23;

/// Source of randomness for floppy analogue noise
///
/// Unseeded, this draws from the thread RNG. Once seeded, it produces a
/// deterministic sequence (SplitMix64) so runs can be reproduced.
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct WeakBitRng {
    state: Cell<Option<u64>>,
}

impl WeakBitRng {
    pub(crate) fn seed(&self, seed: Option<u64>) {
        self.state.set(seed);
    }

    pub(crate) fn random_bool(&self) -> bool {
        let Some(mut state) = self.state.get() else {
            return rand::rng().random();
        };
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.state.set(Some(state));

        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        z & (1 << 63) != 0
    }
}

enum FluxTransitionTime {
    /// 1
    Short,
    /// 01
    Medium,
    /// 001
    Long,
    /// Something else, out of spec.
    /// Contains the amount of bit cells
    OutOfSpec(usize),
}

impl FluxTransitionTime {
    pub fn from_ticks_ex(ticks: FluxTicks, _fast: bool, _highf: bool) -> Option<Self> {
        // Below is from Integrated Woz Machine (IWM) Specification, 1982, rev 19, page 4.
        // TODO fast/low frequency mode.. The Mac SE sets mode to 0x17, which makes things not work?
        match (true, true) {
            (false, false) | (true, false) => match ticks {
                7..=20 => Some(Self::Short),
                21..=34 => Some(Self::Medium),
                35..=48 => Some(Self::Long),
                56.. => Some(Self::OutOfSpec(ticks as usize / 14)),
                _ => None,
            },
            (true, true) | (false, true) => match ticks {
                8..=23 => Some(Self::Short),
                24..=39 => Some(Self::Medium),
                40..=55 => Some(Self::Long),
                56.. => Some(Self::OutOfSpec(ticks as usize / 16)),
                _ => None,
            },
        }
    }

    #[allow(dead_code)]
    pub fn from_ticks(ticks: FluxTicks) -> Option<Self> {
        Self::from_ticks_ex(ticks, true, true)
    }

    pub fn get_zeroes(self) -> usize {
        match self {
            Self::Short => 0,
            Self::Medium => 1,
            Self::Long => 2,
            Self::OutOfSpec(bc) => bc - 1,
        }
    }
}

#[derive(
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    strum::IntoStaticStr,
    Clone,
    Serialize,
    Deserialize,
)]
enum SwimMode {
    #[default]
    Iwm,
    Ism,
}

/// Where a device of the controller is
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Place {
    /// On a drive connector
    Connector(usize),
    /// At a place on the chain of the external drive connector
    Chain(usize),
}

/// The device at a place: a field-level borrow, so that the chip can be used next to it
fn device_in<'a>(
    drives: &'a mut [Option<Box<dyn SwimBusDevice>>],
    chain: &'a mut Chain,
    place: Place,
) -> Option<&'a mut Box<dyn SwimBusDevice>> {
    match place {
        Place::Connector(connector) => drives[connector].as_mut(),
        Place::Chain(place) => chain.device_at(place),
    }
}

/// Sander-Wozniak Integrated Machine - floppy drive controller
#[derive(Serialize, Deserialize)]
pub struct Swim {
    ism_available: bool,
    /// False on the SWIM2, which only has the ISM register set
    iwm_available: bool,

    /// The ticks of the controller in one second
    base_frequency: Ticks,
    cycles: Ticks,
    mode: SwimMode,

    /// Time of the last access by an overclocked CPU, after its hold, in ticks
    /// of the base frequency (native or C16M ticks)
    last_access: Ticks,

    pub ca0: bool,
    pub ca1: bool,
    pub ca2: bool,
    pub lstrb: bool,
    pub q6: bool,
    pub q7: bool,
    pub extdrive: bool,
    pub enable: bool,
    /// While > cycles, the MotorOn timer keeps the drive enabled after
    /// ENABLE (IWM) or MotorOn (ISM) went off
    enable_hold: Ticks,
    pub sel: bool,

    /// Internal drive select for SE
    pub(crate) intdrive: bool,

    chip: SwimChip,

    /// The devices on the drive connectors
    drives: [Option<Box<dyn SwimBusDevice>>; Self::CONNECTORS],

    /// The devices chained on the external drive connector, in front of its drive
    chain: Chain,

    /// Whether the external drive connector was enabled, with a device on its chain, when the
    /// control lines were last looked at
    chain_enabled: bool,

    /// The ticks since the byte in the write shifter started on its way to a device that
    /// exchanges bytes
    write_ticks: Ticks,

    /// The ticks the devices haven't been given yet. They get them before any other of their
    /// methods is called, not on each tick of the controller.
    pending_ticks: Ticks,

    /// The device and the level of the control lines last given to `set_lines()`
    lines_sent: (Place, u8),

    /// SEL and the internal drive select, which the machine sets at any time, as they were
    /// when the control lines were last looked at
    machine_lines: (bool, bool),

    /// Whether the disk of the selected device spun when it was last asked. None: it is asked
    /// at the next tick.
    #[serde(skip)]
    spinning: Option<bool>,

    pub dbg_pc: u32,
    pub dbg_break: LatchingEvent,
}

/// The registers of the chip, with its shifters and its FIFO
///
/// They are apart from the drives so that the functions that move the bits between the chip
/// and a disk can be given the [`FloppyStream`] of one drive: a function that has the whole
/// controller can't also hold a part of one of its drives.
#[derive(Serialize, Deserialize)]
struct SwimChip {
    bit_cycles: Ticks,

    iwm_status: IwmStatus,
    iwm_mode: IwmMode,
    shdata: u8,
    datareg: u8,
    iwm_zeroes: usize,
    write_shift: u8,
    write_pos: usize,
    write_buffer: Option<u8>,

    ism_phase_mask: u8,
    ism_error: IsmError,
    ism_mode: IsmStatus,
    ism_params: Vec<u8>,
    ism_param_idx: usize,
    ism_setup: IsmSetup,
    ism_switch_ctr: usize,
    ism_fifo: VecDeque<IsmFifoEntry>,
    ism_shreg: u16,
    ism_synced: bool,
    ism_shreg_cnt: usize,
    ism_crc: u16,
    /// ISM write shift register (16-bit MFM encoded data)
    ism_write_shreg: u16,
    /// Bits remaining in write shift register
    ism_write_shreg_cnt: u8,
    /// Previous data bit for MFM clock calculation
    ism_write_prev_bit: bool,
}

impl Swim {
    /// The number of drive connectors
    const CONNECTORS: usize = 3;

    /// The ids of the slots of the bus: the drive connectors, counted from 1
    pub const SLOT_IDS: [&str; Self::CONNECTORS] = ["1", "2", "3"];

    /// The controller is ticked at `base_frequency`, which is its FCLK. Its connectors are
    /// empty.
    pub fn new(ism_available: bool, base_frequency: Ticks) -> Self {
        Self {
            drives: Default::default(),
            pending_ticks: 0,
            chain: Chain::new(base_frequency),
            chain_enabled: false,
            write_ticks: 0,
            lines_sent: (Place::Connector(0), 0),
            machine_lines: (false, false),
            spinning: None,
            ism_available,
            iwm_available: true,

            base_frequency,
            cycles: 0,
            // SWIM boots in IWM mode
            mode: Default::default(),

            last_access: 0,

            ca0: false,
            ca1: false,
            ca2: false,
            lstrb: false,
            q6: false,
            q7: false,
            extdrive: false,
            sel: false,
            intdrive: false,

            chip: SwimChip {
                bit_cycles: 0,

                shdata: 0,
                datareg: 0,
                iwm_zeroes: 0,
                write_shift: 0,
                write_pos: 0,
                write_buffer: None,

                iwm_status: IwmStatus(0),
                iwm_mode: IwmMode(0),

                ism_phase_mask: 0xF0,
                ism_error: IsmError(0),
                ism_mode: IsmStatus(0),
                ism_params: vec![0; 16],
                ism_param_idx: 0,
                ism_setup: IsmSetup(0),
                ism_switch_ctr: 0,
                ism_fifo: VecDeque::new(),
                ism_shreg: 0,
                ism_synced: false,
                ism_shreg_cnt: 0,
                ism_crc: SwimChip::ISM_CRC_INIT,
                ism_write_shreg: 0,
                ism_write_shreg_cnt: 0,
                ism_write_prev_bit: false,
            },

            enable: false,
            enable_hold: 0,
            dbg_pc: 0,
            dbg_break: LatchingEvent::default(),
        }
    }

    pub fn new_swim2(base_frequency: Ticks) -> Self {
        let mut swim = Self::new(true, base_frequency);
        swim.iwm_available = false;
        swim.mode = SwimMode::Ism;
        swim.chip.ism_mode.set_ism(true);
        swim.chip.ism_params = vec![0; 4];
        swim
    }

    /// Resets the controller, as the system RESET line does: back to the
    /// power-on mode with all drive control lines low. The drives keep their
    /// disks, and the chained devices stay. Each device is told of the reset.
    pub fn reset(&mut self) {
        let base_frequency = self.base_frequency;
        let mut fresh = if self.iwm_available {
            Self::new(self.ism_available, base_frequency)
        } else {
            Self::new_swim2(base_frequency)
        };
        std::mem::swap(&mut fresh.drives, &mut self.drives);
        std::mem::swap(&mut fresh.chain, &mut self.chain);
        fresh.chain.rewind();
        fresh.pending_ticks = self.pending_ticks;
        fresh.lines_sent = self.lines_sent;
        fresh.cycles = self.cycles;
        fresh.last_access = self.last_access;
        fresh.sel = self.sel;
        fresh.intdrive = self.intdrive;
        fresh.dbg_pc = self.dbg_pc;
        fresh.dbg_break = std::mem::take(&mut self.dbg_break);
        fresh.sync_devices();
        for device in fresh.devices_mut() {
            device.reset();
        }
        *self = fresh;
    }

    /// Native ticks an access is held for, called after device_sync(). When
    /// overclocking, the CPU runs the code between two accesses faster than
    /// the real hardware: the accesses are held to be at least
    /// OVERCLOCK_ACCESS_INTERVAL_NS apart. Without overclocking, nothing is
    /// held.
    pub fn hold<const C16M: bool>(&mut self, clock: &NativeClock<C16M>) -> Ticks {
        if !clock.is_overclocking() {
            return 0;
        }
        // After device_sync(), the clock is up to date when overclocking
        self.hold_at(clock.native_ticks(), clock.native_frequency())
    }

    /// The same as hold(), for a SWIM clocked by C16M: C16M ticks an access
    /// is held for, called after device16_sync()
    pub fn hold16(&mut self, clock: &NativeClock) -> Ticks {
        if !clock.is_overclocking() {
            return 0;
        }
        // After device16_sync(), the clock is up to date
        self.hold_at(clock.c16m_ticks(), C16M_FREQUENCY)
    }

    fn hold_at(&mut self, now: Ticks, frequency: Ticks) -> Ticks {
        let interval = (frequency * OVERCLOCK_ACCESS_INTERVAL_NS).div_ceil(1_000_000_000);
        let hold = (self.last_access + interval).saturating_sub(now);
        self.last_access = now + hold;
        hold
    }

    fn get_selected_drive_idx(&self) -> usize {
        if self.mode == SwimMode::Iwm {
            if self.extdrive {
                1
            } else if self.intdrive {
                2
            } else {
                0
            }
        } else {
            // ISM
            if self.chip.ism_mode.drive2_enable() {
                1
            } else if self.chip.ism_mode.drive1_enable() {
                if self.intdrive { 2 } else { 0 }
            } else {
                // ???
                0
            }
        }
    }

    /// Time the ISM needs between two accesses, in FCLK cycles: "the time
    /// between successive chip accesses must be no less than 4 FCLKs (if the
    /// Setup register's FCLK/2 bit is "0") or 8 FCLKs (if FCLK/2 is "1")"
    /// (SWIM Chip User's Reference, 1988).
    pub fn ism_recovery(&self) -> Ticks {
        match (self.chip.ism_mode.ism(), self.chip.ism_setup.fclk_div2()) {
            (false, _) => 0,
            (true, false) => 4,
            (true, true) => 8,
        }
    }

    /// Gives the devices the time that passed since they last got it. Called before any
    /// other method of a device.
    fn sync_devices(&mut self) {
        if self.pending_ticks == 0 {
            return;
        }
        let ticks = self.pending_ticks;
        for device in self.devices_mut() {
            device.tick(ticks);
        }
        self.pending_ticks = 0;
    }

    /// Every device of the controller: those on the drive connectors, then the chained ones
    fn devices_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn SwimBusDevice>> {
        self.drives
            .iter_mut()
            .flatten()
            .chain(self.chain.devices_mut())
    }

    /// Whether the external drive connector is enabled, with a device on its chain. The
    /// chain is only followed in IWM mode: in ISM mode the drive of the connector answers.
    fn chain_is_enabled(&self) -> bool {
        !self.chain.is_empty()
            && self.mode == SwimMode::Iwm
            && self.extdrive
            && self.iwm_drive_enabled()
    }

    /// The device the controller talks to: the drive of the selected connector or, while the
    /// external connector is enabled, the device of its chain that the selection has reached
    fn selected(&self) -> Place {
        if self.chain_is_enabled()
            && let Some(place) = self.chain.reached()
        {
            return Place::Chain(place);
        }
        Place::Connector(self.get_selected_drive_idx())
    }

    /// The ticks a byte takes between the IWM and a device that exchanges bytes: eight bit
    /// cells of 16 periods of C8M each
    fn byte_ticks(&self) -> Ticks {
        8 * 16 * self.base_frequency / (C16M_FREQUENCY / 2)
    }

    /// Moves whole bytes between the IWM and the selected device, one that has no stream:
    /// it exchanges bytes and not the bits of a disk
    fn exchange_bytes(&mut self, place: Place, ticks: Ticks) {
        let byte_ticks = self.byte_ticks();
        let Some(device) = device_in(&mut self.drives, &mut self.chain, place) else {
            return;
        };

        // Writing: the device has a byte once its eight bits have left the shifter
        if self.chip.write_pos != 0 {
            self.write_ticks += ticks;
            if self.write_ticks >= byte_ticks {
                device.write_byte(self.chip.write_shift);
                self.chip.write_pos = 0;
            }
        }
        if self.chip.write_pos == 0
            && let Some(byte) = self.chip.write_buffer.take()
        {
            self.chip.write_shift = byte;
            self.chip.write_pos = 8;
            self.write_ticks = 0;
        }

        // Reading: a byte of the device goes to the data register
        if let Some(byte) = device.read_byte(ticks) {
            self.chip.datareg = byte;
        }
    }

    /// Gets the chain of the external connector, for the emulator to act on its devices
    pub(crate) fn chain(&mut self) -> &mut dyn BusOps {
        self.sync_devices();
        // A device may come or go
        self.spinning = None;
        &mut self.chain
    }

    /// Gets the device on a connector, for the emulator to act on it
    pub(crate) fn drive_at(&mut self, connector: usize) -> Option<&mut dyn SwimBusDevice> {
        self.sync_devices();
        // A disk may go in or out
        self.spinning = None;
        let device = self.drives.get_mut(connector)?.as_mut()?;
        Some(&mut **device)
    }

    /// The strobe line rose: the selected device gets it. A chained device says whether it
    /// passes the selection on to the next device of the chain.
    fn strobe(&mut self) {
        let selected = self.selected();
        let lines = self.get_selected_drive_reg_u8();
        self.sync_devices();
        self.spinning = None;
        if let Some(device) = device_in(&mut self.drives, &mut self.chain, selected)
            && device.strobe(lines)
            && matches!(selected, Place::Chain(_))
        {
            self.chain.pass();
        }
    }

    /// Gets the level of the sense line: that of the selected device, or high on a connector
    /// with no device, which has a pull-up
    fn sense(&mut self) -> bool {
        let selected = self.selected();
        let lines = self.get_selected_drive_reg_u8();
        self.sync_devices();
        device_in(&mut self.drives, &mut self.chain, selected)
            .is_none_or(|device| device.sense(lines))
    }

    /// Gives the selected device the level of the control lines, if a line changed or
    /// another device is selected
    fn update_lines(&mut self) {
        self.machine_lines = (self.sel, self.intdrive);
        self.chain_enabled = self.chain_is_enabled();
        if !self.chain_enabled {
            self.chain.rewind();
        }
        let lines = (self.selected(), self.get_selected_drive_reg_u8());
        if lines != self.lines_sent {
            self.lines_sent = lines;
            self.sync_devices();
            self.spinning = None;
            if let Some(device) = device_in(&mut self.drives, &mut self.chain, lines.0) {
                device.set_lines(lines.1);
            }
        }
    }

    /// Converts the four register selection I/Os to a u8 value which can be used
    /// to convert to an enum value.
    fn get_selected_drive_reg_u8(&self) -> u8 {
        let mut v = 0;
        if self.ca2 {
            v |= 0b1000;
        };
        if self.ca1 {
            v |= 0b0100;
        };
        if self.ca0 {
            v |= 0b0010;
        };
        if self.sel {
            v |= 0b0001;
        };
        v
    }
}

/// The connector of a slot
fn slot_number(slot: &str) -> Option<usize> {
    Swim::SLOT_IDS.iter().position(|&id| id == slot)
}

impl BusOps for Swim {
    fn device(&mut self, slot: &str) -> Option<&mut dyn DeviceOps> {
        let device = self.drive_at(slot_number(slot)?)?;
        // A device on a drive connector is a DeviceOps too
        Some(device)
    }

    fn attach(&mut self, slot: &str, device: MadeDevice) -> Result<()> {
        let connector = slot_number(slot).context("No such drive connector")?;
        let MadeDevice::Swim(mut device) = device else {
            bail!("Not a device for a drive connector");
        };
        if self.drives[connector].is_some() {
            bail!("The drive connector already has a device");
        }
        self.sync_devices();
        device.connected(connector, self.base_frequency);
        self.drives[connector] = Some(device);
        self.spinning = None;
        Ok(())
    }

    fn detach(&mut self, slot: &str) -> Result<()> {
        let connector = slot_number(slot).context("No such drive connector")?;
        if self.drives[connector].take().is_none() {
            bail!("The drive connector has no device");
        }
        self.spinning = None;
        Ok(())
    }
}

impl BusMember<Address> for Swim {
    fn read(&mut self, addr: Address) -> Option<u8> {
        let result = match self.mode {
            SwimMode::Iwm => self.iwm_read(addr),
            SwimMode::Ism => self.ism_read(addr),
        };
        self.update_lines();
        result
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        match self.mode {
            SwimMode::Iwm => self.iwm_write(addr, val),
            SwimMode::Ism => self.ism_write(addr, val),
        }
        self.update_lines();
        Some(())
    }
}

impl Tickable for Swim {
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        self.cycles += ticks;
        self.pending_ticks += ticks;
        if (self.sel, self.intdrive) != self.machine_lines
            || (self.chain_enabled && !self.chain_is_enabled())
        {
            // The machine changed a line, or the motor timer ended the enable of the external
            // connector, with no access
            self.update_lines();
        }

        // Whether a disk spins only changes when the controller or the emulator calls its
        // device: one that doesn't isn't asked again until then
        if self.spinning != Some(false) {
            self.sync_devices();
            let selected = self.selected();
            let device = device_in(&mut self.drives, &mut self.chain, selected);
            let Some(stream) = device.map(|device| device.stream()) else {
                // No device: nothing to read or write
                self.spinning = Some(false);
                return self.tick_no_disk(ticks);
            };
            let Some(stream) = stream else {
                // A device with no stream exchanges whole bytes, in IWM mode
                if self.mode == SwimMode::Iwm {
                    self.exchange_bytes(selected, ticks);
                    return Ok(ticks);
                }
                return self.tick_no_disk(ticks);
            };
            let stream = Some(stream).filter(|stream| stream.spinning);
            self.spinning = Some(stream.is_some());
            if let Some(stream) = stream {
                // Advance read/write operation
                let head = stream.head(self.sel);
                match self.mode {
                    SwimMode::Iwm => self.chip.iwm_tick(stream, head, ticks, self.cycles)?,
                    SwimMode::Ism => self.chip.ism_tick(stream, head, ticks)?,
                }
                return Ok(ticks);
            }
        }
        self.tick_no_disk(ticks)
    }
}

impl Swim {
    /// A tick with no disk spinning under the selected device's heads
    fn tick_no_disk(&mut self, ticks: Ticks) -> Result<Ticks> {
        if self.mode == SwimMode::Ism {
            let byte_ticks = self.base_frequency * 16 / 1_000_000;
            self.chip.ism_tick_no_disk(ticks, byte_ticks);
        }
        Ok(ticks)
    }
}

impl Debuggable for Swim {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{
            dbgprop_bool, dbgprop_byte, dbgprop_byte_bin, dbgprop_enum, dbgprop_group,
            dbgprop_header, dbgprop_string, dbgprop_udec, dbgprop_word, dbgprop_word_bin,
        };

        let mut result = vec![
            dbgprop_enum!("Mode", self.mode),
            dbgprop_udec!("ISM switch counter", self.chip.ism_switch_ctr),
            dbgprop_group!(
                "I/O",
                vec![
                    dbgprop_bool!("CA0", self.ca0),
                    dbgprop_bool!("CA1", self.ca1),
                    dbgprop_bool!("CA2", self.ca2),
                    dbgprop_bool!("LSTRB", self.lstrb),
                    dbgprop_bool!("Q6", self.q6),
                    dbgprop_bool!("Q7", self.q7),
                    dbgprop_bool!("Extdrive", self.extdrive),
                    dbgprop_bool!("Enable", self.enable),
                    dbgprop_bool!("SEL", self.sel),
                    dbgprop_bool!("Intdrive", self.intdrive),
                ]
            ),
            dbgprop_group!(
                "IWM",
                vec![
                    dbgprop_header!("Registers"),
                    dbgprop_byte!("Status", self.chip.iwm_status.0),
                    dbgprop_byte!("Mode", self.chip.iwm_mode.0),
                    dbgprop_header!("Reading"),
                    dbgprop_byte!("Data register", self.chip.datareg),
                    dbgprop_byte_bin!("Read shifter", self.chip.shdata),
                    dbgprop_udec!("Zeroes", self.chip.iwm_zeroes),
                    dbgprop_header!("Writing"),
                    dbgprop_byte_bin!("Write shifter", self.chip.write_shift),
                    dbgprop_udec!("Write position", self.chip.write_pos),
                    dbgprop_byte!("Write buffer", self.chip.write_buffer.unwrap_or(0)),
                ]
            ),
            dbgprop_group!(
                "ISM",
                vec![
                    dbgprop_header!("Registers"),
                    dbgprop_byte_bin!("Phase mask", self.chip.ism_phase_mask),
                    dbgprop_byte!("Error", self.chip.ism_error.0),
                    dbgprop_byte!("Mode", self.chip.ism_mode.0),
                    dbgprop_byte!("Setup", self.chip.ism_setup.0),
                    dbgprop_header!("Parameters"),
                    dbgprop_udec!("Parameter index", self.chip.ism_param_idx),
                    dbgprop_group!(
                        "Parameters",
                        Vec::from_iter(
                            self.chip
                                .ism_params
                                .iter()
                                .enumerate()
                                .map(|(i, p)| dbgprop_byte!(format!("[{}]", i), *p))
                        )
                    ),
                    dbgprop_header!("Reading/writing"),
                    dbgprop_group!(
                        "FIFO",
                        Vec::from_iter(self.chip.ism_fifo.iter().enumerate().map(
                            |(i, p)| dbgprop_string!(
                                format!("[{}]", i),
                                format!("{} {:08b} (${:02X})", p, p.inner(), p.inner())
                            )
                        ))
                    ),
                    dbgprop_word_bin!("Shifter", self.chip.ism_shreg),
                    dbgprop_udec!("Shifter bits", self.chip.ism_shreg_cnt),
                    dbgprop_bool!("Synchronized", self.chip.ism_synced),
                    dbgprop_word!("CRC", self.chip.ism_crc),
                ]
            ),
        ];
        if !self.chain.is_empty() {
            result.push(dbgprop_udec!("HD20 chain position", self.chain.passed()));
        }
        result
    }
}

#[cfg(test)]
use drive::{DriveType, FloppyDrive};

#[cfg(test)]
impl Swim {
    /// Puts a drive of each of these types on the connectors, from the first
    pub(super) fn with_drives(mut self, drives: &[DriveType]) -> Self {
        for (slot, drive_type) in Self::SLOT_IDS.iter().zip(drives) {
            let drive = Box::new(FloppyDrive::new(*drive_type));
            self.attach(slot, MadeDevice::Swim(drive)).unwrap();
        }
        self
    }

    /// The floppy drive on a connector, for the tests that look inside a drive
    pub(super) fn drive(&mut self, connector: usize) -> &mut FloppyDrive {
        self.drive_at(connector).unwrap().as_floppy_drive()
    }

    /// Puts these devices on the chain of the external connector, from its first place
    pub(super) fn with_chain(mut self, devices: Vec<Box<dyn SwimBusDevice>>) -> Self {
        for (slot, device) in Chain::SLOT_IDS.iter().zip(devices) {
            self.chain().attach(slot, MadeDevice::Swim(device)).unwrap();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use snow_floppy::loaders::{FloppyImageLoader, Moof};
    use snow_floppy::{Floppy, FloppyImage, FloppyType, TrackLength};

    use super::*;
    use crate::devices::{DeviceOps, FunctionConfig, FunctionMut, ModelInfo};

    const CA0H: Address = 1 << 9;
    const CA1H: Address = 3 << 9;
    const CA2H: Address = 5 << 9;
    const LSTRBL: Address = 6 << 9;
    const LSTRBH: Address = 7 << 9;
    const DISABLE: Address = 8 << 9;
    const ENABLE: Address = 9 << 9;
    const EXTDRIVEL: Address = 10 << 9;
    const EXTDRIVEH: Address = 11 << 9;
    const Q6L: Address = 12 << 9;
    const Q6H: Address = 13 << 9;
    const Q7L: Address = 14 << 9;
    const Q7H: Address = 15 << 9;

    const ISM_DATA: Address = 0;
    const ISM_MARK: Address = 1 << 9;
    const ISM_CRC: Address = 2 << 9;
    const ISM_ERROR: Address = 2 << 9;
    const ISM_SETUP: Address = 5 << 9;
    const ISM_MODE_ZERO: Address = 6 << 9;
    const ISM_MODE_ONE: Address = 7 << 9;
    const ISM_HANDSHAKE: Address = 7 << 9;

    const MODE_CLEAR_FIFO: u8 = 0x01;
    const MODE_DRIVE1: u8 = 0x02;
    const MODE_ACTION: u8 = 0x08;
    const MODE_WRITE: u8 = 0x10;
    const MODE_MOTOR: u8 = 0x80;

    const DRIVE_MOTOR_ON: u8 = 0b0100;

    /// The track the head of a new drive is on
    const TRACK: usize = 4;

    /// The lengths of the controller ticks of the tests: uneven, and shorter than a bit cell
    const STEPS: [Ticks; 5] = [1, 3, 7, 2, 11];

    /// Adds a value to the running sum of what a test read. The sums the tests expect were
    /// recorded before the bits of a disk went through `FloppyStream`: the same bytes must
    /// still come at the same times.
    fn mix(sum: u64, value: u64) -> u64 {
        (sum ^ value).wrapping_mul(0x0100_0000_01B3)
    }

    /// Byte `n` of a sequence that repeats from one run to the next, in which no two bits in a
    /// row are zero, as on a GCR disk: the IWM reads no weak bit in it
    fn gcr_byte(n: usize) -> u8 {
        let x = (n as u32).wrapping_mul(2_654_435_761) >> 24;
        0xAA | (x as u8 & 0x55)
    }

    /// The controller with a disk turning in its first drive, which is ready
    fn swim_with(drive: DriveType, image: FloppyImage) -> Swim {
        let mut swim = Swim::new(true, 16_000_000).with_drives(&[drive]);
        swim.drive(0).disk_insert(image).unwrap();
        swim.drive(0).write_drive_reg(DRIVE_MOTOR_ON);
        swim.drive(0).not_ready = 0;
        swim
    }

    /// Reads the IWM data register for a number of ticks. Returns the number of bytes, and
    /// the sum of each byte with the time it came at.
    fn iwm_read(swim: &mut Swim, ticks: Ticks) -> (usize, u64) {
        let (mut count, mut sum, mut time) = (0, 0, 0);
        for step in STEPS.iter().cycle() {
            if time >= ticks {
                break;
            }
            swim.tick(*step, ()).unwrap();
            time += step;
            let byte = swim.read(Q6L).unwrap();
            if byte & 0x80 != 0 {
                count += 1;
                sum = mix(mix(sum, time), byte.into());
            }
        }
        (count, sum)
    }

    #[test]
    fn iwm_bitstream_track_bit_for_bit() {
        let mut image = FloppyImage::new(FloppyType::Mac800K, "");
        let TrackLength::Bits(len) = image.get_track_length(0, TRACK) else {
            unreachable!()
        };
        for pos in 0..len {
            let bit = gcr_byte(pos / 8) & (0x80 >> (pos % 8)) != 0;
            image.set_track_bit(0, TRACK, pos, bit);
        }
        let mut swim = swim_with(DriveType::GCR800K, image);
        swim.read(ENABLE);
        let turn = swim.drive(0).get_ticks_per_bit() * len as Ticks;

        // A turn and a quarter
        assert_eq!(
            iwm_read(&mut swim, turn + turn / 4),
            (11662, 8_261_727_604_954_187_320)
        );

        // Write over a part of the track: a byte each time the handshake register says the
        // write buffer is free
        swim.write(Q6H, 0);
        swim.write(Q7H, gcr_byte(1000));
        let mut written = 1;
        for step in STEPS.iter().cycle() {
            if written == 500 {
                break;
            }
            swim.tick(*step, ()).unwrap();
            if swim.read(Q6L).unwrap() & 0x80 != 0 {
                swim.write(Q6H, gcr_byte(1000 + written));
                written += 1;
            }
        }
        swim.read(Q6L);
        swim.read(Q7L);

        assert_eq!(
            iwm_read(&mut swim, turn + turn / 4),
            (11662, 2_353_383_110_320_722_410)
        );
    }

    /// The CRC-32 of a MOOF file
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// An 800K disk whose track under the head is a flux track, as a MOOF file has it
    fn flux_image() -> FloppyImage {
        // The lengths of the transitions, in ticks: short, medium and long ones, one too
        // short to count and one past the longest in the specification
        const LENGTHS: [u8; 10] = [16, 32, 16, 48, 5, 16, 20, 36, 60, 16];
        let flux: Vec<u8> = (0..20_000)
            .map(|n: u32| LENGTHS[(n.wrapping_mul(2_654_435_761) >> 24) as usize % 10])
            .collect();

        let mut file = b"MOOF\xFF\n\r\n\0\0\0\0".to_vec();
        // INFO: version 1, an 800K disk
        let mut info = [0; 44];
        info[0] = 1;
        info[1] = 2;
        file.extend(b"INFO");
        file.extend(44u32.to_le_bytes());
        file.extend(info);
        // FLUX: the track is entry 0 of TRKS
        let mut map = [0xFF; 160];
        map[TRACK * 2] = 0;
        file.extend(b"FLUX");
        file.extend(160u32.to_le_bytes());
        file.extend(map);
        // TRKS: its bytes start at block 3
        let mut trks = [0; 1280];
        trks[0] = 3;
        trks[2] = flux.len().div_ceil(512) as u8;
        trks[4..8].copy_from_slice(&(flux.len() as u32).to_le_bytes());
        file.extend(b"TRKS");
        file.extend(1280u32.to_le_bytes());
        file.extend(trks);
        file.resize(3 * 512, 0);
        file.extend(&flux);
        let crc = crc32(&file[12..]);
        file[8..12].copy_from_slice(&crc.to_le_bytes());

        Moof::load(&file, None).unwrap()
    }

    #[test]
    fn iwm_flux_track_bit_for_bit() {
        let mut swim = swim_with(DriveType::GCR800K, flux_image());
        swim.read(ENABLE);
        // More than a turn: 20000 transitions of 28 ticks on average
        assert_eq!(
            iwm_read(&mut swim, 700_000),
            (4671, 4_005_945_229_481_203_460)
        );
    }

    #[test]
    fn a_byte_written_to_a_stopped_drive_stays() {
        let mut swim = Swim::new(true, 16_000_000).with_drives(&[DriveType::GCR800K]);
        swim.drive(0)
            .disk_insert(FloppyImage::new(FloppyType::Mac800K, ""))
            .unwrap();
        // The IWM writes, but the motor of the drive is off: the disk doesn't spin
        swim.read(ENABLE);
        swim.read(Q6H);
        swim.write(Q7H, 0xAA);
        swim.tick(10_000, ()).unwrap();
        // The handshake register: the write buffer still holds the byte
        swim.read(Q6L);
        assert_eq!(swim.read(Q7H).unwrap() & 0x80, 0);

        // Once the disk spins, the byte goes to it
        swim.drive(0).write_drive_reg(DRIVE_MOTOR_ON);
        swim.drive(0).not_ready = 0;
        swim.tick(10_000, ()).unwrap();
        assert_eq!(swim.read(Q7H).unwrap() & 0x80, 0x80);
    }

    #[test]
    fn ism_mfm_track_bit_for_bit() {
        // A disk with no transition at all: only what the test writes can be read
        let mut image = FloppyImage::new(FloppyType::Mfm144M, "");
        let TrackLength::Bits(len) = image.get_track_length(0, TRACK) else {
            unreachable!()
        };
        for pos in 0..len {
            image.set_track_bit(0, TRACK, pos, false);
        }
        let mut swim = swim_with(DriveType::SuperDrive, image);
        let turn = swim.drive(0).get_ticks_per_bit() * len as Ticks;

        // To ISM mode: Q6 on, then the sequence on the mode register
        swim.write(Q6H, 0);
        for val in [0x57, 0x17, 0x57, 0x57] {
            swim.write(Q7H, val);
        }
        assert_eq!(swim.mode, SwimMode::Ism);
        swim.write(ISM_SETUP, 0);
        swim.write(ISM_MODE_ONE, MODE_DRIVE1 | MODE_MOTOR);

        // Write three marks, 40 bytes and their CRC
        let mut bytes = [(ISM_MARK, 0xA1); 3].to_vec();
        bytes.extend((0..40).map(|n| (ISM_DATA, gcr_byte(n) ^ 0x3C)));
        bytes.push((ISM_CRC, 0));
        let mut bytes = bytes.into_iter();

        swim.write(ISM_MODE_ONE, MODE_WRITE);
        swim.write(ISM_MODE_ONE, MODE_CLEAR_FIFO);
        swim.write(ISM_MODE_ZERO, MODE_CLEAR_FIFO);
        let (reg, val) = bytes.next().unwrap();
        swim.write(reg, val);
        swim.write(ISM_MODE_ONE, MODE_ACTION);
        for step in STEPS.iter().cycle() {
            if !swim.chip.ism_mode.action() {
                break;
            }
            if swim.read(ISM_HANDSHAKE).unwrap() & 0x80 != 0
                && let Some((reg, val)) = bytes.next()
            {
                swim.write(reg, val);
            }
            swim.tick(*step, ()).unwrap();
        }
        // The write ends when the FIFO runs empty
        assert_eq!(swim.read(ISM_ERROR), Some(0x01));

        // Read a whole turn: each byte with the handshake register that announced it and the
        // time it came at
        swim.write(ISM_MODE_ZERO, MODE_WRITE | MODE_ACTION);
        swim.write(ISM_MODE_ONE, MODE_CLEAR_FIFO);
        swim.write(ISM_MODE_ZERO, MODE_CLEAR_FIFO);
        swim.write(ISM_MODE_ONE, MODE_ACTION);
        let (mut count, mut sum, mut time) = (0, 0, 0);
        for step in STEPS.iter().cycle() {
            if time >= turn {
                break;
            }
            swim.tick(*step, ()).unwrap();
            time += step;
            let handshake = swim.read(ISM_HANDSHAKE).unwrap();
            if handshake & 0x80 != 0 {
                let reg = if handshake & 0x01 != 0 {
                    ISM_MARK
                } else {
                    ISM_DATA
                };
                let byte = swim.read(reg).unwrap();
                count += 1;
                sum = mix(mix(mix(sum, time), handshake.into()), byte.into());
            }
        }
        assert_eq!((count, sum), (45, 9_063_078_967_827_888_595));
        assert_eq!(swim.read(ISM_ERROR), Some(0x00));
    }

    /// A device that notes what the controller gives it, and answers on the sense line when
    /// CA2 and SEL are high
    #[derive(Default, Serialize, Deserialize)]
    struct Probe {
        #[serde(skip)]
        notes: Arc<Mutex<Vec<String>>>,
        /// It is chained: it passes the selection on at a strobe
        chained: bool,
        /// The byte it sends when it is next asked for one
        byte: Option<u8>,
    }

    impl Probe {
        fn note(&self, note: String) {
            self.notes.lock().unwrap().push(note);
        }
    }

    impl DeviceOps for Probe {
        fn info(&self) -> &'static ModelInfo {
            &drive::INFO_800K
        }

        fn config(&self) -> FunctionConfig {
            FunctionConfig::Floppy
        }

        fn function(&mut self) -> FunctionMut<'_> {
            unreachable!()
        }
    }

    #[typetag::serde]
    impl SwimBusDevice for Probe {
        fn connected(&mut self, connector: usize, tick_rate: Ticks) {
            self.note(format!("connected {} {}", connector, tick_rate));
        }

        fn set_lines(&mut self, lines: u8) {
            self.note(format!("lines {:04b}", lines));
        }

        fn strobe(&mut self, lines: u8) -> bool {
            self.note(format!("strobe {:04b}", lines));
            self.chained
        }

        fn sense(&self, lines: u8) -> bool {
            lines == 0b1001
        }

        fn tick(&mut self, ticks: Ticks) {
            self.note(format!("tick {}", ticks));
        }

        fn reset(&mut self) {
            self.note("reset".to_string());
        }

        fn read_byte(&mut self, _ticks: Ticks) -> Option<u8> {
            self.byte.take()
        }

        fn write_byte(&mut self, byte: u8) {
            self.note(format!("byte {:02X}", byte));
        }
    }

    /// The controller with a probe on its external connector and two chained in front of
    /// it, with the notes of the three: the two of the chain, then the one on the connector.
    /// The first of the chain has a byte to send.
    fn swim_with_chain() -> (Swim, [Arc<Mutex<Vec<String>>>; 3]) {
        let chained = |byte| Probe {
            chained: true,
            byte,
            ..Default::default()
        };
        let probes = [chained(Some(0xC5)), chained(None), Probe::default()];
        let notes = probes.each_ref().map(|probe| probe.notes.clone());
        let [first, second, drive] = probes.map(|probe| Box::new(probe) as Box<dyn SwimBusDevice>);
        let mut swim = Swim::new(false, 16_000_000).with_chain(vec![first, second]);
        swim.attach("2", MadeDevice::Swim(drive)).unwrap();
        for notes in &notes {
            take_notes(notes);
        }
        (swim, notes)
    }

    /// Takes the notes of a probe, without those of the time passing
    fn take_notes(notes: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        let notes = std::mem::take(&mut *notes.lock().unwrap());
        notes
            .into_iter()
            .filter(|note| !note.starts_with("tick"))
            .collect()
    }

    #[test]
    fn the_selection_goes_down_the_chain_of_the_external_connector() {
        let (mut swim, notes) = swim_with_chain();
        let [first, second, drive] = notes.each_ref();

        // The first device of the chain answers when the connector is enabled
        swim.read(EXTDRIVEH);
        assert_eq!(take_notes(drive), ["lines 0000"]);
        swim.read(ENABLE);
        swim.read(CA2H);
        assert_eq!(take_notes(first), ["lines 0000", "lines 1000"]);

        // A strobe: it passes the selection on, once, however often LSTRBH is accessed
        for _ in 0..3 {
            swim.read(LSTRBH);
        }
        swim.read(LSTRBL);
        assert_eq!(take_notes(first), ["strobe 1000"]);
        assert_eq!(take_notes(second), ["lines 1000"]);

        // Past the last device of the chain is the drive of the connector, which keeps
        // the selection
        swim.read(LSTRBH);
        swim.read(LSTRBL);
        assert_eq!(take_notes(second), ["strobe 1000"]);
        assert_eq!(take_notes(drive), ["lines 1000"]);
        swim.read(LSTRBH);
        swim.read(LSTRBL);
        assert_eq!(take_notes(drive), ["strobe 1000"]);

        // The motor timer keeps the connector enabled after ENABLE goes off: the selection
        // stays where it is
        swim.read(DISABLE);
        swim.read(ENABLE);
        swim.sel = true;
        swim.tick(1, ()).unwrap();
        assert_eq!(take_notes(drive), ["lines 1001"]);

        // When the timer runs out, it goes back to the first device
        swim.read(DISABLE);
        swim.tick(MOTOR_TIMER + 1, ()).unwrap();
        swim.read(ENABLE);
        assert_eq!(take_notes(first), ["lines 1001"]);

        // The chain isn't on the internal connector
        swim.read(EXTDRIVEL);
        swim.read(LSTRBH);
        assert!(take_notes(first).is_empty());
        assert!(take_notes(second).is_empty());
    }

    #[test]
    fn a_device_with_no_stream_exchanges_whole_bytes() {
        let (mut swim, notes) = swim_with_chain();
        let byte_ticks = swim.byte_ticks();
        assert_eq!(byte_ticks, 261);
        // As Mac OS sets the mode: no motor timer
        swim.read(Q6H);
        swim.write(Q7H, 0x1F);
        swim.read(Q7L);
        swim.read(Q6L);
        swim.read(EXTDRIVEH);
        swim.read(ENABLE);

        // The byte the selected device sends is in the data register at the next tick
        assert_eq!(swim.read(Q6L), Some(0));
        swim.tick(1, ()).unwrap();
        assert_eq!(swim.read(Q6L), Some(0xC5));
        swim.tick(1, ()).unwrap();
        assert_eq!(swim.read(Q6L), Some(0));

        // A byte the guest writes reaches the selected device, here the second, after the
        // time of eight bit cells, and the write buffer is free meanwhile
        swim.read(LSTRBH);
        for notes in &notes {
            take_notes(notes);
        }
        swim.read(Q6H);
        swim.write(Q7H, 0xAA);
        swim.tick(1, ()).unwrap();
        assert_eq!(swim.read(Q6L).unwrap() & 0x80, 0x80);
        swim.tick(byte_ticks - 1, ()).unwrap();
        assert!(take_notes(&notes[1]).is_empty());
        swim.tick(1, ()).unwrap();
        assert_eq!(take_notes(&notes[1]), ["byte AA"]);
        assert!(take_notes(&notes[2]).is_empty());
    }

    #[test]
    fn the_chain_stays_through_a_reset_and_a_saved_state() {
        let (mut swim, notes) = swim_with_chain();
        swim.read(EXTDRIVEH);
        swim.read(ENABLE);
        swim.read(LSTRBH);
        for notes in &notes {
            take_notes(notes);
        }

        // Every device is told of a reset, in the middle of an exchange, after which the
        // first of the chain answers again
        swim.reset();
        for notes in &notes {
            assert_eq!(take_notes(notes), ["reset"]);
        }
        swim.read(EXTDRIVEH);
        swim.read(ENABLE);
        swim.read(CA0H);
        assert_eq!(take_notes(&notes[0]), ["lines 0000", "lines 0010"]);

        let state = serde_json::to_string(&swim).unwrap();
        let mut loaded: Swim = serde_json::from_str(&state).unwrap();
        assert!(loaded.chain.device_at(0).is_some());
        assert!(loaded.chain.device_at(1).is_some());
        assert!(loaded.chain.device_at(2).is_none());
        assert!(loaded.selected() == Place::Chain(0));
    }

    #[test]
    fn devices_get_the_lines_the_strobe_and_the_time() {
        let mut swim = Swim::new(false, 16_000_000);
        let probes = [Probe::default(), Probe::default()];
        let notes = probes.each_ref().map(|probe| probe.notes.clone());
        for (slot, probe) in Swim::SLOT_IDS.iter().zip(probes) {
            swim.attach(slot, MadeDevice::Swim(Box::new(probe)))
                .unwrap();
        }
        swim.read(ENABLE);

        // A line changes: the selected device is told, once
        swim.read(CA2H);
        swim.read(CA2H);
        // The machine changes SEL: the controller sees it at its next tick
        swim.sel = true;
        swim.tick(5, ()).unwrap();
        swim.read(LSTRBH);
        // The status register has the sense line of the selected device in bit 7
        assert_eq!(swim.read(Q6H).unwrap() & 0x80, 0x80);
        swim.read(CA0H);
        assert_eq!(swim.read(Q6H).unwrap() & 0x80, 0x00);
        // The other device is selected, and learns the lines
        swim.read(EXTDRIVEH);

        assert_eq!(
            *notes[0].lock().unwrap(),
            [
                "connected 0 16000000",
                "lines 1000",
                "tick 5",
                "lines 1001",
                "strobe 1001",
                "lines 1011"
            ]
        );
        assert_eq!(
            *notes[1].lock().unwrap(),
            ["connected 1 16000000", "tick 5", "lines 1011"]
        );
    }

    #[test]
    fn connector_without_a_device_reads_high() {
        let mut swim = Swim::new(false, 16_000_000).with_drives(&[DriveType::GCR800K]);
        swim.read(ENABLE);
        swim.read(EXTDRIVEH);
        swim.read(Q6H);
        for lines in [CA0H, CA1H, CA2H] {
            assert_eq!(swim.read(lines).unwrap() & 0x80, 0x80);
        }
        swim.sel = true;
        assert_eq!(swim.read(Q6H).unwrap() & 0x80, 0x80);
        // A strobe goes nowhere
        swim.read(LSTRBH);
        swim.tick(100, ()).unwrap();
    }

    #[test]
    fn drives_come_back_from_a_saved_state() {
        let mut swim = swim_with(
            DriveType::GCR800K,
            FloppyImage::new(FloppyType::Mac800K, "saved"),
        );
        swim.tick(1000, ()).unwrap();

        let state = serde_json::to_string(&swim).unwrap();
        let mut loaded: Swim = serde_json::from_str(&state).unwrap();
        assert!(loaded.drive_at(1).is_none());
        let FunctionMut::Floppy(floppy) = loaded.drive_at(0).unwrap().function() else {
            unreachable!()
        };
        assert_eq!(floppy.status().title, "saved");
        // It turns as the one that was saved does
        assert!(loaded.drive(0).stream().is_some());
        assert_eq!(
            loaded.drive(0).get_ticks_per_bit(),
            swim.drive(0).get_ticks_per_bit()
        );
    }
}
