use std::path::PathBuf;

use anyhow::Result;
use log::*;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use serde::{Deserialize, Serialize};
use snow_floppy::flux::FluxTicks;
use snow_floppy::{Floppy, FloppyImage, FloppyType, TrackLength, TrackType};
use strum::Display;

use super::WeakBitRng;
use crate::debuggable::Debuggable;
use crate::tickable::Ticks;
use crate::{dbgprop_bool, dbgprop_enum, dbgprop_sdec, dbgprop_udec};

/// Floppy drive types
///
/// Identification:
/// PRESENT, !READY, MFM, RDDATA1
///    0000 - 400K GCR drive
///    0001 - 4MB Typhoon drive
///    x011 - Superdrive (x depends on the HD hole of the inserted disk, if any)
///    1010 - 800K GCR drive
///    1110 - HD-20 drive
///    1111 - No drive (pull-up on the sense line)
#[derive(
    Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Debug, Display, strum::IntoStaticStr,
)]
pub enum DriveType {
    None,
    GCR400K,
    GCR800K,
    /// PWM-controlled 800K drive for use in the 128K/512K
    GCR800KPWM,
    SuperDrive,
    /// Apple FileWare ("Twiggy") 5.25" drive, of the Twiggy Mac. It doesn't have the Sony
    /// drives' registers: the IWM's phase lines drive its stepper, see `twiggy_phases`.
    Twiggy,
}

impl DriveType {
    pub const fn io_superdrive(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM | Self::Twiggy => false,
            Self::SuperDrive => true,
        }
    }

    pub const fn io_present(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K | Self::Twiggy => false,
            Self::GCR800K | Self::GCR800KPWM => true,
            Self::SuperDrive => unreachable!(),
        }
    }

    pub const fn io_mfm(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM | Self::Twiggy => false,
            Self::SuperDrive => true,
        }
    }

    pub const fn io_rddata1(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM | Self::Twiggy => false,
            Self::SuperDrive => true,
        }
    }

    pub const fn io_ready(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM | Self::SuperDrive | Self::Twiggy => {
                false
            }
        }
    }

    pub const fn is_doublesided(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K => false,
            Self::GCR800K | Self::GCR800KPWM | Self::SuperDrive | Self::Twiggy => true,
        }
    }

    pub const fn has_pwm_control(self) -> bool {
        match self {
            Self::None => false,
            Self::GCR400K => true,
            Self::GCR800KPWM => true,
            Self::GCR800K | Self::SuperDrive => false,
            Self::Twiggy => true,
        }
    }

    pub const fn compatible_floppies(self) -> &'static [FloppyType] {
        match self {
            Self::None => &[],
            Self::GCR400K => &[FloppyType::Mac400K],
            Self::GCR800K => &[FloppyType::Mac400K, FloppyType::Mac800K],
            Self::GCR800KPWM => &[FloppyType::Mac400K, FloppyType::Mac800K],
            Self::SuperDrive => &[
                FloppyType::Mac400K,
                FloppyType::Mac800K,
                FloppyType::Mfm144M,
            ],
            Self::Twiggy => &[FloppyType::Twiggy],
        }
    }
}

/// The Twiggy drive, as the Twiggy Mac's disk driver (Twiggy.ROM, `.Disk` at $16E4) uses
/// it. No documentation of the drive is known.
///
/// Its actuator moves by half steps of its stepper: two phases on hold it on a step, one on
/// half way to the next. The driver counts 8 steps a track and ends every move holding one
/// phase ($401CA2), half a step past its count. The heads face each other: side 0's is
/// over track (count - 8) / 8, side 1's over (368 - count) / 8 (the driver's seek,
/// $401CE0). It calibrates by stepping down 4 at a time until the track 0 sensor (CAL)
/// goes low, and takes that as count 368 ($401BEE). Moving well past track 0 of side 0,
/// where side 1's head is past its last track, ejects the disk (the driver's eject,
/// $401B5E, and ESFloppy, which emulates the drive for a Lisa 1).
///
/// Positions here are in half steps: twice the driver's count, plus 1 at rest.
mod twiggy {
    pub const HALF_STEPS_PER_TRACK: i32 = 16;
    pub const SIDE_0_TRACK_0: i32 = 2 * 8 + 1;
    pub const SIDE_1_TRACK_0: i32 = 2 * 368 + 1;
    /// CAL is high from here: between counts 368 and 372 at rest
    pub const CAL: i32 = 2 * 370 + 1;
    pub const EJECT: i32 = 2 * -52;
    /// Where the actuator starts: on a step, above CAL
    pub const PARKED: i32 = 2 * 376;
    /// The speed follows the PWM from the sound buffer, as on the 400K drive, but the
    /// driver's speed values (0 to 399, $4021A0) are 399 minus the PWM index the IWM code
    /// decodes. It measures the tachometer at two values, then sets for each zone the value
    /// that gives its speed ($40231A). Its speeds go from about 216 rpm (zone 0) to 317 rpm
    /// (zone 7): only the sign of the slope and the range matter.
    pub const RPM_AT_SPEED_0: u64 = 150;
    pub const RPM_AT_SPEED_399: u64 = 400;
    /// Tachometer pulses a turn. The driver's speeds for the zones are tachometer periods
    /// ($402304: 4.21 ms for zone 0), and it checks that a turn of a zone 0 track takes
    /// 0.279 s ($402488): about 66 pulses a turn.
    pub const TACH_PULSES: u64 = 66;

