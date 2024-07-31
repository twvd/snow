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
    pub sel: bool,

    status: IwmStatus,
    mode: IwmMode,
}

bitfield! {
    /// IWM status register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IwmStatus(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Lower bits of MODE
        pub mode_low: u8 @ 0..=4,

        /// Enable active
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
#[derive(FromPrimitive, Debug)]
enum IwmReg {
    /// Head step direction
    DIRTN = 0b0000,
    /// Disk in place
    CISTN = 0b0001,
    /// Disk head stepping
    STEP = 0b0010,
    /// Disk write protect
    WRTPRT = 0b0011,
    /// Disk motor running
    MOTORON = 0b0100,
    /// Head at track 0
    TKO = 0b0101,
    /// Disk eject
    EJECT = 0b1110,
    /// Tachometer
    TACH = 0b0111,
    /// Read data, low head
    RDDATA0 = 0b1000,
    /// Read data, upper head
    RDDATA1 = 0b1001,
    /// Single/double sided drive
    SIDES = 0b1100,
    /// Drive installed
    DRVIN = 0b1111,
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
            mode: IwmMode(0x1F),
        }
    }

    fn access(&mut self, offset: Address) -> u8 {
        match offset / 512 {
            0 => self.ca0 = false,
            1 => self.ca0 = true,
            2 => self.ca1 = false,
            3 => self.ca1 = true,
            4 => self.ca2 = false,
            5 => self.ca2 = true,
            6 => self.lstrb = false,
            7 => self.lstrb = true,
            8 => {
                println!("motor on");
                self.motor = false; // ?
                self.status.set_enable(false);
            }
            9 => {
                println!("motor on");
                self.motor = true; // ?
                self.status.set_enable(true);
            }
            10 => self.extdrive = false,
            11 => self.extdrive = true,
            12 => self.q6 = false,
            13 => self.q6 = true,
            14 => {
                println!("extdrive = {:?}", self.extdrive);
                let result = self.iwm_read();
                self.q7 = false;

                return result;
            }
            15 => self.q7 = true,
            _ => (),
        };
        0xFF
    }

    fn read_reg(&self) -> bool {
        let reg = self.get_active_reg();
        println!("IWM reg read {:?}", reg);
        match reg {
            IwmReg::CISTN => false,
            IwmReg::SIDES => false,
            IwmReg::MOTORON => true, //self.motor,
            IwmReg::DRVIN if self.extdrive => false,
            _ => true,
        }
    }

    fn iwm_read(&mut self) -> u8 {
        self.status.set_sense(self.read_reg());
        self.status.set_mode_low(self.mode.mode_low());

        match (self.q6, self.q7) {
            // Status
            (true, false) => {
                println!("IWM status read");
                self.status.0
            }
            _ => {
                println!("IWM unknown read");
                self.status.0
            }
        }
    }

    fn get_active_reg(&self) -> IwmReg {
        let mut v = 0;
        if self.ca2 {
            v |= 0b1000
        };
        if self.ca1 {
            v |= 0b0100
        };
        if self.ca0 {
            v |= 0b0010
        };
        if self.sel {
            v |= 0b0001
        };
        IwmReg::from_u8(v).unwrap()
    }
}

impl BusMember<Address> for Iwm {
    fn read(&mut self, addr: Address) -> Option<u8> {
        Some(self.access(addr - 0xDFE1FF))
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        println!("IWM write {:08X} {:02X}", addr, val);
        self.access(addr - 0xDFE1FF);
        Some(())
    }
}
