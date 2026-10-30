//! Sander-Wozniak Integrated Machine
//!
//! Floppy drive controller consisting of two different controllers:
//! Integrated Wozniak Machine, Integrated Sander Machine.

pub mod drive;
pub mod ism;
pub mod iwm;

use std::cell::Cell;
use std::collections::VecDeque;

use anyhow::{Result, bail};
use ism::{IsmError, IsmSetup, IsmStatus};
use rand::Rng;

use drive::{DriveType, FloppyDrive};
use iwm::{IwmMode, IwmStatus};
use serde::{Deserialize, Serialize};
use snow_floppy::flux::FluxTicks;
use snow_floppy::{Floppy, FloppyImage};

use crate::bus::native_clock::NativeClock;
use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
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

/// Sander-Wozniak Integrated Machine - floppy drive controller
#[derive(Serialize, Deserialize)]
pub struct Swim {
    ism_available: bool,
    /// False on the SWIM2, which only has the ISM register set
    iwm_available: bool,

    cycles: Ticks,
    bit_cycles: Ticks,
    mode: SwimMode,

    /// Native time of the last access by an overclocked CPU, after its hold
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

    iwm_status: IwmStatus,
    iwm_mode: IwmMode,
    shdata: u8,
    datareg: u8,
    iwm_zeroes: usize,
    weak_bit_rng: WeakBitRng,
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

    pub(crate) drives: [FloppyDrive; 3],

    pub dbg_pc: u32,
    pub dbg_break: LatchingEvent,
}

impl Swim {
    /// The controller is ticked at `base_frequency`, which is its FCLK
    pub fn new(drives: &[DriveType], ism_available: bool, base_frequency: Ticks) -> Self {
        Self {
            drives: core::array::from_fn(|i| {
                FloppyDrive::new(
                    i,
                    *drives.get(i).unwrap_or(&DriveType::None),
                    base_frequency,
                )
            }),
            ism_available,
            iwm_available: true,

            cycles: 0,
            bit_cycles: 0,
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

            shdata: 0,
            datareg: 0,
            iwm_zeroes: 0,
            weak_bit_rng: WeakBitRng::default(),
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
            ism_crc: Self::ISM_CRC_INIT,
            ism_write_shreg: 0,
            ism_write_shreg_cnt: 0,
            ism_write_prev_bit: false,

            enable: false,
            enable_hold: 0,
            dbg_pc: 0,
            dbg_break: LatchingEvent::default(),
        }
    }

    /// Seeds the random number generators used for floppy noise (weak bits).
    /// `None` reverts to non-deterministic randomness.
    pub fn seed_weak_bits(&mut self, seed: Option<u64>) {
        self.weak_bit_rng.seed(seed);
        for (i, drive) in self.drives.iter().enumerate() {
            drive
                .weak_bit_rng
                .seed(seed.map(|s| s.wrapping_add(i as u64 + 1)));
        }
    }

    pub fn new_swim2(drives: &[DriveType], base_frequency: Ticks) -> Self {
        let mut swim = Self::new(drives, true, base_frequency);
        swim.iwm_available = false;
        swim.mode = SwimMode::Ism;
        swim.ism_mode.set_ism(true);
        swim.ism_params = vec![0; 4];
        swim
    }