    /// Where the phases on hold the stepper, in half steps modulo 8. Stepping up goes
    /// through the pairs in this order ($401C66: $5A, $69, $A5, $96), and a phase alone
    /// holds it between the two pairs it is in ($401CA2: $6A, $A9, $A6, $9A).
    pub fn hold(phases: [bool; 4]) -> Option<i32> {
        match phases {
            [true, true, false, false] => Some(0),
            [true, false, false, false] => Some(1),
            [true, false, false, true] => Some(2),
            [false, false, false, true] => Some(3),
            [false, false, true, true] => Some(4),
            [false, false, true, false] => Some(5),
            [false, true, true, false] => Some(6),
            [false, true, false, false] => Some(7),
            _ => None,
        }
    }
}

/// Direction the drive head is set to step to
#[derive(
    PartialEq, Eq, Clone, Copy, Debug, Display, strum::IntoStaticStr, Serialize, Deserialize,
)]
enum HeadStepDirection {
    Up,
    Down,
}

/// Drive registers
/// Value bits: CA2 CA1 CA0 SEL
#[allow(clippy::upper_case_acronyms)]
#[derive(FromPrimitive, Debug, PartialEq, Eq)]
enum DriveReg {
    /// Head step direction
    /// 0 = track++, 1 = track--
    DIRTN = 0b0000,
    /// Disk in place
    /// 0 = disk in drive, 1 = no disk
    CISTN = 0b0001,
    /// Disk head stepping
    /// 0 = head stepping, 1 = head not stepping
    STEP = 0b0010,
    /// Disk write protect
    /// 0 = disk write protected, 1 = not write protected
    WRTPRT = 0b0011,
    /// Disk switched (?)
    /// 0 = not switched, 1 = switched
    SWITCHED = 0b0110,
    /// Disk motor running
    /// 0 = running, 1 = off
    MOTORON = 0b0100,
    /// Head at track 0
    /// 0 = track 0, 1 = other track
    TKO = 0b0101,
    /// Tachometer
    /// 60 pulses/revolution
    TACH = 0b0111,
    /// Read data, low head (GCR)
    /// Index pulse (MFM), active low
    RDDATA0 = 0b1000,
    /// Read data, upper head (GCR)
    /// Index pulse (MFM), active low
    RDDATA1 = 0b1001,
    /// SuperDrive
    SUPERDRIVE = 0b1010,
    /// MFM mode
    MFM = 0b1011,
    /// Single/double sided drive
    /// 0 = single, 1 = double
    SIDES = 0b1100,
    /// Disk ready (?)
    /// 0 = ready, 1 = not ready
    READY = 0b1101,

    /// Drive installed
    /// 0 = installed, 1 = not installed
    INSTALLED = 0b1110,

    /// REVISED/PRESENT/!HD
    PRESENTHD = 0b1111,

    /// For unknown values
    UNKNOWN,
}

/// Drive write registers
/// Value bits: CA2 CA1 CA0 SEL
#[allow(clippy::upper_case_acronyms)]
#[derive(FromPrimitive, Debug)]
enum DriveWriteReg {
    /// Step to higher track (track++)
    TRACKUP = 0b0000,

    /// Step to lower track (track--)
    TRACKDN = 0b1000,

    /// Step in current direction
    TRACKSTEP = 0b0010,

    /// Switch to MFM mode
    MFMMODE = 0b0011,

    /// Drive motor on
    MOTORON = 0b0100,

    /// Clear SWITCHED
    CLRSWITCHED = 0b1001,

    /// Switch to GCR mode
    GCRMODE = 0b1011,

    /// Drive motor off
    MOTOROFF = 0b1100,

    /// Eject disk
    EJECT = 0b1110,

    /// For unknown values
    UNKNOWN,
}

/// A single disk drive, attached to the drive controller
#[derive(Serialize, Deserialize)]
pub(crate) struct FloppyDrive {
    idx: usize,
    pub(super) base_frequency: Ticks,
    pub(crate) drive_type: DriveType,
    pub(super) cycles: Ticks,

    pub(crate) floppy_inserted: bool,
    pub(crate) track: usize,
    stepdir: HeadStepDirection,
    pub(crate) motor: bool,
    pub(crate) floppy: FloppyImage,

    /// Copy of last ejected floppy image
    pub(crate) floppy_ejected: Option<Box<FloppyImage>>,
    pub(super) track_position: usize,

    /// True if disk was switched without eject
    switched: bool,

    /// In MFM mode (in GCR mode when false)
    mfm: bool,

    // While > cycles, the drive head is moving
    pub(super) stepping: Ticks,
    // While > cycles, the drive is not ready
    // (e.g. stepping, spindle motor changing speed)
    pub(super) not_ready: Ticks,

