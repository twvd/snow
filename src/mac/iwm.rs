use std::io::Write;

use anyhow::Result;
use log::*;
use num::clamp;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use strum::Display;

use crate::{
    bus::{Address, BusMember},
    tickable::{Tickable, Ticks, TICKS_PER_SECOND},
    types::LatchingEvent,
};

#[derive(Debug, Default)]
struct Track {
    data: Vec<u8>,
    bits: usize,
}

/// Integrated Woz Machine - floppy drive controller
#[derive(Debug)]
pub struct Iwm {
    double_sided: bool,

    cycles: Ticks,

    pub ca0: bool,
    pub ca1: bool,
    pub ca2: bool,
    pub lstrb: bool,
    pub motor: bool,
    pub q6: bool,
    pub q7: bool,
    pub extdrive: bool,
    pub enable: bool,
    pub sel: bool,

    status: IwmStatus,
    mode: IwmMode,

    disk_inserted: bool,
    track: usize,
    stepdir: HeadStepDirection,
    trackdata: [Track; Self::DISK_TRACKS * Self::DISK_SIDES],
    track_bit: usize,
    track_byte: usize,
    shdata: u8,
    datareg: u8,

    pub dbg_pc: u32,
    pub dbg_break: LatchingEvent,

    // While > 0, the drive head is moving
    stepping: Ticks,
    ejecting: Option<Ticks>,

    pwm_avg_sum: i64,
    pwm_avg_count: usize,
    pwm_dutycycle: Ticks,

    write_shift: u8,
    write_pos: usize,
    write_buffer: Option<u8>,
}

/// Direction the drive head is set to step to
#[derive(PartialEq, Eq, Clone, Copy, Debug, Display)]
enum HeadStepDirection {
    Up,
    Down,
}

bitfield! {
    /// IWM handshake register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IwmHandshake(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Write buffer underrun
        /// 1 = no under-run, 0 = under-run occurred
        pub underrun: bool @ 6,

        /// Register ready for data
        /// 1 = ready, 0 = not ready
        pub ready: bool @ 7,
    }
}

impl Default for IwmHandshake {
    fn default() -> Self {
        Self(0xFF)
    }
}

bitfield! {
    /// IWM status register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IwmStatus(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Lower bits of MODE
        pub mode_low: u8 @ 0..=4,

        /// Enable active
        /// Enable means: disk locked, drive light on, drive ready
        /// Does not mean motor active
        /// Follows the 'ENABLE' I/O
        pub enable: bool @ 5,

        /// MZ (always 0)
        pub mz: bool @ 6,

        /// SENSE (current register read)
        pub sense: bool @ 7,
    }
}

bitfield! {
    /// IWM mode
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IwmMode(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Full MODE register
        pub mode: u8 @ 0..=7,

        /// Lower bits of MODE (to write to status)
        pub mode_low: u8 @ 0..=4,

        /// Latch mode (1)
        pub latch: bool @ 0,

        /// 0 = synchronous, 1 = asynchronous
        pub sync: bool @ 1,

        /// One-sec output enabled (0)
        pub onesec: bool @ 2,

        /// 0 = slow mode, 1 = fast mode
        pub fast: bool @ 3,

        /// Clock speed (0 = 7 MHz, 1 = 8 MHz)
        pub speed: bool @ 4,

        /// Test mode
        pub test: bool @ 5,

        /// MZ-Reset
        pub mzreset: bool @ 6,

        /// Reserved
        pub reserved: bool @ 7,
    }
}

/// IWM registers
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

/// IWM write registers
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

impl Iwm {
    /// Amount of tracks per disk side
    const DISK_TRACKS: usize = 80;

    /// Amount of sides
    const DISK_SIDES: usize = 2;

    /// Tacho pulses/disk revolution
    const TACHO_SPEED: Ticks = 60;

    pub fn new(double_sided: bool) -> Self {
        Self {
            cycles: 0,

            ca0: false,
            ca1: false,
            ca2: false,
            lstrb: false,
            motor: false,
            q6: false,
            q7: false,
            extdrive: false,
            sel: false,

            status: IwmStatus(0),
            mode: IwmMode(0),

            disk_inserted: false,
            track: 4,
            stepdir: HeadStepDirection::Up,
            trackdata: core::array::from_fn(|_| Track::default()),
            track_bit: 0,
            track_byte: 0,
            shdata: 0,
            datareg: 0,

            enable: false,
            dbg_pc: 0,
            dbg_break: LatchingEvent::default(),
            stepping: 0,
            ejecting: None,

            double_sided,
            pwm_avg_sum: 0,
            pwm_avg_count: 0,
            pwm_dutycycle: 0,

            write_shift: 0,
            write_pos: 0,
            write_buffer: None,
        }
    }

