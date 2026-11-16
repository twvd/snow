//! Sander-Wozniak Integrated Machine
//!
//! Floppy drive controller consisting of two different controllers:
//! Integrated Wozniak Machine, Integrated Sander Machine.

pub mod drive;
pub mod ism;
pub mod iwm;
pub mod stream;

use std::cell::Cell;
use std::collections::VecDeque;

use anyhow::Result;
use ism::{IsmError, IsmSetup, IsmStatus};
use rand::Rng;

use drive::{DriveType, FloppyDrive};
use iwm::{IwmMode, IwmStatus};
use serde::{Deserialize, Serialize};
use snow_floppy::flux::FluxTicks;

use crate::bus::native_clock::{C16M_FREQUENCY, NativeClock};
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

    pub(crate) drives: [FloppyDrive; 3],

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

    pub fn new_swim2(drives: &[DriveType], base_frequency: Ticks) -> Self {
        let mut swim = Self::new(drives, true, base_frequency);
        swim.iwm_available = false;
        swim.mode = SwimMode::Ism;
        swim.chip.ism_mode.set_ism(true);
        swim.chip.ism_params = vec![0; 4];
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

    fn get_selected_drive(&self) -> &FloppyDrive {
        &self.drives[self.get_selected_drive_idx()]
    }

    fn get_selected_drive_mut(&mut self) -> &mut FloppyDrive {
        &mut self.drives[self.get_selected_drive_idx()]
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

        let selected = self.get_selected_drive_idx();
        let stream = self.drives[selected].stream();
        if stream.spinning {
            // Advance read/write operation
            let head = stream.head(self.sel);
            match self.mode {
                SwimMode::Iwm => self.chip.iwm_tick(stream, head, ticks, self.cycles)?,
                SwimMode::Ism => self.chip.ism_tick(stream, head, ticks)?,
            }
        } else if self.mode == SwimMode::Ism {
            let byte_ticks = self.drives[selected].base_frequency * 16 / 1_000_000;
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
            dbgprop_header, dbgprop_nest, dbgprop_string, dbgprop_udec, dbgprop_word,
            dbgprop_word_bin,
        };

        vec![
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

#[cfg(test)]
mod tests {
    use snow_floppy::loaders::{FloppyImageLoader, Moof};
    use snow_floppy::{Floppy, FloppyImage, FloppyType, TrackLength};

    use super::*;

    const ENABLE: Address = 9 << 9;
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
        let mut swim = Swim::new(&[drive], true, 16_000_000);
        swim.drives[0].disk_insert(image).unwrap();
        swim.drives[0].write_drive_reg(DRIVE_MOTOR_ON);
        swim.drives[0].not_ready = 0;
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
        let turn = swim.drives[0].get_ticks_per_bit() * len as Ticks;

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
        let mut swim = Swim::new(&[DriveType::GCR800K], true, 16_000_000);
        swim.drives[0]
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
        swim.drives[0].write_drive_reg(DRIVE_MOTOR_ON);
        swim.drives[0].not_ready = 0;
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
        let turn = swim.drives[0].get_ticks_per_bit() * len as Ticks;

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
}