    /// Amount of flux ticks for current transition (for flux tracks)
    pub(super) flux_ticks: FluxTicks,

    /// Amount of flux ticks left for current transition (for flux tracks)
    pub(super) flux_ticks_left: FluxTicks,

    pub(super) head_bit: [bool; 2],

    pub(super) pwm_avg_sum: i64,
    pub(super) pwm_avg_count: usize,
    pub(super) pwm_dutycycle: Ticks,

    /// RPM adjustment for PWM-controlled drives
    pub(crate) rpm_adjustment: i32,

    /// Cached ticks-per-bit
    ticks_per_bit: Ticks,

    /// Noise source for garbage read while the drive is not ready
    pub(super) weak_bit_rng: WeakBitRng,

    /// Twiggy: where the actuator is, in eighths of a track
    twiggy_position: i32,
    /// Twiggy: the head the IWM reads, whose track sets the bit rate
    twiggy_head: usize,
    /// The speed the PWM asks for, from 0 to 399
    pub(super) pwm_index: Ticks,

    /// Source file path of the currently inserted image, if it was loaded
    /// from a writeback-capable file. Cleared on eject.
    #[serde(skip)]
    pub(crate) image_path: Option<PathBuf>,

    /// The file the inserted image came from, if any: what a workspace remembers
    #[serde(skip)]
    pub(crate) media_path: Option<PathBuf>,

    /// The inserted image was asked to be write-protected
    #[serde(skip)]
    pub(crate) media_read_only: bool,

    /// Per-drive writeback opt-in. Only meaningful when [`Self::image_path`]
    /// is set and `floppy.supports_writeback()`.
    #[serde(skip)]
    pub(crate) writeback_enabled: bool,

    /// Set by [`DriveWriteReg::MOTOROFF`] when a writeback save is due. The
    /// emulator polls this and clears it after attempting the save.
    #[serde(skip)]
    pub(crate) pending_writeback: bool,

    /// Path to save [`Self::floppy_ejected`] to: set when a disk with unsaved
    /// writes and writeback armed is ejected
    #[serde(skip)]
    pub(crate) eject_writeback: Option<PathBuf>,
}

impl FloppyDrive {
    /// Amount of tracks per disk side
    const DISK_TRACKS: usize = 80;

    /// Tacho pulses/disk revolution
    const TACHO_SPEED: Ticks = 60;

    pub fn new(idx: usize, drive_type: DriveType, base_frequency: Ticks) -> Self {
        Self {
            idx,
            base_frequency,
            drive_type,
            cycles: 0,
            floppy_inserted: false,
            floppy_ejected: None,
            track: 4,
            stepdir: HeadStepDirection::Up,
            floppy: FloppyImage::new(FloppyType::Mac400K, ""),
            track_position: 0,
            motor: false,
            mfm: drive_type.io_mfm(),
            switched: false,

            stepping: 0,
            not_ready: 0,

            flux_ticks: 0,
            flux_ticks_left: 0,

            head_bit: [false; 2],

            pwm_avg_sum: 0,
            pwm_avg_count: 0,
            pwm_dutycycle: 0,
            rpm_adjustment: 0,
            ticks_per_bit: Ticks::MAX,
            weak_bit_rng: WeakBitRng::default(),
            twiggy_position: twiggy::PARKED,
            twiggy_head: 0,
            pwm_index: 0,

            image_path: None,
            media_path: None,
            media_read_only: false,
            writeback_enabled: false,
            pending_writeback: false,
            eject_writeback: None,
        }
    }

    /// Returns true if drive is present
    pub fn is_present(&self) -> bool {
        self.drive_type != DriveType::None
    }

    /// Returns true if drive's spindle motor is running
    pub(super) fn is_running(&self) -> bool {
        self.floppy_inserted && self.motor
    }

    /// Returns true if disk is at index (MFM only)
    #[inline(always)]
    pub(super) fn at_index(&self) -> bool {
        debug_assert!(self.mfm);

        // Disk spins at 300rpm, 200000 bits, index pulse ~2ms
        // 300 / 60 = 5 revs/sec * 200000 bits = 1.000.000 bits/sec
        // = ~2000 bits pulse length
        self.is_running() && (0..2000).contains(&self.track_position)
    }

