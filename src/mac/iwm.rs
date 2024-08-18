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
    stepdir: StepDirection,
    trackdata: [Track; 80],
    track_bit: usize,
    track_byte: usize,
    shdata: u8,
    datareg: u8,

    pub dbg_pc: u32,
    pub dbg_break: LatchingEvent,

    // While > 0, the drive head is moving
    stepping: Ticks,

    pwm_avg_sum: i64,
    pwm_avg_count: usize,
    pwm_dutycycle: Ticks,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Display)]
enum StepDirection {
    Up,
    Down,
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
enum IwmReg {
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

    DUNNO = 0b1110,
    /// Drive installed
    /// 0 = drive connected, 1 = no drive connected
    DRVIN = 0b1111,

    /// For unknown values
    UNKNOWN,
}

/// IWM write registers
/// Value bits: CA2 CA1 CA0 SEL
#[allow(clippy::upper_case_acronyms)]
#[derive(FromPrimitive, Debug)]
enum IwmWriteReg {
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
            stepdir: StepDirection::Up,
            trackdata: core::array::from_fn(|_| Track::default()),
            track_bit: 0,
            track_byte: 0,
            shdata: 0,
            datareg: 0,

            enable: false,
            dbg_pc: 0,
            dbg_break: LatchingEvent::default(),
            stepping: 0,

