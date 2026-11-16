use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use log::*;
use num::clamp;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use serde::{Deserialize, Serialize};
use snow_floppy::loaders::{Autodetect, FloppyImageLoader, FloppyImageSaver, Moof};
use snow_floppy::{Floppy, FloppyImage, FloppyType};
use strum::Display;

use super::device::SwimBusDevice;
use super::stream::FloppyStream;
use crate::debuggable::DebuggableProperties;
use crate::devices::{
    BusKind, DeviceConfig, DeviceModel, DeviceOps, FloppyFunction, FloppyImages, FloppyInfo,
    FloppyStatus, FunctionConfig, FunctionInfo, FunctionMut, MadeDevice, ModelInfo,
};
use crate::mac::MacModel;
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
    GCR400K,
    GCR800K,
    /// PWM-controlled 800K drive for use in the 128K/512K
    GCR800KPWM,
    SuperDrive,
}

impl DriveType {
    pub const fn io_superdrive(self) -> bool {
        match self {
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM => false,
            Self::SuperDrive => true,
        }
    }

    pub const fn io_present(self) -> bool {
        match self {
            Self::GCR400K => false,
            Self::GCR800K | Self::GCR800KPWM => true,
            Self::SuperDrive => unreachable!(),
        }
    }

    pub const fn io_mfm(self) -> bool {
        match self {
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM => false,
            Self::SuperDrive => true,
        }
    }

    pub const fn io_rddata1(self) -> bool {
        match self {
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM => false,
            Self::SuperDrive => true,
        }
    }

    pub const fn io_ready(self) -> bool {
        match self {
            Self::GCR400K | Self::GCR800K | Self::GCR800KPWM | Self::SuperDrive => false,
        }
    }

    pub const fn is_doublesided(self) -> bool {
        match self {
            Self::GCR400K => false,
            Self::GCR800K | Self::GCR800KPWM | Self::SuperDrive => true,
        }
    }

    pub const fn has_pwm_control(self) -> bool {
        match self {
            Self::GCR400K => true,
            Self::GCR800KPWM => true,
            Self::GCR800K | Self::SuperDrive => false,
        }
    }

    /// The device model of a drive of this type
    pub const fn device_model(self) -> DeviceModel {
        match self {
            Self::GCR400K => DeviceModel::Mac400KDrive,
            Self::GCR800K => DeviceModel::Mac800KDrive,
            Self::GCR800KPWM => DeviceModel::Mac800KPwmDrive,
            Self::SuperDrive => DeviceModel::MacSuperDrive,
        }
    }

    /// The type of drive of a device model, if it is one of a Mac floppy drive
    pub const fn of_model(model: DeviceModel) -> Option<Self> {
        match model {
            DeviceModel::Mac400KDrive => Some(Self::GCR400K),
            DeviceModel::Mac800KDrive => Some(Self::GCR800K),
            DeviceModel::Mac800KPwmDrive => Some(Self::GCR800KPWM),
            DeviceModel::MacSuperDrive => Some(Self::SuperDrive),
            _ => None,
        }
    }