    /// Reads from the currently selected drive register
    // Silence clippy so we can write easier to understand expressions for the
    // inverse signals.
    #[allow(clippy::nonminimal_bool)]
    pub(super) fn read_sense(&self, regraw: u8) -> bool {
        if self.drive_type == DriveType::Twiggy {
            return self.twiggy_sense(regraw);
        }
        let reg = DriveReg::from_u8(regraw).unwrap_or(DriveReg::UNKNOWN);

        let result = match reg {
            DriveReg::CISTN => !self.floppy_inserted,
            DriveReg::DIRTN => self.stepdir == HeadStepDirection::Down,
            DriveReg::SIDES => self.drive_type.is_doublesided(),
            DriveReg::MOTORON => !(self.motor && self.floppy_inserted),
            DriveReg::PRESENTHD => {
                if self.drive_type == DriveType::SuperDrive {
                    !(self.floppy.get_type() == FloppyType::Mfm144M && self.floppy_inserted)
                } else {
                    self.drive_type.io_present()
                }
            }
            DriveReg::INSTALLED => !self.is_present(),
            DriveReg::READY if !self.motor => self.drive_type.io_ready(),
            DriveReg::READY => !(self.not_ready < self.cycles),
            DriveReg::TKO => self.track != 0,
            DriveReg::STEP => !self.motor || self.stepping < self.cycles,
            DriveReg::TACH => self.get_tacho(),
            DriveReg::RDDATA0 | DriveReg::RDDATA1 if self.mfm && self.motor => !self.at_index(),
            DriveReg::RDDATA0 => self.get_head_bit(0),
            DriveReg::RDDATA1 if self.motor => self.get_head_bit(1),
            DriveReg::RDDATA1 => self.drive_type.io_rddata1(),
            DriveReg::MFM => self.mfm,
            DriveReg::SUPERDRIVE => self.drive_type.io_superdrive(),
            DriveReg::WRTPRT => !self.floppy.get_write_protect(),
            DriveReg::SWITCHED => self.switched,
            _ => {
                warn!(
                    "Drive {}: unimplemented register read {:?} {:0b}",
                    self.idx, reg, regraw
                );
                true
            }
        };

        if reg != DriveReg::CISTN {
            //debug!("Drive {} reg read {:?} = {}", self.idx, reg, result);
        }

        result
    }

    /// The Twiggy's sense line. What it says depends on PH0, PH2 and SEL, as the Twiggy
    /// Mac's driver reads it: disk in place ($401B9C, $4022AE), CAL ($401BA4), write protect
    /// ($4022AE, then $4018D4) and the tachometer ($402398). ESFloppy has write protect with
    /// PH0 and SEL low too, as the Lisa 1 reads it. The rest reads low.
    fn twiggy_sense(&self, regraw: u8) -> bool {
        let sel = regraw & 0b0001 != 0;
        let ph0 = regraw & 0b0010 != 0;
        let ph2 = regraw & 0b1000 != 0;
        match (ph0, ph2, sel) {
            (true, false, true) => self.floppy_inserted,
            (true, false, false) => self.twiggy_position >= twiggy::CAL,
            (true, true, true) | (false, false, false) => {
                self.floppy_inserted && self.floppy.get_write_protect()
            }
            (false, true, _) => self.get_tacho(),
            _ => false,
        }
    }

    /// The IWM's phase lines, PH0 to PH3, on a Twiggy: the stepper goes to the nearest
    /// place the phases on hold it. None, or three, or two opposite ones don't move it.
    pub(super) fn twiggy_phases(&mut self, phases: [bool; 4]) {
        let Some(hold) = twiggy::hold(phases) else {
            return;
        };
        let before = self.twiggy_position;
        match (hold - before).rem_euclid(8) {
            delta @ 1..=3 => self.twiggy_position += delta,
            delta @ 5..=7 => self.twiggy_position -= 8 - delta,
            _ => return,
        }
        self.update_ticks_per_bit();
        if self.floppy_inserted && before >= twiggy::EJECT && self.twiggy_position < twiggy::EJECT {
            self.eject();
        }
    }

    /// The Twiggy's motor runs while the IWM enables it
    pub(super) fn set_twiggy_motor(&mut self, on: bool) {
        self.motor = on;
    }

    /// The head the IWM reads from: on a Twiggy, the bit rate is that of its track
    pub(super) fn set_twiggy_head(&mut self, head: usize) {
        if self.twiggy_head != head {
            self.twiggy_head = head;
            self.update_ticks_per_bit();
        }
    }

    /// The track under a head: on a Twiggy, the nearest one to its head
    pub(super) fn get_head_track(&self, head: usize) -> usize {
        if self.drive_type != DriveType::Twiggy {
            return self.track;
        }
        let from_track_0 = if head == 0 {
            self.twiggy_position - twiggy::SIDE_0_TRACK_0
        } else {
            twiggy::SIDE_1_TRACK_0 - self.twiggy_position
        };
        let last = snow_floppy::twiggy::TRACKS as i32 - 1;
        let track = from_track_0 + twiggy::HALF_STEPS_PER_TRACK / 2;
        track
            .div_euclid(twiggy::HALF_STEPS_PER_TRACK)
            .clamp(0, last) as usize
    }