    /// Resets the controller, as the system RESET line does: back to the
    /// power-on mode with all drive control lines low. The drives keep their
    /// disks.
    pub fn reset(&mut self) {
        let base_frequency = self.drives[0].base_frequency;
        let mut fresh = if self.iwm_available {
            Self::new(&[], self.ism_available, base_frequency)
        } else {
            Self::new_swim2(&[], base_frequency)
        };
        std::mem::swap(&mut fresh.drives, &mut self.drives);
        fresh.cycles = self.cycles;
        fresh.last_access = self.last_access;
        fresh.sel = self.sel;
        fresh.intdrive = self.intdrive;
        fresh.weak_bit_rng = std::mem::take(&mut self.weak_bit_rng);
        fresh.dbg_pc = self.dbg_pc;
        fresh.dbg_break = std::mem::take(&mut self.dbg_break);
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
        let now = clock.native_ticks();
        let interval =
            (clock.native_frequency() * OVERCLOCK_ACCESS_INTERVAL_NS).div_ceil(1_000_000_000);
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
            if self.ism_mode.drive2_enable() {
                1
            } else if self.ism_mode.drive1_enable() {
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
        match (self.ism_mode.ism(), self.ism_setup.fclk_div2()) {
            (false, _) => 0,
            (true, false) => 4,
            (true, true) => 8,
        }
    }

    pub fn is_writing(&self) -> bool {
        self.write_buffer.is_some()
    }

    fn get_selected_drive(&self) -> &FloppyDrive {
        &self.drives[self.get_selected_drive_idx()]
    }

    fn get_selected_drive_mut(&mut self) -> &mut FloppyDrive {
        &mut self.drives[self.get_selected_drive_idx()]
    }

    /// Inserts a disk into the disk drive
    pub fn disk_insert(&mut self, drive: usize, image: FloppyImage) -> Result<()> {
        if !self.drives[drive].is_present() {
            bail!("Drive {} not present", drive);
        }

        self.drives[drive].disk_insert(image)
    }

    /// Gets the active (selected) drive head
    fn get_active_head(&self) -> usize {
        if !self.get_selected_drive().drive_type.is_doublesided()
            || self.get_selected_drive().floppy.get_side_count() == 1
            || !self.sel
        {
            0
        } else {
            1
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

    pub fn get_active_image(&self, drive: usize) -> &FloppyImage {
        &self.drives[drive].floppy
    }
}

impl BusMember<Address> for Swim {
    fn read(&mut self, addr: Address) -> Option<u8> {
        match self.mode {
            SwimMode::Iwm => self.iwm_read(addr),
            SwimMode::Ism => self.ism_read(addr),
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        match self.mode {
            SwimMode::Iwm => self.iwm_write(addr, val),
            SwimMode::Ism => self.ism_write(addr, val),
        }
        Some(())
    }
}

impl Tickable for Swim {
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        self.cycles += ticks;
        for drv in &mut self.drives {
            drv.cycles = self.cycles;
        }

        if self.get_selected_drive().is_running() {
            // Advance read/write operation
            match self.mode {
                SwimMode::Iwm => self.iwm_tick(ticks)?,
                SwimMode::Ism => self.ism_tick(ticks)?,
            }
        } else if self.mode == SwimMode::Ism {
            self.ism_tick_no_disk(ticks);
        }

        Ok(ticks)
    }
}

impl Debuggable for Swim {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{
            dbgprop_bool, dbgprop_byte, dbgprop_byte_bin, dbgprop_enum, dbgprop_group,
            dbgprop_header, dbgprop_nest, dbgprop_string, dbgprop_udec, dbgprop_word,
            dbgprop_word_bin,
        };

        vec![
            dbgprop_enum!("Mode", self.mode),
            dbgprop_udec!("ISM switch counter", self.ism_switch_ctr),
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
                    dbgprop_byte!("Status", self.iwm_status.0),
                    dbgprop_byte!("Mode", self.iwm_mode.0),
                    dbgprop_header!("Reading"),
                    dbgprop_byte!("Data register", self.datareg),
                    dbgprop_byte_bin!("Read shifter", self.shdata),
                    dbgprop_udec!("Zeroes", self.iwm_zeroes),
                    dbgprop_header!("Writing"),
                    dbgprop_byte_bin!("Write shifter", self.write_shift),
                    dbgprop_udec!("Write position", self.write_pos),
                    dbgprop_byte!("Write buffer", self.write_buffer.unwrap_or(0)),
                ]
            ),
            dbgprop_group!(
                "ISM",
                vec![
                    dbgprop_header!("Registers"),
                    dbgprop_byte_bin!("Phase mask", self.ism_phase_mask),
                    dbgprop_byte!("Error", self.ism_error.0),
                    dbgprop_byte!("Mode", self.ism_mode.0),
                    dbgprop_byte!("Setup", self.ism_setup.0),
                    dbgprop_header!("Parameters"),
                    dbgprop_udec!("Parameter index", self.ism_param_idx),
                    dbgprop_group!(
                        "Parameters",
                        Vec::from_iter(
                            self.ism_params
                                .iter()
                                .enumerate()
                                .map(|(i, p)| dbgprop_byte!(format!("[{}]", i), *p))
                        )
                    ),
                    dbgprop_header!("Reading/writing"),
                    dbgprop_group!(
                        "FIFO",
                        Vec::from_iter(self.ism_fifo.iter().enumerate().map(
                            |(i, p)| dbgprop_string!(
                                format!("[{}]", i),
                                format!("{} {:08b} (${:02X})", p, p.inner(), p.inner())
                            )
                        ))
                    ),
                    dbgprop_word_bin!("Shifter", self.ism_shreg),
                    dbgprop_udec!("Shifter bits", self.ism_shreg_cnt),
                    dbgprop_bool!("Synchronized", self.ism_synced),
                    dbgprop_word!("CRC", self.ism_crc),
                ]
            ),
            dbgprop_nest!(
                format!("Drive #1 ({})", self.drives[0].drive_type),
                self.drives[0]
            ),
            dbgprop_nest!(
                format!("Drive #2 ({})", self.drives[1].drive_type),
                self.drives[1]
            ),
            dbgprop_nest!(
                format!("Drive #3 ({})", self.drives[2].drive_type),
                self.drives[2]
            ),
        ]
    }
}
