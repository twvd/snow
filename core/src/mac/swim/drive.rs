use anyhow::Result;
use log::*;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use serde::{Deserialize, Serialize};
use snow_floppy::flux::FluxTicks;
use snow_floppy::{Floppy, FloppyImage, FloppyType, TrackLength, TrackType};
use strum::Display;

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
}

impl DriveType {
    pub const fn io_superdrive(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM => false,
            Self::SuperDrive => true,
        }
    }

    pub const fn io_present(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K => false,
            Self::GCR800K | Self::GCR800KPWM => true,
            Self::SuperDrive => unreachable!(),
        }
    }

    pub const fn io_mfm(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM => false,
            Self::SuperDrive => true,
        }
    }

    pub const fn io_rddata1(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM => false,
            Self::SuperDrive => true,
        }
    }

    pub const fn io_ready(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM | Self::SuperDrive => false,
        }
    }

    pub const fn is_doublesided(self) -> bool {
        match self {
            Self::None => true,
            Self::GCR400K => false,
            Self::GCR800K | Self::GCR800KPWM | Self::SuperDrive => true,
        }
    }

    pub const fn has_pwm_control(self) -> bool {
        match self {
            Self::None => false,
            Self::GCR400K => true,
            Self::GCR800KPWM => true,
            Self::GCR800K | Self::SuperDrive => false,
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
    /// Read data, low head
    RDDATA0 = 0b1000,
    /// Read data, upper head
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
    base_frequency: Ticks,
    pub(super) drive_type: DriveType,
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

    // While > 0, the drive head is moving
    pub(super) stepping: Ticks,
    pub(super) ejecting: Option<Ticks>,

    /// Amount of flux ticks for current transition (for flux tracks)
    pub(super) flux_ticks: FluxTicks,

    /// Amount of flux ticks left for current transition (for flux tracks)
    pub(super) flux_ticks_left: FluxTicks,

    pub(super) head_bit: [bool; 2],

    pub(super) pwm_avg_sum: i64,
    pub(super) pwm_avg_count: usize,
    pub(super) pwm_dutycycle: Ticks,
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
            ejecting: None,

            flux_ticks: 0,
            flux_ticks_left: 0,

            head_bit: [false; 2],

            pwm_avg_sum: 0,
            pwm_avg_count: 0,
            pwm_dutycycle: 0,
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

    /// Reads from the currently selected drive register
    pub(super) fn read_sense(&self, regraw: u8) -> bool {
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
            DriveReg::READY => self.drive_type.io_ready(),
            DriveReg::TKO if self.track == 0 => false,
            DriveReg::TKO => true,
            DriveReg::STEP => self.stepping == 0,
            DriveReg::TACH => self.get_tacho(),
            DriveReg::RDDATA0 => self.get_head_bit(0),
            DriveReg::RDDATA1 => {
                if self.motor {
                    self.get_head_bit(1)
                } else {
                    self.drive_type.io_rddata1()
                }
            }
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

    /// Moves the drive head one step in the selected position
    fn step_head(&mut self) {
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
                    error!("Drive {}: head moving lower than track 0", self.idx);
                } else {
                    self.track -= 1;
                }
            }
        }

        self.flux_ticks = 0;
        self.flux_ticks_left = 0;

        // Track-to-track stepping time: 30ms
        self.stepping = self.base_frequency / 60_000 * 30;
    }

    /// Writes to the currently selected drive register
    pub(super) fn write_drive_reg(&mut self, regraw: u8, cycles: Ticks) {
        let reg = DriveWriteReg::from_u8(regraw).unwrap_or(DriveWriteReg::UNKNOWN);

        match reg {
            DriveWriteReg::MOTORON => {
                self.track_position = 0;
                self.motor = true;
            }
            DriveWriteReg::MOTOROFF => {
                self.motor = false;
            }
            DriveWriteReg::EJECT => {
                if self.floppy_inserted {
                    self.ejecting = Some(cycles);
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
        Ok(())
    }

    /// Gets the spindle motor speed in rounds/minute for the currently selected track
    pub fn get_track_rpm(&self) -> Ticks {
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
            ((self.pwm_dutycycle - DUTY_T0) * (SPEED_T79 * 100 + SPEED_T0 * 100)
                / (DUTY_T79 - DUTY_T0))
                / 100
                + SPEED_T0
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
        if self.get_track_rpm() == 0 || !self.floppy_inserted {
            return Ticks::MAX;
        }
        ((self.base_frequency * 60)
            / self.get_track_rpm()
            / self.floppy.get_type().get_approx_track_length(self.track))
            + 1
    }

    /// Gets the current state of the TACH (spindle motor tachometer) signal
    pub fn get_tacho(&self) -> bool {
        if !self.motor || self.get_track_rpm() == 0 {
            return false;
        }

        // The disk spins at 390-605rpm
        // Each rotation produces 60 tacho pulses (= 120 edges)
        let pulses_per_min = self.get_track_rpm() * Self::TACHO_SPEED;
        let edges_per_min = pulses_per_min * 2;
        let ticks_per_min = self.base_frequency * 60;
        let ticks_per_edge = ticks_per_min / edges_per_min;
        !(self.cycles / ticks_per_edge).is_multiple_of(2)
    }

    /// Gets the active selected track offset
    pub(super) fn get_active_track(&self) -> usize {
        self.track
    }

    /// Gets the length of a track on the loaded floppy
    pub(super) fn get_track_len(&self, side: usize, track: usize) -> TrackLength {
        self.floppy.get_track_length(side, track)
    }

    /// Gets the physical disk bit currently under a head
    fn get_head_bit(&self, head: usize) -> bool {
        let track = self.get_active_track();
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
        let TrackLength::Bits(tracklen) = self.get_track_len(head, self.get_active_track()) else {
            unreachable!()
        };
        self.track_position = (self.track_position + 1) % tracklen;

        self.get_head_bit(head)
    }

    /// Writes a bit to the current track position
    pub(super) fn write_bit(&mut self, head: usize, bit: bool) {
        self.floppy
            .set_track_bit(head, self.track, self.track_position, bit);
    }

    /// Ejects the disk
    pub fn eject(&mut self) {
        info!("Drive {}: disk ejected", self.idx);
        self.switched = true;
        self.floppy_inserted = false;
        self.ejecting = None;
        self.mfm = self.drive_type.io_mfm();

        self.floppy_ejected = Some(Box::new(self.floppy.clone()));
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
            dbgprop_udec!("Head stepping timer", self.stepping),
            dbgprop_udec!("Track", self.track),
            dbgprop_udec!("Track position", self.track_position),
            dbgprop_udec!("Track RPM", self.get_track_rpm()),
            dbgprop_udec!("Ticks per bit", self.get_ticks_per_bit()),
            dbgprop_sdec!("Flux transition len", self.flux_ticks.into()),
            dbgprop_sdec!("Flux transition left", self.flux_ticks_left.into()),
            dbgprop_udec!("PWM dutycycle", self.pwm_dutycycle),
        ]
    }
}

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
}