    /// Moves the drive head one step in the selected position
    fn step_head(&mut self) {
        let previous_rpm = self.get_track_rpm();

        if self.stepping > self.cycles {
            log::warn!("Head stepping before /STEP released");
        }

        match self.stepdir {
            HeadStepDirection::Up => {
                if (self.track + 1) >= Self::DISK_TRACKS {
                    error!("Drive {}: head moving further than track 79!", self.idx);
                } else {
                    self.track += 1;
                }
            }
            HeadStepDirection::Down => {
                if self.track == 0 {
                    // The Sony driver recalibrates some drives with 80 steps,
                    // without looking at the track 0 signal
                    trace!("Drive {}: head moving lower than track 0", self.idx);
                } else {
                    self.track -= 1;
                }
            }
        }

        self.flux_ticks = 0;
        self.flux_ticks_left = 0;

        // * The Mac ROM runs a 434 cycle wait loop with a limit of 80,
        //   meaning we have 34720 cycles, around 4.34ms (on Mac Plus).
        // * The timing diagram in the 400K drive datasheet 3.4.4 shows /STEP
        //   stays low for 12 us to 12 ms max.
        // * The author of Applesauce shared stepping takes roughly 3ms in
        //   practice, but /READY remains low until drive speed has settled.
        //
        // 400K drive datasheet 2.3.3 also mentions 'Track-to-track step settling
        // time' of max 30ms. Assuming that's the max time for the drive to release
        // /READY.
        //
        // Mac II has issues if this is too long. 0.5ms should do..
        self.stepping = self.cycles + (self.base_frequency / 10000 * 5);

        if self.get_track_rpm() != previous_rpm {
            // Changed speed group
            // 400K drive datasheet 2.3.3 'Speed group to speed group motor settling
            // time' is 150ms.
            // In practice, it can be a bit more.
            self.not_ready = std::cmp::max(
                self.not_ready,
                self.cycles + (self.base_frequency / 1000 * 200),
            );
        } else {
            // Same speed group
            // Wait for 'Track-to-track step settling time'.
            self.not_ready = std::cmp::max(
                self.not_ready,
                self.cycles + (self.base_frequency / 1000 * 30),
            );
        }
    }

    /// Writes to the currently selected drive register
    pub(super) fn write_drive_reg(&mut self, regraw: u8) {
        let reg = DriveWriteReg::from_u8(regraw).unwrap_or(DriveWriteReg::UNKNOWN);

        match reg {
            DriveWriteReg::MOTORON => {
                self.track_position = 0;
                self.motor = true;

                // 400K drive datasheet 2.3.3 'Motor start time' is 400ms.
                self.not_ready = self.cycles + (self.base_frequency / 1000 * 400);
            }
            DriveWriteReg::MOTOROFF => {
                self.motor = false;
                if self.writeback_enabled && self.image_path.is_some() && self.floppy.is_dirty() {
                    self.pending_writeback = true;
                }
            }
            DriveWriteReg::EJECT => {
                // "At the rising edge of the LSTRB, EJECT is set to high and
                // the ejection operation starts", and "The eject operation
                // will continue to completion even if /ENBL goes high" (800K
                // drive ERS 669-0452-A, 3.2.4.4 and 2.17.5). The 400K drive
                // instead needs LSTRB held for 750 ms and drops the eject
                // when disabled (400K drive spec 699-0285-A, 3.2.8): not
                // modelled, the Mac 128K and 512K hold it for 0.75 s anyway.
                if self.floppy_inserted {
                    self.eject();
                }
            }
            DriveWriteReg::TRACKUP => {
                self.stepdir = HeadStepDirection::Up;
            }
            DriveWriteReg::TRACKDN => {
                self.stepdir = HeadStepDirection::Down;
            }
            DriveWriteReg::TRACKSTEP => self.step_head(),
            DriveWriteReg::MFMMODE if self.drive_type == DriveType::SuperDrive => {
                self.mfm = true;
            }
            DriveWriteReg::GCRMODE if self.drive_type == DriveType::SuperDrive => {
                self.mfm = false;
            }
            DriveWriteReg::CLRSWITCHED => {
                self.switched = false;
            }
            _ => {
                warn!("Unimplemented register write {:?} {:0b}", reg, regraw);
            }
        }

        self.update_ticks_per_bit();
    }

    /// Inserts a disk into the disk drive
    pub fn disk_insert(&mut self, image: FloppyImage) -> Result<()> {
        info!(
            "Drive {}: disk inserted, {} tracks, title: '{}'",
            self.idx,
            image.get_track_count() * image.get_side_count(),
            image.get_title()
        );
        self.floppy = image;
        self.floppy_inserted = true;
        self.switched = true;
        self.track_position = 0;
        if self.motor {
            // "the disk motor is turned on if a disk is in the drive" (400K
            // drive spec 699-0285-A, 3.2.7): the new disk spins up
            self.not_ready = self.cycles + (self.base_frequency / 1000 * 400);
        }
        Ok(())
    }

