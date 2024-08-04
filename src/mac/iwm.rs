use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};

/// Integrated Woz Machine
#[derive(Debug)]
pub struct Iwm {
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

    pub dbg_pc: u32,
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
#[derive(FromPrimitive, Debug)]
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
    pub fn new() -> Self {
        Self {
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

            disk_inserted: true,
            enable: false,
            dbg_pc: 0,
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
                println!("IWM drive disable ext = {}", self.extdrive);
            }
            9 => {
                self.enable = true;
                println!("IWM drive enable ext = {}", self.extdrive);
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

    fn read_reg(&self) -> bool {
        let reg = IwmReg::from_u8(self.get_selected_reg()).unwrap_or(IwmReg::UNKNOWN);
        let res = match reg {
            IwmReg::CISTN => !self.disk_inserted,
            IwmReg::SIDES => true,
            IwmReg::MOTORON => !(self.motor && self.disk_inserted),
            IwmReg::DRVIN if self.extdrive => true,
            IwmReg::DRVIN => false, // internal drive installed
            IwmReg::DUNNO if self.extdrive => true,
            IwmReg::DUNNO => false,
            IwmReg::READY => false,
            _ => {
                println!(
                    "Unimplemented register read {:?} {:0b}",
                    reg,
                    self.get_selected_reg()
                );
                true
            }
        };

        println!(
            "{:08X} IWM reg read {:?} = {} ext = {}",
            self.dbg_pc, reg, res, self.extdrive
        );

        res
    }

    fn write_reg(&mut self) {
        let reg = IwmWriteReg::from_u8(self.get_selected_reg()).unwrap_or(IwmWriteReg::UNKNOWN);
        println!("IWM reg write {:?}", reg);
        match reg {
            IwmWriteReg::MOTORON => self.motor = true,
            IwmWriteReg::MOTOROFF => self.motor = false,
            IwmWriteReg::EJECT => self.disk_inserted = false,
            _ => {
                println!(
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
                    // TODO actual data register
                    self.dbg_pc as u8
                }
            }
            // Status
            (true, false) => {
                println!("IWM status read");
                let sense = self.read_reg();
                self.status.set_sense(sense);
                self.status.set_mode_low(self.mode.mode_low());
                self.status.set_enable(self.enable);

                self.status.0
            }
            (false, true) => {
                println!("IWM handshake read");
                0xFF
            }
            _ => {
                println!("IWM unknown read q6 = {:?} q7 = {:?}", self.q6, self.q7);
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
        println!("Disk inserted");
        self.disk_inserted = true;
    }
}

impl BusMember<Address> for Iwm {
    fn read(&mut self, addr: Address) -> Option<u8> {
        self.access(addr - 0xDFE1FF);
        let result = self.iwm_read();
        println!("IWM read: {:08X} = {:02X}", addr, result);
        Some(result)
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        println!("IWM write {:08X} {:02X}", addr, val);
        self.access(addr - 0xDFE1FF);

        match (self.q6, self.q7, self.enable) {
            (true, true, false) => {
                // Write MODE
                self.mode.set_mode(val);
                println!("IWM mode write: {:02X}", self.mode.mode());
            }
            _ => (),
        }

        Some(())
    }
}
