use anyhow::Result;
use log::*;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use snow_floppy::flux::FluxTicks;
use snow_floppy::{Floppy, FloppyImage, FloppyType, TrackLength, TrackType};
use strum::Display;

use crate::tickable::{Ticks, TICKS_PER_SECOND};

/// Direction the drive head is set to step to
#[derive(PartialEq, Eq, Clone, Copy, Debug, Display)]
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
    /// Single/double sided drive
    /// 0 = single, 1 = double
    SIDES = 0b1100,
    /// Disk ready (?)
    /// 0 = ready, 1 = not ready
    READY = 0b1101,

    /// Drive installed
    /// 0 = installed, 1 = not installed
    INSTALLED = 0b1110,

    /// PRESENT/HD (?)
    PRESENT = 0b1111,

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

    /// Drive motor on
    MOTORON = 0b0100,

    /// Drive motor off
    MOTOROFF = 0b1100,

    /// Eject disk
    EJECT = 0b1110,

    /// For unknown values
    UNKNOWN,
}

/// A single disk drive, attached to the drive controller
pub(crate) struct FloppyDrive {
    idx: usize,
    pub(super) cycles: Ticks,
    double_sided: bool,
    pub(crate) present: bool,

    pub(crate) floppy_inserted: bool,
    pub(crate) track: usize,
    stepdir: HeadStepDirection,
    pub(crate) motor: bool,
    pub(crate) floppy: FloppyImage,
    pub(super) track_position: usize,

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

    pub fn new(idx: usize, present: bool, double_sided: bool) -> Self {
        Self {
            idx,
            cycles: 0,
            double_sided,
            present,
            floppy_inserted: false,
            track: 4,
            stepdir: HeadStepDirection::Up,
            floppy: FloppyImage::new(FloppyType::Mac400K, ""),
            track_position: 0,
            motor: false,

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

    /// Returns true if drive's spindle motor is running
    pub(super) fn is_running(&self) -> bool {
        self.floppy_inserted && self.motor
    }

    /// Reads from the currently selected drive register
    pub(super) fn read_sense(&self, regraw: u8) -> bool {
        let reg = DriveReg::from_u8(regraw).unwrap_or(DriveReg::UNKNOWN);

        match reg {
            DriveReg::CISTN => !self.floppy_inserted,
            DriveReg::DIRTN => self.stepdir == HeadStepDirection::Down,
            DriveReg::SIDES => self.double_sided,
            DriveReg::MOTORON => !(self.motor && self.floppy_inserted),
            DriveReg::PRESENT => !self.present,
            DriveReg::INSTALLED => !self.present,
            DriveReg::READY => false,
            DriveReg::TKO if self.track == 0 => false,
            DriveReg::TKO => true,
            DriveReg::STEP => self.stepping == 0,
            DriveReg::TACH => self.get_tacho(),
            DriveReg::RDDATA0 => self.get_head_bit(0),
            DriveReg::RDDATA1 => self.get_head_bit(1),
            DriveReg::WRTPRT => !self.floppy.get_write_protect(),
            DriveReg::SWITCHED => false,
            _ => {
                warn!(
                    "Drive {}: unimplemented register read {:?} {:0b}",
                    self.idx, reg, regraw
                );
                true
            }
        }
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

        // Reset track position
        self.track_position = 0;
        self.flux_ticks = 0;
        self.flux_ticks_left = 0;

        // Track-to-track stepping time: 30ms
        self.stepping = TICKS_PER_SECOND / 60_000 * 30;
    }

    /// Writes to the currently selected drive register
    pub(super) fn write_drive_reg(&mut self, regraw: u8, cycles: Ticks) {
        let reg = DriveWriteReg::from_u8(regraw).unwrap_or(DriveWriteReg::UNKNOWN);

        match reg {
            DriveWriteReg::MOTORON => self.motor = true,
            DriveWriteReg::MOTOROFF => {
                self.motor = false;
            }
            DriveWriteReg::EJECT => {
                if self.floppy_inserted {
                    self.ejecting = Some(cycles + (TICKS_PER_SECOND / 2));
                }
            }
            DriveWriteReg::TRACKUP => {
                self.stepdir = HeadStepDirection::Up;
            }
            DriveWriteReg::TRACKDN => {
                self.stepdir = HeadStepDirection::Down;
            }
            DriveWriteReg::TRACKSTEP => self.step_head(),
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
        Ok(())
    }

    /// Gets the spindle motor speed in rounds/minute for the currently selected track
    pub const fn get_track_rpm(&self) -> Ticks {
        if !self.double_sided {
            // PWM-driven spindle motor speed control

            // Apple 3.5" single-sided drive specifications
            // 2.17.1.a: Track 0: 9.4% duty cycle: 305 - 380rpm
            const DUTY_T0: Ticks = 9;
            const SPEED_T0: Ticks = (380 + 305) / 2;
            // 2.17.2.b: Track 79: 91% duty cycle: 625 - 780rpm
            const DUTY_T79: Ticks = 91;
            const SPEED_T79: Ticks = (625 + 780) / 2;

            if self.pwm_dutycycle == 0 {
                return 0;
            }
            ((self.pwm_dutycycle - DUTY_T0) * (SPEED_T79 * 100 + SPEED_T0 * 100)
                / (DUTY_T79 - DUTY_T0))
                / 100
                + SPEED_T0
        } else {
            // Automatic spindle motor speed control
            match self.track {
                0..=15 => 402,
                16..=31 => 438,
                32..=47 => 482,
                48..=63 => 536,
                64..=79 => 603,
                _ => unreachable!(),
            }
        }
    }

    /// Gets the amount of ticks a physical bit is under the drive head
    pub fn get_ticks_per_bit(&self) -> Ticks {
        if self.get_track_rpm() == 0 || !self.floppy_inserted {
            return Ticks::MAX;
        }
        ((TICKS_PER_SECOND * 60)
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
        let ticks_per_min = TICKS_PER_SECOND * 60;
        let ticks_per_edge = ticks_per_min / edges_per_min;
        (self.cycles / ticks_per_edge % 2) != 0
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
            TrackType::Bitstream => self.floppy.get_track_bit(head, track, self.track_position),
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
    pub(super) fn eject(&mut self) {
        info!("Drive {}: disk ejected", self.idx);
        self.floppy_inserted = false;
        self.ejecting = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disk revolutions/minute at outer track (0)
    const DISK_RPM_OUTER: Ticks = 402;

    /// Disk revolutions/minute at inner track (79)
    const DISK_RPM_INNER: Ticks = 603;

    #[test]
    fn disk_double_tacho_outer() {
        let mut drv = FloppyDrive::new(0, true, true);
        drv.floppy_inserted = true;
        drv.motor = true;
        drv.track = 0;

        let mut last = false;
        let mut result = 0;

        for _ in 0..(TICKS_PER_SECOND * 60) {
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
        let mut drv = FloppyDrive::new(0, true, true);
        drv.floppy_inserted = true;
        drv.motor = true;
        drv.track = 79;

        let mut last = false;
        let mut result = 0;

        for _ in 0..(TICKS_PER_SECOND * 60) {
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