    /// Gets the spindle motor speed in rounds/minute for the currently selected track
    pub fn get_track_rpm(&self) -> Ticks {
        if self.drive_type == DriveType::Twiggy {
            let speed = 399 - self.pwm_index.min(399);
            return twiggy::RPM_AT_SPEED_0
                + speed * (twiggy::RPM_AT_SPEED_399 - twiggy::RPM_AT_SPEED_0) / 399;
        }
        if self.mfm {
            // SuperDrive in MFM mode, fixed speed (CAV)
            // TODO DD spins at 600rpm!
            300
        } else if self.drive_type.has_pwm_control() {
            // Macintosh CLV
            // PWM-driven spindle motor speed control

            // Apple 3.5" single-sided drive specifications
            // 2.17.1.a: Track 0: 9.4% duty cycle: 305 - 380rpm
            const DUTY_T0: Ticks = 9;
            const SPEED_T0: Ticks = (380 + 305) / 2;
            // 2.17.2.b: Track 79: 91% duty cycle: 625 - 780rpm
            const DUTY_T79: Ticks = 91;
            const SPEED_T79: Ticks = (625 + 780) / 2;

            if self.pwm_dutycycle < DUTY_T0 {
                return 0;
            }
            let base_rpm = ((self.pwm_dutycycle - DUTY_T0) * (SPEED_T79 * 100 + SPEED_T0 * 100)
                / (DUTY_T79 - DUTY_T0))
                / 100
                + SPEED_T0;

            // Apply RPM adjustment only for PWM-controlled drives
            (base_rpm as i32 + self.rpm_adjustment).max(0) as Ticks
        } else {
            // Macintosh CLV
            // Automatic spindle motor speed control
            //
            // Apple 3.5" single-sided drive specifications, appendix B, speed 1.
            match self.track {
                0..=15 => 394,
                16..=31 => 429,
                32..=47 => 472,
                48..=63 => 525,
                64..=79 => 590,
                _ => unreachable!(),
            }
        }
    }

    /// Gets the amount of ticks a physical bit is under the drive head
    pub fn get_ticks_per_bit(&self) -> Ticks {
        self.ticks_per_bit
    }

    pub(crate) fn update_ticks_per_bit(&mut self) {
        self.ticks_per_bit = if self.drive_type == DriveType::Twiggy {
            let track = self.get_head_track(self.twiggy_head);
            let bits = snow_floppy::twiggy::track_bits(track) as Ticks;
            (self.base_frequency * 60 / self.get_track_rpm() / bits).max(1)
        } else if self.get_track_rpm() == 0 || !self.floppy_inserted {
            Ticks::MAX
        } else {
            ((self.base_frequency * 60)
                / self.get_track_rpm()
                / self.floppy.get_type().get_approx_track_length(self.track) as u64)
                + 1
        }
    }

    /// Gets the current state of the TACH (spindle motor tachometer) signal
    pub fn get_tacho(&self) -> bool {
        if !self.motor || self.get_track_rpm() == 0 {
            return false;
        }

        // The disk spins at 390-605rpm
        // Each rotation produces 60 tacho pulses (= 120 edges)
        let pulses = if self.drive_type == DriveType::Twiggy {
            twiggy::TACH_PULSES
        } else {
            Self::TACHO_SPEED
        };
        let pulses_per_min = self.get_track_rpm() * pulses;
        let edges_per_min = pulses_per_min * 2;
        let ticks_per_min = self.base_frequency * 60;
        let ticks_per_edge = ticks_per_min / edges_per_min;
        !(self.cycles / ticks_per_edge).is_multiple_of(2)
    }

    /// Gets the length of a track on the loaded floppy
    pub(super) fn get_track_len(&self, side: usize, track: usize) -> TrackLength {
        self.floppy.get_track_length(side, track)
    }

    /// Gets the physical disk bit currently under a head
    fn get_head_bit(&self, head: usize) -> bool {
        let track = self.get_head_track(head);

        // Garbage if drive is not yet ready
        if self.not_ready > self.cycles {
            return self.weak_bit_rng.random_bool();
        }

        match self.floppy.get_track_type(head, track) {
            TrackType::Bitstream => {
                let TrackLength::Bits(tracklen) = self.get_track_len(head, track) else {
                    unreachable!()
                };
                // Extra modulus here because RDDATAx can be read while the controller is currently
                // has the other side selected or we just changed tracks.
                self.floppy
                    .get_track_bit(head, track, self.track_position % tracklen)
            }
            TrackType::Flux => self.head_bit[head],
        }
    }

    /// Advances to the next bit on the track (bitstream tracks)
    pub(super) fn next_bit(&mut self, head: usize) -> bool {
        let TrackLength::Bits(tracklen) = self.get_track_len(head, self.get_head_track(head))
        else {
            unreachable!()
        };
        self.track_position = (self.track_position + 1) % tracklen;

        self.get_head_bit(head)
    }

    /// Writes a bit to the current track position
    pub(super) fn write_bit(&mut self, head: usize, bit: bool) {
        self.floppy
            .set_track_bit(head, self.get_head_track(head), self.track_position, bit);
    }

    /// Ejects the disk
    pub fn eject(&mut self) {
        info!("Drive {}: disk ejected", self.idx);
        self.switched = true;
        self.floppy_inserted = false;
        self.mfm = self.drive_type.io_mfm();

        self.floppy_ejected = Some(Box::new(self.floppy.clone()));

        // The writes since the last save must not be lost with the disk,
        // whoever ejects it
        self.eject_writeback = if self.writeback_enabled && self.floppy.is_dirty() {
            self.image_path.clone()
        } else {
            None
        };
        self.image_path = None;
        self.media_path = None;
        self.media_read_only = false;
        self.writeback_enabled = false;
        self.pending_writeback = false;
    }