    /// A memory-mapped I/O address was accessed (offset from IWM base address)
    fn access(&mut self, offset: Address) {
        match offset / 512 {
            0 => self.ca0 = false,
            1 => self.ca0 = true,
            2 => self.ca1 = false,
            3 => self.ca1 = true,
            4 => self.ca2 = false,
            5 => self.ca2 = true,
            6 => self.lstrb = false,
            7 => {
                self.lstrb = true;
                self.write_drive_reg();
            }
            8 => {
                self.enable = false;
            }
            9 => {
                self.enable = true;
            }
            10 => self.extdrive = false,
            11 => self.extdrive = true,
            12 => self.q6 = false,
            13 => self.q6 = true,
            14 => {
                self.q7 = false;
            }
            15 => self.q7 = true,
            _ => (),
        }
    }

    fn dump_tracks(&self) -> Result<()> {
        let mut f = std::fs::File::create("write.bin")?;
        for t in 0..(Self::DISK_TRACKS * Self::DISK_SIDES) {
            f.write_all(&(self.trackdata[t].data.len() as u32).to_le_bytes())?;
            f.write_all(&(self.trackdata[t].data.len() as u32).to_le_bytes())?;
            f.write_all(&self.trackdata[t].data)?;
        }
        Ok(())
    }

    /// Reads from the currently selected drive register
    fn read_drive_reg(&self) -> bool {
        let reg = DriveReg::from_u8(self.get_selected_drive_reg_u8()).unwrap_or(DriveReg::UNKNOWN);

        //if reg != IwmReg::TACH {
        //trace!(
        //"{:08X} IWM reg read {:?} = {} ext = {}",
        //self.dbg_pc,
        //reg,
        //res,
        //self.extdrive
        //);
        //}

        match reg {
            DriveReg::CISTN => !self.disk_inserted,
            DriveReg::DIRTN => self.stepdir == HeadStepDirection::Down,
            DriveReg::SIDES => self.double_sided,
            DriveReg::MOTORON => !(self.motor && self.disk_inserted),
            DriveReg::PRESENT if self.extdrive => true,
            DriveReg::PRESENT => false,
            DriveReg::INSTALLED if self.extdrive => true,
            DriveReg::INSTALLED => false,
            DriveReg::READY => false,
            DriveReg::TKO if self.track == 0 => false,
            DriveReg::TKO => true,
            DriveReg::STEP => self.stepping == 0,
            DriveReg::TACH => self.get_tacho(),
            DriveReg::RDDATA0 => self.get_head_bit(0),
            DriveReg::RDDATA1 => self.get_head_bit(1),
            DriveReg::WRTPRT => true,
            _ => {
                warn!(
                    "Unimplemented register read {:?} {:0b}",
                    reg,
                    self.get_selected_drive_reg_u8()
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
                    error!("Disk drive head moving further than track 79!");
                } else {
                    self.track += 1;
                }
            }
            HeadStepDirection::Down => {
                if self.track == 0 {
                    error!("Disk drive head moving lower than track 0");
                } else {
                    self.track -= 1;
                }
            }
        }

        // Reset track position
        self.track_byte = 0;

        // Track-to-track stepping time: 30ms
        self.stepping = TICKS_PER_SECOND / 60_000 * 30;

        //trace!(
        //"Track {}, now: {} - byte len: {} bit len: {} rpm: {} cycles/bit: {}",
        //self.stepdir,
        //self.track,
        //self.trackdata[self.track].data.len(),
        //self.trackdata[self.track].bits,
        //self.get_track_rpm(),
        //self.get_cycles_per_bit(),
        //);
    }