            double_sided,
            pwm_avg_sum: 0,
            pwm_avg_count: 0,
            pwm_dutycycle: 0,
        }
    }

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
                self.write_reg();
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

    #[allow(clippy::needless_pass_by_ref_mut)]
    fn read_reg(&mut self) -> bool {
        let reg = IwmReg::from_u8(self.get_selected_reg()).unwrap_or(IwmReg::UNKNOWN);

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
            IwmReg::CISTN => !self.disk_inserted,
            IwmReg::DIRTN => self.stepdir == StepDirection::Down,
            IwmReg::SIDES => self.double_sided,
            IwmReg::MOTORON => !(self.motor && self.disk_inserted),
            IwmReg::DRVIN if self.extdrive => true,
            IwmReg::DRVIN => false, // internal drive installed
            IwmReg::DUNNO if self.extdrive => true,
            IwmReg::DUNNO => false,
            IwmReg::READY => false,
            IwmReg::TKO if self.track == 0 => false,
            IwmReg::TKO => true,
            IwmReg::STEP => self.stepping == 0,
            IwmReg::TACH => self.get_tacho(),
            IwmReg::RDDATA0 => self.get_head_bit(),
            IwmReg::WRTPRT => false,
            _ => {
                warn!(
                    "Unimplemented register read {:?} {:0b}",
                    reg,
                    self.get_selected_reg()
                );
                true
            }
        }
    }

    fn step_head(&mut self) {
        match self.stepdir {
            StepDirection::Up => {
                self.track += 1;
            }
            StepDirection::Down => {
                self.track -= 1;
            }
        }
        //trace!(
        //"Track {}, now: {} - byte len: {} bit len: {} rpm: {} cycles/bit: {}",
        //self.stepdir,
        //self.track,
        //self.trackdata[self.track].data.len(),
        //self.trackdata[self.track].bits,
        //self.get_track_rpm(),
        //self.get_cycles_per_bit(),
        //);

        // Track-to-track stepping time: 30ms
        self.stepping = TICKS_PER_SECOND / 60_000 * 30;
    }

    fn write_reg(&mut self) {
        let reg = IwmWriteReg::from_u8(self.get_selected_reg()).unwrap_or(IwmWriteReg::UNKNOWN);
        match reg {
            IwmWriteReg::MOTORON => self.motor = true,
            IwmWriteReg::MOTOROFF => {
                self.motor = false;
            }
            IwmWriteReg::EJECT => {
                info!("Disk ejected");
                self.disk_inserted = false;
            }
            IwmWriteReg::TRACKUP => {
                self.stepdir = StepDirection::Up;
            }
            IwmWriteReg::TRACKDN => {
                self.stepdir = StepDirection::Down;
            }
            IwmWriteReg::TRACKSTEP => self.step_head(),
            _ => {
                warn!(
                    "Unimplemented register write {:?} {:0b}",
                    reg,
                    self.get_selected_reg()
                );
            }
        }
    }

    fn iwm_read(&mut self) -> u8 {
        match (self.q6, self.q7) {
            // Data register
            (false, false) => {
                if !self.enable {
                    0xFF
                } else {
                    std::mem::replace(&mut self.datareg, 0)
                }
            }
            // Status
            (true, false) => {
                //trace!("IWM status read");
                let sense = self.read_reg();
                self.status.set_sense(sense);
                self.status.set_mode_low(self.mode.mode_low());
                self.status.set_enable(self.enable);

                self.status.0
            }
            (false, true) => {
                trace!("IWM handshake read");
                0xFF
            }
            _ => {
                warn!("IWM unknown read q6 = {:?} q7 = {:?}", self.q6, self.q7);
                0
            }
        }
    }

    fn get_selected_reg(&self) -> u8 {
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

    pub fn disk_insert(&mut self, _data: &[u8]) {
        // Parse track data
        // TODO use iterator, make fancy
        let data = std::fs::read("disk.bin").unwrap();
        let mut offset = 0;
        for tracknum in 0..Self::DISK_TRACKS {
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
        }

        info!("Disk inserted");
        self.disk_inserted = true;
    }

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

    pub const fn get_cycles_per_bit(&self) -> Ticks {
        if self.get_track_rpm() == 0 {
            return Ticks::MAX;
        }
        ((TICKS_PER_SECOND * 60) / self.get_track_rpm() / self.trackdata[self.track].bits) + 1
    }

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

    fn get_head_bit(&self) -> bool {
        let res = self.trackdata[self.track].data[self.track_byte];
        assert!([0, 1].contains(&res));

        res != 0
    }

    pub fn push_pwm(&mut self, pwm: u8) -> Result<()> {
        const VALUE_TO_LEN: [u8; 64] = [
            0, 1, 59, 2, 60, 40, 54, 3, 61, 32, 49, 41, 55, 19, 35, 4, 62, 52, 30, 33, 50, 12, 14,
            42, 56, 16, 27, 20, 36, 23, 44, 5, 63, 58, 39, 53, 31, 48, 18, 34, 51, 29, 11, 13, 15,
            26, 22, 43, 57, 38, 47, 17, 28, 10, 25, 21, 37, 46, 9, 24, 45, 8, 7, 6,
        ];

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
        if addr & 1 == 0 {
            return None;
        }

        self.access(addr - 0xDFE1FF);
        let result = self.iwm_read();
        Some(result)
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        if addr & 1 == 0 {
            return None;
        }

        self.access(addr - 0xDFE1FF);

        match (self.q6, self.q7, self.enable) {
            (true, true, false) => {
                // Write MODE
                self.mode.set_mode(val);
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

        self.stepping = self.stepping.saturating_sub(ticks);

        if self.disk_inserted && self.motor && self.cycles % self.get_cycles_per_bit() == 0 {
            self.shdata <<= 1;
            if self.get_head_bit() {
                self.shdata |= 1;
            }

            self.track_byte += 1;
            self.track_bit = 0;
            if self.track_byte >= self.trackdata[self.track].bits {
                //trace!(
                //    "Track at start - track {} len {} rpm {} cyc/bit {} duty {}%",
                //    self.track,
                //    self.trackdata[self.track].bits,
                //    self.get_track_rpm(),
                //    self.get_cycles_per_bit(),
                //    self.pwm_dutycycle
                //);
                self.track_byte = 0;
                self.track_bit = 0;
            }

            if self.shdata & 0x80 != 0 {
                self.datareg = self.shdata;
                self.shdata = 0;
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