    pub(crate) fn take_ejected_image(&mut self) -> Option<Box<FloppyImage>> {
        self.floppy_ejected.take()
    }
}

impl Debuggable for FloppyDrive {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;

        vec![
            dbgprop_enum!("Type", self.drive_type),
            dbgprop_bool!("Disk inserted", self.floppy_inserted),
            dbgprop_bool!("Disk switched", self.switched),
            dbgprop_bool!("MFM mode", self.mfm),
            dbgprop_bool!("Motor on", self.motor),
            dbgprop_enum!("Head step direction", self.stepdir),
            dbgprop_udec!(
                "Head stepping timer",
                self.stepping.saturating_sub(self.cycles)
            ),
            dbgprop_udec!(
                "Not-ready timer",
                self.not_ready.saturating_sub(self.cycles)
            ),
            dbgprop_udec!("Track", self.track),
            dbgprop_sdec!("Twiggy actuator", self.twiggy_position.into()),
            dbgprop_udec!("Track position", self.track_position),
            dbgprop_bool!("Index signal", self.mfm && self.at_index()),
            dbgprop_udec!("Track RPM", self.get_track_rpm()),
            dbgprop_udec!("Ticks per bit", self.get_ticks_per_bit()),
            dbgprop_sdec!("Flux transition len", self.flux_ticks.into()),
            dbgprop_sdec!("Flux transition left", self.flux_ticks_left.into()),
            dbgprop_udec!("PWM dutycycle", self.pwm_dutycycle),
        ]
    }
}

/// The early Macs can be built with a drive other than their own
const fn chosen_with_early_macs(mac: crate::mac::MacModel) -> bool {
    matches!(
        mac,
        crate::mac::MacModel::Early128K | crate::mac::MacModel::Early512K
    )
}

/// The drives the other Macs have, which aren't chosen
const fn never_chosen(_mac: crate::mac::MacModel) -> bool {
    false
}

pub const INFO_400K: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::Mac400KDrive,
    name: "400K drive",
    bus: crate::emulator::devices::BusKind::Swim,
    fits: chosen_with_early_macs,
    function: crate::emulator::devices::FunctionKind::Floppy,
    fixed: true,
    one_per_bus: false,
    rom: None,
    path_keys: &["media"],
    adds_buses: &[],
    monitors: &[],
    create: None,
};

pub const INFO_800K: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::Mac800KDrive,
    name: "800K drive",
    bus: crate::emulator::devices::BusKind::Swim,
    fits: never_chosen,
    function: crate::emulator::devices::FunctionKind::Floppy,
    fixed: true,
    one_per_bus: false,
    rom: None,
    path_keys: &["media"],
    adds_buses: &[],
    monitors: &[],
    create: None,
};

pub const INFO_800K_PWM: crate::emulator::devices::ModelInfo =
    crate::emulator::devices::ModelInfo {
        model: crate::emulator::devices::DeviceModel::Mac800KPwmDrive,
        name: "800K drive (PWM)",
        bus: crate::emulator::devices::BusKind::Swim,
        fits: chosen_with_early_macs,
        function: crate::emulator::devices::FunctionKind::Floppy,
        fixed: true,
        one_per_bus: false,
        rom: None,
        path_keys: &["media"],
        adds_buses: &[],
        monitors: &[],
        create: None,
    };

pub const INFO_TWIGGY: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::MacTwiggyDrive,
    name: "Twiggy drive",
    bus: crate::emulator::devices::BusKind::Swim,
    fits: never_chosen,
    function: crate::emulator::devices::FunctionKind::Floppy,
    fixed: true,
    one_per_bus: false,
    rom: None,
    path_keys: &["media"],
    adds_buses: &[],
    monitors: &[],
    create: None,
};

pub const INFO_SUPERDRIVE: crate::emulator::devices::ModelInfo =
    crate::emulator::devices::ModelInfo {
        model: crate::emulator::devices::DeviceModel::MacSuperDrive,
        name: "SuperDrive",
        bus: crate::emulator::devices::BusKind::Swim,
        fits: never_chosen,
        function: crate::emulator::devices::FunctionKind::Floppy,
        fixed: true,
        one_per_bus: false,
        rom: None,
        path_keys: &["media"],
        adds_buses: &[],
        monitors: &[],
        create: None,
    };

#[cfg(test)]
mod tests {
    use super::*;

    /// Disk revolutions/minute at outer track (0)
    const DISK_RPM_OUTER: Ticks = 394;

    /// Disk revolutions/minute at inner track (79)
    const DISK_RPM_INNER: Ticks = 590;

    const BASE_FREQUENCY: Ticks = 8_000_000;

    #[test]
    fn disk_double_tacho_outer() {
        let mut drv = FloppyDrive::new(0, DriveType::GCR800K, BASE_FREQUENCY);
        drv.floppy_inserted = true;
        drv.motor = true;
        drv.track = 0;

        let mut last = false;
        let mut result = 0;

        for _ in 0..(BASE_FREQUENCY * 60) {
            drv.cycles += 1;
            if drv.get_tacho() != last {
                result += 1;
                last = drv.get_tacho();
            }
        }

        assert_eq!(result / 10, DISK_RPM_OUTER * 120 / 10);
    }