    /// Writes to the currently selected drive register
    fn write_drive_reg(&mut self) {
        let reg = DriveWriteReg::from_u8(self.get_selected_drive_reg_u8())
            .unwrap_or(DriveWriteReg::UNKNOWN);
        match reg {
            DriveWriteReg::MOTORON => self.motor = true,
            DriveWriteReg::MOTOROFF => {
                self.motor = false;
            }
            DriveWriteReg::EJECT => {
                if self.disk_inserted {
                    self.ejecting = Some(self.cycles + (TICKS_PER_SECOND / 2));
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
                warn!(
                    "Unimplemented register write {:?} {:0b}",
                    reg,
                    self.get_selected_drive_reg_u8()
                );
            }
        }
    }

    /// Reads the currently selected (Q6, Q7) IWM register
    fn iwm_read(&mut self) -> u8 {
        match (self.q6, self.q7) {
            (false, false) => {
                // Data register
                if !self.enable {
                    0xFF
                } else {
                    std::mem::replace(&mut self.datareg, 0)
                }
            }
            (true, false) => {
                // Read status register
                let sense = self.read_drive_reg();
                self.status.set_sense(sense);
                self.status.set_mode_low(self.mode.mode_low());
                self.status.set_enable(self.enable);

                self.status.0
            }
            (false, true) => {
                // Read handshake register
                let mut result = IwmHandshake::default();
                result.set_underrun(!(self.write_pos == 0 && self.write_buffer.is_none()));
                result.set_ready(self.write_buffer.is_none());
                result.0
            }
            _ => {
                warn!("IWM unknown read q6 = {:?} q7 = {:?}", self.q6, self.q7);
                0
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

    /// Inserts a disk into the disk drive
    pub fn disk_insert(&mut self, data: &[u8]) {
        // Parse track data
        // TODO use iterator, make fancy
        let mut offset = 0;
        let mut last = 0;
        for tracknum in 0.. {
            let bytes = u32::from_le_bytes(data[offset..(offset + 4)].try_into().unwrap()) as usize;
            let bits =
                u32::from_le_bytes(data[(offset + 4)..(offset + 8)].try_into().unwrap()) as usize;
            assert_eq!(bytes, bits);

            let track = Track {
                bits,
                data: Vec::from(&data[(offset + 8)..(offset + 8 + bytes)]),
            };
            self.trackdata[tracknum] = track;
            offset += 8 + bytes;

            if data.len() <= offset {
                last = tracknum;
                break;
            }
        }

        info!("Disk inserted, {} tracks", last + 1);
        self.disk_inserted = true;
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
    pub const fn get_ticks_per_bit(&self) -> Ticks {
        if self.get_track_rpm() == 0 || !self.disk_inserted {
            return Ticks::MAX;
        }
        ((TICKS_PER_SECOND * 60) / self.get_track_rpm() / self.trackdata[self.track].bits) + 1
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

    /// Gets the active selected track offset, offset by the active selected head for double-sided disks
    fn get_active_track(&self) -> usize {
        if self.get_active_head() == 1 && self.trackdata[self.track + 80].bits != 0 {
            self.track + 80
        } else {
            self.track
        }
    }

    /// Gets the active (selected) drive head
    fn get_active_head(&self) -> usize {
        if !self.double_sided || !self.sel {
            0
        } else {
            1
        }
    }

    /// Gets the length (in bits) of a track
    fn get_track_len(&self, track: usize) -> usize {
        self.trackdata[track].bits
    }

    /// Gets the physical disk bit currently under a head
    fn get_head_bit(&self, head: usize) -> bool {
        debug_assert!(head < Self::DISK_SIDES);

        let track_offset =
            if self.double_sided && head == 1 && self.trackdata[self.track + 80].bits != 0 {
                self.track + 80
            } else {
                self.track
            };

        let res = self.trackdata[track_offset].data[self.track_byte];
        assert!([0, 1].contains(&res));

        res != 0
    }

    /// Update current drive PWM signal from the sound buffer
    pub fn push_pwm(&mut self, pwm: u8) -> Result<()> {
        const VALUE_TO_LEN: [u8; 64] = [
            0, 1, 59, 2, 60, 40, 54, 3, 61, 32, 49, 41, 55, 19, 35, 4, 62, 52, 30, 33, 50, 12, 14,
            42, 56, 16, 27, 20, 36, 23, 44, 5, 63, 58, 39, 53, 31, 48, 18, 34, 51, 29, 11, 13, 15,
            26, 22, 43, 57, 38, 47, 17, 28, 10, 25, 21, 37, 46, 9, 24, 45, 8, 7, 6,
        ];

        if self.double_sided {
            // Only single-sided drives are PWM controlled
            return Ok(());
        }

        self.pwm_avg_sum += VALUE_TO_LEN[usize::from(pwm) % VALUE_TO_LEN.len()] as i64;
        self.pwm_avg_count += 1;
        if self.pwm_avg_count >= 100 {
            let idx = clamp(
                self.pwm_avg_sum / (self.pwm_avg_count as i64 / 10) - 11,
                0,
                399,
            );
            self.pwm_dutycycle = ((idx * 100) / 419).try_into()?;
            self.pwm_avg_sum = 0;
            self.pwm_avg_count = 0;
        }
        Ok(())
    }
}

impl BusMember<Address> for Iwm {
    fn read(&mut self, addr: Address) -> Option<u8> {
        // Only the lower 8-bits of the databus are connected to IWM.
        // Assume the upper 8 bits are undefined.
        if addr & 1 == 0 {
            return None;
        }

        self.access(addr - 0xDFE1FF);
        let result = self.iwm_read();
        Some(result)
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        // UDS/LDS are not connected to IWM, so ignore the lower address bit here.
        self.access((addr | 1) - 0xDFE1FF);

        match (self.q6, self.q7, self.enable) {
            (true, true, false) => {
                // Write MODE
                if val != 0x1F {
                    warn!("Non-standard IWM mode: {:02X}", val);
                }
                self.mode.set_mode(val);
            }
            (true, true, true) => {
                // Write data register
                trace!(
                    "Write data reg: {:02X}, bit position: {}, track: {}, head: {}",
                    val,
                    self.track_byte,
                    self.get_active_track(),
                    self.get_active_head()
                );
                if self.write_buffer.is_some() {
                    warn!("Disk write while write buffer not empty");
                }
                self.write_buffer = Some(val);
            }
            _ => (),
        }

        Some(())
    }
}

impl Tickable for Iwm {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        debug_assert_eq!(ticks, 1);

        // This is called at the Macintosh main clock speed (TICKS_PER_SECOND == 8 MHz)
        self.cycles += ticks;

        // When an EJECT command is sent, do not actually eject the disk until eject strobe has been
        // asserted for at least 500ms. Specifications say a 750ms strobe is required.
        // For some reason, the Mac Plus ROM gives a very short eject strobe on bootup during drive
        // enumeration. If we do not ignore that, the Mac Plus always ejects the boot disk.
        if self.ejecting.is_some() && self.lstrb {
            let Some(eject_ticks) = self.ejecting else {
                unreachable!()
            };
            if eject_ticks < self.cycles {
                info!("Disk ejected");
                self.disk_inserted = false;
                self.ejecting = None;
            }
        } else if !self.lstrb {
            self.ejecting = None;
        }

        // Decrement 'head stepping' timer
        self.stepping = self.stepping.saturating_sub(ticks);

        if self.disk_inserted && self.motor && self.cycles % self.get_ticks_per_bit() == 0 {
            // Progress the head over the track
            self.shdata <<= 1;
            if self.get_head_bit(self.get_active_head()) {
                self.shdata |= 1;
            }

            self.track_byte += 1;
            self.track_bit = 0;

            if self.track_byte >= self.get_track_len(self.get_active_track()) {
                // Back to start of track
                //trace!(
                //    "Track at start - track {} len {} rpm {} cyc/bit {} duty {}% head {}",
                //    self.track,
                //    self.trackdata[self.track].bits,
                //    self.get_track_rpm(),
                //    self.get_cycles_per_bit(),
                //    self.pwm_dutycycle,
                //    self.sel,
                //);
                self.track_byte = 0;
                self.track_bit = 0;
            }

            if self.shdata & 0x80 != 0 {
                // Data is moved to the data register when the most significant bit is set.
                // Because the Mac uses GCR encoding, the most significant bit is always set in
                // any valid data.
                self.datareg = self.shdata;
                self.shdata = 0;
            }

            if self.write_pos == 0 && self.write_buffer.is_some() {
                // Write idle and new data in write FIFO, start writing 8 new bits
                let Some(v) = self.write_buffer else {
                    unreachable!()
                };
                self.write_shift = v;
                self.write_pos = 8;
                self.write_buffer = None;
            }
            if self.write_pos > 0 {
                // Write in progress - write one bit to current head location
                let bit = self.write_shift & 0x80 != 0;
                self.write_shift <<= 1;
                self.write_pos -= 1;
                self.trackdata[self.get_active_track()].data[self.track_byte] =
                    if bit { 1 } else { 0 };

                if self.write_pos == 0 && self.write_buffer.is_none() {
                    self.dump_tracks()?;
                }
            }
        }

        Ok(ticks)
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
        let mut iwm = Iwm::new(true);
        iwm.disk_inserted = true;
        iwm.motor = true;
        iwm.track = 0;

        let mut last = false;
        let mut result = 0;

        for _ in 0..(TICKS_PER_SECOND * 60) {
            iwm.cycles += 1;
            if iwm.get_tacho() != last {
                result += 1;
                last = iwm.get_tacho();
            }
        }

        assert_eq!(result / 10, DISK_RPM_OUTER * 120 / 10);
    }

    #[test]
    fn disk_double_tacho_inner() {
        let mut iwm = Iwm::new(true);
        iwm.disk_inserted = true;
        iwm.motor = true;
        iwm.track = 79;

        let mut last = false;
        let mut result = 0;

        for _ in 0..(TICKS_PER_SECOND * 60) {
            iwm.cycles += 1;
            if iwm.get_tacho() != last {
                result += 1;
                last = iwm.get_tacho();
            }
        }

        // Roughly is good enough..
        assert_eq!(result / 10, DISK_RPM_INNER * 120 / 10);
    }
}
