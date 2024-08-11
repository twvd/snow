use anyhow::Result;
use log::*;
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

/// Integrated Woz Machine
#[derive(Debug)]
pub struct Iwm {
    cycles: Ticks,

    ca0: bool,
    ca1: bool,
    ca2: bool,
    lstrb: bool,
    motor: bool,
    q6: bool,
    q7: bool,
    extdrive: bool,
    enable: bool,
    pub sel: bool,

    status: IwmStatus,
    mode: IwmMode,

    disk_inserted: bool,
    track: usize,
    stepdir: StepDirection,
    data_ready: bool,

    pub dbg_pc: u32,
    pub dbg_break: LatchingEvent,
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
    /// Disk revolutions/minute at outer track (0)
    const DISK_RPM_OUTER: Ticks = 390;

    /// Disk revolutions/minute at inner track (79)
    const DISK_RPM_INNER: Ticks = 605;

    /// Amount of tracks per disk side
    const DISK_TRACKS: usize = 80;

    /// Tacho pulses/disk revolution
    const TACHO_SPEED: Ticks = 60;

    pub fn new() -> Self {
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

            enable: false,
            dbg_pc: 0,
            dbg_break: LatchingEvent::default(),

            data_ready: false,
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

    fn read_reg(&mut self) -> bool {
        let reg = IwmReg::from_u8(self.get_selected_reg()).unwrap_or(IwmReg::UNKNOWN);
        let res = match reg {
            IwmReg::CISTN => !self.disk_inserted,
            IwmReg::DIRTN => self.stepdir == StepDirection::Down,
            IwmReg::SIDES => true,
            IwmReg::MOTORON => !(self.motor && self.disk_inserted),
            IwmReg::DRVIN if self.extdrive => true,
            IwmReg::DRVIN => false, // internal drive installed
            IwmReg::DUNNO if self.extdrive => true,
            IwmReg::DUNNO => false,
            IwmReg::READY => false,
            IwmReg::TKO if self.track == 0 => false,
            IwmReg::TKO => true,
            IwmReg::TACH => self.get_tacho(),
            _ => {
                warn!(
                    "Unimplemented register read {:?} {:0b}",
                    reg,
                    self.get_selected_reg()
                );
                true
            }
        };

        if reg != IwmReg::TACH {
            trace!(
                "{:08X} IWM reg read {:?} = {} ext = {}",
                self.dbg_pc,
                reg,
                res,
                self.extdrive
            );
        }

        res
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
        trace!("Track {}, now: {}", self.stepdir, self.track);
    }

    fn write_reg(&mut self) {
        let reg = IwmWriteReg::from_u8(self.get_selected_reg()).unwrap_or(IwmWriteReg::UNKNOWN);
        trace!("IWM reg write {:?}", reg);
        match reg {
            IwmWriteReg::MOTORON => self.motor = true,
            IwmWriteReg::MOTOROFF => {
                self.motor = false;
            }
            IwmWriteReg::EJECT => {
                //self.dbg_break.set();
                self.disk_inserted = false;
            }
            IwmWriteReg::TRACKUP => {
                self.stepdir = StepDirection::Up;
                self.step_head();
            }
            IwmWriteReg::TRACKDN => {
                self.stepdir = StepDirection::Down;
                self.step_head();
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
                //trace!(
                //    "IWM data reg read, tacho = {}, track = {}",
                //    self.tach,
                //    self.track
                //);
                if !self.enable {
                    0xFF
                } else {
                    // TODO actual data register
                    if self.data_ready {
                        self.data_ready = false;
                        0x88
                    } else {
                        0
                    }
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
        info!("Disk inserted");
        self.disk_inserted = true;
    }

    pub const fn get_track_rpm(&self) -> Ticks {
        (((Self::DISK_RPM_INNER - Self::DISK_RPM_OUTER) * self.track) / (Self::DISK_TRACKS - 1))
            + Self::DISK_RPM_OUTER
    }

    pub const fn get_tacho(&self) -> bool {
        if !self.motor {
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
}

impl BusMember<Address> for Iwm {
    fn read(&mut self, addr: Address) -> Option<u8> {
        self.access(addr - 0xDFE1FF);
        let result = self.iwm_read();
        //trace!("IWM read: {:08X} = {:02X}", addr, result);
        Some(result)
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        //trace!("IWM write {:08X} {:02X}", addr, val);
        self.access(addr - 0xDFE1FF);

        match (self.q6, self.q7, self.enable) {
            (true, true, false) => {
                // Write MODE
                self.mode.set_mode(val);
                trace!("IWM mode write: {:02X}", self.mode.mode());
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

        if self.cycles % (self.get_track_rpm() / 12) == 0 && self.motor {
            self.data_ready = true;
        }

        Ok(ticks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_tacho_outer() {
        let mut iwm = Iwm::new();
        iwm.disk_inserted = true;
        iwm.motor = true;
        iwm.track = 0;

        assert_eq!(iwm.get_track_rpm(), Iwm::DISK_RPM_OUTER);

        let mut last = false;
        let mut result = 0;

        for _ in 0..(TICKS_PER_SECOND * 60) {
            iwm.tick(1).unwrap();
            if iwm.get_tacho() != last {
                result += 1;
                last = iwm.get_tacho();
            }
        }

        assert_eq!(result / 10, Iwm::DISK_RPM_OUTER * 120 / 10);
    }

    #[test]
    fn disk_tacho_inner() {
        let mut iwm = Iwm::new();
        iwm.disk_inserted = true;
        iwm.motor = true;
        iwm.track = 79;

        assert_eq!(iwm.get_track_rpm(), Iwm::DISK_RPM_INNER);

        let mut last = false;
        let mut result = 0;

        for _ in 0..(TICKS_PER_SECOND * 60) {
            iwm.tick(1).unwrap();
            if iwm.get_tacho() != last {
                result += 1;
                last = iwm.get_tacho();
            }
        }

        // Roughly is good enough..
        assert_eq!(result / 10, Iwm::DISK_RPM_INNER * 120 / 10);
    }
}