    #[test]
    fn disk_double_tacho_inner() {
        let mut drv = FloppyDrive::new(0, DriveType::GCR800K, BASE_FREQUENCY);
        drv.floppy_inserted = true;
        drv.motor = true;
        drv.track = 79;

        let mut last = false;
        let mut result = 0;

        for _ in 0..(BASE_FREQUENCY * 60) {
            drv.cycles += 1;
            if drv.get_tacho() != last {
                result += 1;
                last = drv.get_tacho();
            }
        }

        // Roughly is good enough..
        assert_eq!(result / 10, DISK_RPM_INNER * 120 / 10);
    }

    /// Pairs of phases, as the Twiggy Mac's driver steps up through them
    const TWIGGY_STEPS: [[bool; 4]; 4] = [
        [true, true, false, false],
        [true, false, false, true],
        [false, false, true, true],
        [false, true, true, false],
    ];

    fn twiggy() -> FloppyDrive {
        let mut drv = FloppyDrive::new(0, DriveType::Twiggy, BASE_FREQUENCY);
        drv.disk_insert(FloppyImage::new(FloppyType::Twiggy, ""))
            .unwrap();
        drv
    }

    #[test]
    fn twiggy_steps_by_half_steps() {
        let mut drv = twiggy();
        let start = drv.twiggy_position;
        // Up a step, up half a step on one phase, and back
        drv.twiggy_phases(TWIGGY_STEPS[1]);
        assert_eq!(drv.twiggy_position, start + 2);
        drv.twiggy_phases([false, false, false, true]);
        assert_eq!(drv.twiggy_position, start + 3);
        drv.twiggy_phases(TWIGGY_STEPS[1]);
        assert_eq!(drv.twiggy_position, start + 2);
        // Nothing on, or opposite phases: no move
        drv.twiggy_phases([false; 4]);
        drv.twiggy_phases(TWIGGY_STEPS[3]);
        assert_eq!(drv.twiggy_position, start + 2);
    }

    #[test]
    fn twiggy_heads_face_each_other() {
        let mut drv = twiggy();
        // Where the driver calibrates: side 1's track 0, side 0's last
        drv.twiggy_position = twiggy::SIDE_1_TRACK_0;
        assert_eq!(drv.get_head_track(1), 0);
        assert_eq!(drv.get_head_track(0), 45);
        assert!(!drv.read_sense(0b0010));
        // CAL, with PH0 on and SEL off
        drv.twiggy_position += 8;
        assert!(drv.read_sense(0b0010));
        drv.twiggy_position = twiggy::SIDE_0_TRACK_0 + 3 * twiggy::HALF_STEPS_PER_TRACK;
        assert_eq!(drv.get_head_track(0), 3);
        assert_eq!(drv.get_head_track(1), 42);
        // Disk in place, with PH0 and SEL on
        assert!(drv.read_sense(0b0011));
    }

    #[test]
    fn twiggy_ejects_past_side_0_track_0() {
        let mut drv = twiggy();
        let mut step = 0;
        while drv.floppy_inserted {
            step = (step + 3) % 4;
            drv.twiggy_phases(TWIGGY_STEPS[step]);
            assert!(drv.twiggy_position >= twiggy::EJECT - 2);
        }
        assert!(!drv.read_sense(0b0011));
    }

    #[test]
    fn insert_spins_up() {
        let mut drv = FloppyDrive::new(0, DriveType::GCR800K, BASE_FREQUENCY);
        drv.disk_insert(FloppyImage::new(FloppyType::Mac800K, ""))
            .unwrap();
        // MOTORON, then let it spin up
        drv.write_drive_reg(0b0100);
        drv.cycles = BASE_FREQUENCY;
        assert!(drv.not_ready < drv.cycles);

        // The motor stays on across an eject: the next disk spins up
        drv.eject();
        drv.disk_insert(FloppyImage::new(FloppyType::Mac800K, ""))
            .unwrap();
        assert!(drv.not_ready > drv.cycles);
    }

    #[test]
    fn eject_arms_writeback_of_unsaved_writes() {
        let mut drv = FloppyDrive::new(0, DriveType::GCR800K, BASE_FREQUENCY);
        drv.disk_insert(FloppyImage::new(FloppyType::Mac800K, ""))
            .unwrap();
        drv.image_path = Some(PathBuf::from("disk.img"));
        drv.writeback_enabled = true;

        // Nothing written: nothing to save
        drv.eject();
        assert!(drv.eject_writeback.is_none());

        drv.disk_insert(FloppyImage::new(FloppyType::Mac800K, ""))
            .unwrap();
        drv.image_path = Some(PathBuf::from("disk.img"));
        drv.writeback_enabled = true;
        drv.write_bit(0, true);
        drv.eject();
        assert_eq!(drv.eject_writeback, Some(PathBuf::from("disk.img")));
        assert!(drv.floppy_ejected.is_some());
    }
}