    pub const fn compatible_floppies(self) -> &'static [FloppyType] {
        match self {
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

/// What the Mac's floppy drives accept and can do
const MAC_FLOPPY: FloppyInfo = FloppyInfo {
    images: FloppyImages::Mac,
    blank_formats: &[FloppyType::Mac800K],
    guest_ejects: true,
    rpm_control: false,
    writable: true,
};

/// The drive the Mac model has on its first connector
fn first_drive(mac: MacModel) -> Option<DeviceModel> {
    mac.floppy_drives().first().map(|(_, model)| *model)
}

fn fits_400k(mac: MacModel) -> bool {
    first_drive(mac) == Some(DeviceModel::Mac400KDrive)
}

fn fits_800k(mac: MacModel) -> bool {
    first_drive(mac) == Some(DeviceModel::Mac800KDrive)
}

/// The 800K drive with its speed set by PWM, which the 128K and 512K can be upgraded with
fn fits_800k_pwm(mac: MacModel) -> bool {
    fits_400k(mac)
}

fn fits_superdrive(mac: MacModel) -> bool {
    first_drive(mac) == Some(DeviceModel::MacSuperDrive)
}

fn create_400k(_config: &DeviceConfig) -> Result<MadeDevice> {
    Ok(MadeDevice::Swim(Box::new(FloppyDrive::new(
        DriveType::GCR400K,
    ))))
}

fn create_800k(_config: &DeviceConfig) -> Result<MadeDevice> {
    Ok(MadeDevice::Swim(Box::new(FloppyDrive::new(
        DriveType::GCR800K,
    ))))
}

fn create_800k_pwm(_config: &DeviceConfig) -> Result<MadeDevice> {
    Ok(MadeDevice::Swim(Box::new(FloppyDrive::new(
        DriveType::GCR800KPWM,
    ))))
}

fn create_superdrive(_config: &DeviceConfig) -> Result<MadeDevice> {
    Ok(MadeDevice::Swim(Box::new(FloppyDrive::new(
        DriveType::SuperDrive,
    ))))
}

pub const INFO_400K: ModelInfo = ModelInfo {
    fits: fits_400k,
    fixed: true,
    create: Some(create_400k),
    ..ModelInfo::base(
        DeviceModel::Mac400KDrive,
        "400K drive",
        BusKind::Swim,
        FunctionInfo::Floppy(FloppyInfo {
            rpm_control: true,
            ..MAC_FLOPPY
        }),
    )
};

pub const INFO_800K: ModelInfo = ModelInfo {
    fits: fits_800k,
    fixed: true,
    create: Some(create_800k),
    ..ModelInfo::base(
        DeviceModel::Mac800KDrive,
        "800K drive",
        BusKind::Swim,
        FunctionInfo::Floppy(MAC_FLOPPY),
    )
};

pub const INFO_800K_PWM: ModelInfo = ModelInfo {
    fits: fits_800k_pwm,
    fixed: true,
    create: Some(create_800k_pwm),
    ..ModelInfo::base(
        DeviceModel::Mac800KPwmDrive,
        "800K drive (PWM)",
        BusKind::Swim,
        FunctionInfo::Floppy(FloppyInfo {
            rpm_control: true,
            ..MAC_FLOPPY
        }),
    )
};

pub const INFO_SUPERDRIVE: ModelInfo = ModelInfo {
    fits: fits_superdrive,
    fixed: true,
    create: Some(create_superdrive),
    ..ModelInfo::base(
        DeviceModel::MacSuperDrive,
        "SuperDrive",
        BusKind::Swim,
        FunctionInfo::Floppy(FloppyInfo {
            blank_formats: &[FloppyType::Mac800K, FloppyType::Mfm144M],
            ..MAC_FLOPPY
        }),
    )
};

/// A single disk drive, attached to the drive controller
#[derive(Serialize, Deserialize)]
pub(crate) struct FloppyDrive {
    /// The connector the drive is on, from 0
    idx: usize,
    /// The ticks of the controller in one second
    base_frequency: Ticks,
    drive_type: DriveType,
    cycles: Ticks,

    pub(super) floppy_inserted: bool,
    stepdir: HeadStepDirection,
    motor: bool,

    /// The disk in the drive, the track under the heads and the position on it
    pub(super) stream: FloppyStream,

    /// Copy of last ejected floppy image
    floppy_ejected: Option<Box<FloppyImage>>,

    /// True if disk was switched without eject
    switched: bool,

    /// In MFM mode (in GCR mode when false)
    mfm: bool,

    // While > cycles, the drive head is moving
    stepping: Ticks,
    // While > cycles, the drive is not ready
    // (e.g. stepping, spindle motor changing speed)
    pub(super) not_ready: Ticks,

    pwm_avg_sum: i64,
    pwm_avg_count: usize,
    pwm_dutycycle: Ticks,

    /// RPM adjustment for PWM-controlled drives
    rpm_adjustment: i32,

    /// Source file path of the currently inserted image, if it was loaded
    /// from a writeback-capable file. Cleared on eject.
    #[serde(skip)]
    image_path: Option<PathBuf>,

    /// Per-drive writeback opt-in. Only meaningful when [`Self::image_path`]
    /// is set and `floppy.supports_writeback()`.
    #[serde(skip)]
    writeback_enabled: bool,

    /// Set by [`DriveWriteReg::MOTOROFF`] when a writeback save is due. The
    /// next [`FloppyFunction::write_back`] does the save and clears it.
    #[serde(skip)]
    pending_writeback: bool,

    /// Path to save [`Self::floppy_ejected`] to: set when a disk with unsaved
    /// writes and writeback armed is ejected
    #[serde(skip)]
    eject_writeback: Option<PathBuf>,
}

impl FloppyDrive {
    /// Amount of tracks per disk side
    const DISK_TRACKS: usize = 80;

    /// Tacho pulses/disk revolution
    const TACHO_SPEED: Ticks = 60;

    /// A drive that isn't on a connector yet: `connected()` gives it its place and the
    /// frequency of its ticks
    pub fn new(drive_type: DriveType) -> Self {
        Self {
            idx: 0,
            base_frequency: 0,
            drive_type,
            cycles: 0,
            floppy_inserted: false,
            floppy_ejected: None,
            stepdir: HeadStepDirection::Up,
            stream: FloppyStream::new(drive_type.is_doublesided()),
            motor: false,
            mfm: drive_type.io_mfm(),
            switched: false,

            stepping: 0,
            not_ready: 0,

            pwm_avg_sum: 0,
            pwm_avg_count: 0,
            pwm_dutycycle: 0,
            rpm_adjustment: 0,

            image_path: None,
            writeback_enabled: false,
            pending_writeback: false,
            eject_writeback: None,
        }
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
        self.is_running() && (0..2000).contains(&self.stream.track_position)
    }

    /// Reads from the currently selected drive register
    // Silence clippy so we can write easier to understand expressions for the
    // inverse signals.
    #[allow(clippy::nonminimal_bool)]
    pub(super) fn read_sense(&self, regraw: u8) -> bool {
        let reg = DriveReg::from_u8(regraw).unwrap_or(DriveReg::UNKNOWN);

        let result = match reg {
            DriveReg::CISTN => !self.floppy_inserted,
            DriveReg::DIRTN => self.stepdir == HeadStepDirection::Down,
            DriveReg::SIDES => self.drive_type.is_doublesided(),
            DriveReg::MOTORON => !(self.motor && self.floppy_inserted),
            DriveReg::PRESENTHD => {
                if self.drive_type == DriveType::SuperDrive {
                    !(self.stream.floppy.get_type() == FloppyType::Mfm144M && self.floppy_inserted)
                } else {
                    self.drive_type.io_present()
                }
            }
            DriveReg::INSTALLED => false,
            DriveReg::READY if !self.motor => self.drive_type.io_ready(),
            DriveReg::READY => !(self.not_ready < self.cycles),
            DriveReg::TKO => self.stream.track != 0,
            DriveReg::STEP => !self.motor || self.stepping < self.cycles,
            DriveReg::TACH => self.get_tacho(),
            DriveReg::RDDATA0 | DriveReg::RDDATA1 if self.mfm && self.motor => !self.at_index(),
            DriveReg::RDDATA0 => self.get_head_bit(0),
            DriveReg::RDDATA1 if self.motor => self.get_head_bit(1),
            DriveReg::RDDATA1 => self.drive_type.io_rddata1(),
            DriveReg::MFM => self.mfm,
            DriveReg::SUPERDRIVE => self.drive_type.io_superdrive(),
            DriveReg::WRTPRT => !self.stream.floppy.get_write_protect(),
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
        let previous_rpm = self.get_track_rpm();

        if self.stepping > self.cycles {
            log::warn!("Head stepping before /STEP released");
        }

        match self.stepdir {
            HeadStepDirection::Up => {
                if (self.stream.track + 1) >= Self::DISK_TRACKS {
                    error!("Drive {}: head moving further than track 79!", self.idx);
                } else {
                    self.stream.track += 1;
                }
            }
            HeadStepDirection::Down => {
                if self.stream.track == 0 {
                    // The Sony driver recalibrates some drives with 80 steps,
                    // without looking at the track 0 signal
                    trace!("Drive {}: head moving lower than track 0", self.idx);
                } else {
                    self.stream.track -= 1;
                }
            }
        }

        self.stream.flux_ticks = 0;
        self.stream.flux_ticks_left = 0;

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
                self.stream.track_position = 0;
                self.motor = true;

                // 400K drive datasheet 2.3.3 'Motor start time' is 400ms.
                self.not_ready = self.cycles + (self.base_frequency / 1000 * 400);
            }
            DriveWriteReg::MOTOROFF => {
                self.motor = false;
                if self.writeback_enabled
                    && self.image_path.is_some()
                    && self.stream.floppy.is_dirty()
                {
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
    pub(super) fn disk_insert(&mut self, image: FloppyImage) -> Result<()> {
        info!(
            "Drive {}: disk inserted, {} tracks, title: '{}'",
            self.idx,
            image.get_track_count() * image.get_side_count(),
            image.get_title()
        );
        self.stream.floppy = image;
        self.floppy_inserted = true;
        self.switched = true;
        self.stream.track_position = 0;
        if self.motor {
            // "the disk motor is turned on if a disk is in the drive" (400K
            // drive spec 699-0285-A, 3.2.7): the new disk spins up
            self.not_ready = self.cycles + (self.base_frequency / 1000 * 400);
        }
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
            match self.stream.track {
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
        self.stream.ticks_per_bit()
    }

    pub(super) fn update_ticks_per_bit(&mut self) {
        self.stream.ticks_per_bit = if self.get_track_rpm() == 0 || !self.floppy_inserted {
            Ticks::MAX
        } else {
            ((self.base_frequency * 60)
                / self.get_track_rpm()
                / self
                    .stream
                    .floppy
                    .get_type()
                    .get_approx_track_length(self.stream.track) as u64)
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
        let pulses_per_min = self.get_track_rpm() * Self::TACHO_SPEED;
        let edges_per_min = pulses_per_min * 2;
        let ticks_per_min = self.base_frequency * 60;
        let ticks_per_edge = ticks_per_min / edges_per_min;
        !(self.cycles / ticks_per_edge).is_multiple_of(2)
    }

    /// Gets the physical disk bit currently under a head
    fn get_head_bit(&self, head: usize) -> bool {
        // Garbage if drive is not yet ready
        if self.not_ready > self.cycles {
            return self.stream.weak_bit();
        }
        self.stream.head_bit(head)
    }

    /// Saves the disk that was ejected with unsaved writes to the file it
    /// came from, if the eject armed it
    fn write_back_ejected(&mut self) -> Result<()> {
        let Some(path) = self.eject_writeback.take() else {
            return Ok(());
        };
        let Some(image) = self.floppy_ejected.as_deref() else {
            return Ok(());
        };

        let name = file_name(&path);
        write_image(&path, image)
            .with_context(|| format!("writeback of ejected disk to '{}' failed", name))?;
        info!(
            "Floppy #{}: writeback saved ejected disk '{}'",
            self.idx + 1,
            name
        );
        Ok(())
    }

    /// Saves the disk in the drive to the file it came from, if writeback is
    /// armed and a save is due. Clears the dirty flag on success.
    fn write_back_inserted(&mut self, force: bool) -> Result<()> {
        if !self.writeback_enabled
            || !(force || self.pending_writeback)
            || !self.stream.floppy.is_dirty()
        {
            return Ok(());
        }
        let Some(path) = self.image_path.clone() else {
            return Ok(());
        };
        self.pending_writeback = false;

        let name = file_name(&path);
        write_image(&path, &self.stream.floppy)
            .with_context(|| format!("writeback to '{}' failed", name))?;
        self.stream.floppy.clear_dirty();
        info!("Floppy #{}: writeback auto-saved '{}'", self.idx + 1, name);
        Ok(())
    }
}

/// Writes a disk image to a file, in the format it was loaded from
fn write_image(path: &Path, image: &FloppyImage) -> Result<()> {
    crate::util::atomic_write(path, |f| snow_floppy::loaders::save_image(image, f))
}

/// The name of an image file, for messages
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl FloppyFunction for FloppyDrive {
    fn status(&self) -> FloppyStatus {
        FloppyStatus {
            title: self.stream.floppy.get_title().to_owned(),
            ejected: !self.floppy_inserted,
            dirty: self.stream.floppy.is_dirty(),
            writeback_supported: self.image_path.is_some()
                && self.stream.floppy.supports_writeback(),
            writeback_enabled: self.writeback_enabled,
        }
    }

    fn load(&mut self, path: &Path, read_only: bool) -> Result<()> {
        let mut image = Autodetect::load_file(&path.to_string_lossy())
            .map_err(|e| anyhow!("Cannot load image '{}': {:?}", path.display(), e))?;
        if read_only {
            image.set_force_wp();
        }
        let writeback_path = image.supports_writeback().then(|| path.to_path_buf());
        self.disk_insert(image)?;
        self.image_path = writeback_path;
        self.writeback_enabled = false;
        self.pending_writeback = false;
        Ok(())
    }

    fn eject(&mut self) {
        info!("Drive {}: disk ejected", self.idx);
        self.switched = true;
        self.floppy_inserted = false;
        self.mfm = self.drive_type.io_mfm();

        self.floppy_ejected = Some(Box::new(self.stream.floppy.clone()));

        // The writes since the last save must not be lost with the disk,
        // whoever ejects it
        self.eject_writeback = if self.writeback_enabled && self.stream.floppy.is_dirty() {
            self.image_path.clone()
        } else {
            None
        };
        self.image_path = None;
        self.writeback_enabled = false;
        self.pending_writeback = false;
    }

    fn insert_image(&mut self, mut image: Box<FloppyImage>, read_only: bool) -> Result<()> {
        if read_only {
            image.set_force_wp();
        }
        self.disk_insert(*image)
    }

    fn save(&mut self, path: &Path) -> Result<()> {
        Moof::save_file(&self.stream.floppy, &path.to_string_lossy())
    }

    fn set_writeback(&mut self, enabled: bool) -> Result<()> {
        if enabled && (self.image_path.is_none() || !self.stream.floppy.supports_writeback()) {
            bail!("Writeback unavailable: image was not loaded from a writeback-capable file");
        }
        self.writeback_enabled = enabled;
        self.pending_writeback = false;
        Ok(())
    }

    fn write_back(&mut self, force: bool) -> Result<()> {
        let ejected = self.write_back_ejected();
        let inserted = self.write_back_inserted(force);
        ejected.and(inserted)
    }

    fn take_ejected_image(&mut self) -> Option<Box<FloppyImage>> {
        self.floppy_ejected.take()
    }

    fn seed_weak_bits(&mut self, seed: Option<u64>) {
        self.stream
            .seed_weak_bits(seed.map(|s| s.wrapping_add(self.idx as u64 + 1)));
    }

    fn set_rpm_adjustment(&mut self, rpm: i32) -> Result<()> {
        if !self.drive_type.has_pwm_control() {
            bail!("The speed of this drive can't be adjusted");
        }
        self.rpm_adjustment = rpm;
        self.update_ticks_per_bit();
        Ok(())
    }
}

impl DeviceOps for FloppyDrive {
    fn info(&self) -> &'static ModelInfo {
        match self.drive_type {
            DriveType::GCR400K => &INFO_400K,
            DriveType::GCR800K => &INFO_800K,
            DriveType::GCR800KPWM => &INFO_800K_PWM,
            DriveType::SuperDrive => &INFO_SUPERDRIVE,
        }
    }

    fn config(&self) -> FunctionConfig {
        FunctionConfig::Floppy
    }

    fn function(&mut self) -> FunctionMut<'_> {
        FunctionMut::Floppy(self)
    }

    fn debug_properties(&self) -> DebuggableProperties {
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
            dbgprop_udec!("Track", self.stream.track),
            dbgprop_udec!("Track position", self.stream.track_position),
            dbgprop_bool!("Index signal", self.mfm && self.at_index()),
            dbgprop_udec!("Track RPM", self.get_track_rpm()),
            dbgprop_udec!("Ticks per bit", self.get_ticks_per_bit()),
            dbgprop_sdec!("Flux transition len", self.stream.flux_ticks.into()),
            dbgprop_sdec!("Flux transition left", self.stream.flux_ticks_left.into()),
            dbgprop_udec!("PWM dutycycle", self.pwm_dutycycle),
        ]
    }
}

#[typetag::serde]
impl SwimBusDevice for FloppyDrive {
    fn connected(&mut self, connector: usize, tick_rate: Ticks) {
        self.idx = connector;
        self.base_frequency = tick_rate;
    }

    fn set_lines(&mut self, _lines: u8) {}

    fn strobe(&mut self, lines: u8) -> bool {
        self.write_drive_reg(lines);
        false
    }

    fn sense(&self, lines: u8) -> bool {
        self.read_sense(lines)
    }

    fn set_pwm(&mut self, pulse: u8) {
        if !self.drive_type.has_pwm_control() {
            return;
        }

        self.pwm_avg_sum += i64::from(pulse);
        self.pwm_avg_count += 1;
        if self.pwm_avg_count >= 100 {
            let idx = clamp(
                self.pwm_avg_sum / (self.pwm_avg_count as i64 / 10) - 11,
                0,
                399,
            );
            self.pwm_dutycycle = ((idx * 100) / 419) as Ticks;
            self.update_ticks_per_bit();
            self.pwm_avg_sum = 0;
            self.pwm_avg_count = 0;
        }
    }

    fn tick(&mut self, ticks: Ticks) {
        self.cycles += ticks;
    }

    #[inline]
    fn stream(&mut self) -> Option<&mut FloppyStream> {
        self.stream.spinning = self.is_running();
        self.stream.not_ready = self.not_ready > self.cycles;
        Some(&mut self.stream)
    }

    #[cfg(test)]
    fn as_floppy_drive(&mut self) -> &mut FloppyDrive {
        self
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

    /// A drive on a connector of a controller
    fn drive(connector: usize, drive_type: DriveType) -> FloppyDrive {
        let mut drv = FloppyDrive::new(drive_type);
        drv.connected(connector, BASE_FREQUENCY);
        drv
    }

    #[test]
    fn disk_double_tacho_outer() {
        let mut drv = drive(0, DriveType::GCR800K);
        drv.floppy_inserted = true;
        drv.motor = true;
        drv.stream.track = 0;

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
        let mut drv = drive(0, DriveType::GCR800K);
        drv.floppy_inserted = true;
        drv.motor = true;
        drv.stream.track = 79;

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

    #[test]
    fn insert_spins_up() {
        let mut drv = drive(0, DriveType::GCR800K);
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
        let mut drv = drive(0, DriveType::GCR800K);
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
        drv.stream.write_bit(0, true);
        drv.eject();
        assert_eq!(drv.eject_writeback, Some(PathBuf::from("disk.img")));
        assert!(drv.floppy_ejected.is_some());
    }

    /// A folder of its own for a test, removed when the test is done
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("snow_drive_{}_{}", name, std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// A blank 800K disk in a MOOF file, a format that can be written back
        fn moof(&self) -> PathBuf {
            let path = self.0.join("disk.moof");
            Moof::save_file(
                &FloppyImage::new(FloppyType::Mac800K, "disk"),
                &path.to_string_lossy(),
            )
            .unwrap();
            path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The bit under the head of a drive that was just made, as an image file has it
    fn file_bit(path: &Path) -> bool {
        let image = Autodetect::load_file(&path.to_string_lossy()).unwrap();
        image.get_track_bit(0, 4, 0)
    }

    /// Writes the bit under the head of a drive that was just made, changing its value, and
    /// returns the new value. A blank disk is filled with noise.
    fn flip_bit(drv: &mut FloppyDrive) -> bool {
        let bit = !drv.stream.floppy.get_track_bit(0, 4, 0);
        drv.stream.write_bit(0, bit);
        bit
    }

    const MOTOROFF: u8 = 0b1100;

    #[test]
    fn load_puts_the_disk_of_a_file_in_the_drive() {
        let dir = TestDir::new("load");
        let mut drv = drive(0, DriveType::GCR800K);
        assert!(drv.status().ejected);

        drv.load(&dir.moof(), false).unwrap();
        let status = drv.status();
        assert!(!status.ejected);
        assert!(!status.dirty);
        assert!(status.writeback_supported);
        assert!(!status.writeback_enabled);
        assert!(!drv.stream.floppy.get_write_protect());

        drv.load(&dir.moof(), true).unwrap();
        assert!(drv.stream.floppy.get_write_protect());
    }

    #[test]
    fn load_refuses_a_missing_file() {
        let dir = TestDir::new("refuse");
        let mut drv = drive(0, DriveType::GCR800K);
        let e = drv.load(&dir.0.join("none.moof"), false).unwrap_err();
        assert!(e.to_string().starts_with("Cannot load image"));
        assert!(drv.status().ejected);
    }

    #[test]
    fn eject_hands_over_the_image_once() {
        let mut drv = drive(0, DriveType::GCR800K);
        let image = Box::new(FloppyImage::new(FloppyType::Mac800K, "blank"));
        drv.insert_image(image, false).unwrap();
        assert_eq!(drv.status().title, "blank");
        assert!(drv.take_ejected_image().is_none());

        drv.eject();
        assert!(drv.status().ejected);
        assert_eq!(drv.take_ejected_image().unwrap().get_title(), "blank");
        assert!(drv.take_ejected_image().is_none());
    }

    #[test]
    fn insert_image_has_no_file_to_write_back_to() {
        let mut drv = drive(0, DriveType::GCR800K);
        let image = Box::new(FloppyImage::new(FloppyType::Mac800K, ""));
        drv.insert_image(image, true).unwrap();
        assert!(drv.stream.floppy.get_write_protect());
        assert!(!drv.status().writeback_supported);
        assert!(drv.set_writeback(true).is_err());
        assert!(!drv.status().writeback_enabled);
        // Turning it off is never refused
        drv.set_writeback(false).unwrap();
    }

    #[test]
    fn save_writes_an_image_file() {
        let dir = TestDir::new("save");
        let mut drv = drive(0, DriveType::GCR800K);
        let image = Box::new(FloppyImage::new(FloppyType::Mac800K, ""));
        drv.insert_image(image, false).unwrap();
        let bit = flip_bit(&mut drv);

        let path = dir.0.join("saved.moof");
        drv.save(&path).unwrap();
        assert_eq!(file_bit(&path), bit);
    }

    #[test]
    fn writeback_saves_when_the_motor_stops() {
        let dir = TestDir::new("motor");
        let path = dir.moof();
        let mut drv = drive(0, DriveType::GCR800K);
        drv.load(&path, false).unwrap();
        drv.set_writeback(true).unwrap();
        assert!(drv.status().writeback_enabled);

        // The disk still turns: nothing is saved yet
        let bit = flip_bit(&mut drv);
        drv.write_back(false).unwrap();
        assert!(drv.status().dirty);
        assert_eq!(file_bit(&path), !bit);

        drv.write_drive_reg(MOTOROFF);
        drv.write_back(false).unwrap();
        assert!(!drv.status().dirty);
        assert_eq!(file_bit(&path), bit);
    }

    #[test]
    fn writeback_saves_a_turning_disk_when_forced() {
        let dir = TestDir::new("force");
        let path = dir.moof();
        let mut drv = drive(0, DriveType::GCR800K);
        drv.load(&path, false).unwrap();

        // Without writeback, nothing is saved
        let bit = flip_bit(&mut drv);
        drv.write_back(true).unwrap();
        assert!(drv.status().dirty);
        assert_eq!(file_bit(&path), !bit);

        drv.set_writeback(true).unwrap();
        drv.write_back(true).unwrap();
        assert!(!drv.status().dirty);
        assert_eq!(file_bit(&path), bit);
    }

    #[test]
    fn writeback_saves_an_ejected_disk() {
        let dir = TestDir::new("ejected");
        let path = dir.moof();
        let mut drv = drive(0, DriveType::GCR800K);
        drv.load(&path, false).unwrap();
        drv.set_writeback(true).unwrap();
        let bit = flip_bit(&mut drv);

        drv.eject();
        assert_eq!(file_bit(&path), !bit);
        drv.write_back(false).unwrap();
        assert_eq!(file_bit(&path), bit);
        // The ejected disk took its file and its writeback with it
        assert!(!drv.status().writeback_enabled);
        assert!(!drv.status().writeback_supported);
    }

    #[test]
    fn writeback_reports_a_failed_save() {
        let dir = TestDir::new("failed");
        let path = dir.moof();
        let mut drv = drive(0, DriveType::GCR800K);
        drv.load(&path, false).unwrap();
        drv.set_writeback(true).unwrap();
        flip_bit(&mut drv);
        drv.write_drive_reg(MOTOROFF);

        // The folder of the file is gone
        std::fs::remove_dir_all(&dir.0).unwrap();
        let e = drv.write_back(false).unwrap_err();
        assert_eq!(e.to_string(), "writeback to 'disk.moof' failed");
        assert!(drv.status().dirty);
        // It is tried again at the next stop of the motor, not before
        drv.write_back(false).unwrap();
    }

    #[test]
    fn rpm_adjustment_only_on_a_drive_with_speed_control() {
        let mut drv = drive(0, DriveType::GCR400K);
        drv.pwm_dutycycle = 50;
        let rpm = drv.get_track_rpm();
        drv.set_rpm_adjustment(10).unwrap();
        assert_eq!(drv.get_track_rpm(), rpm + 10);

        let mut drv = drive(0, DriveType::GCR800K);
        assert!(drv.set_rpm_adjustment(10).is_err());
    }
}
