use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::bus::Address;

use std::fmt;

bitfield! {
    /// SR register bitfield
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterSR(pub u16): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Carry
        pub c: bool @ 0,
        /// Overflow
        pub v: bool @ 1,
        /// Zero
        pub z: bool @ 2,
        /// Negative
        pub n: bool @ 3,
        /// Extend
        pub x: bool @ 4,

        /// Interrupt priority mask
        pub int_prio_mask: u8 @ 8..=10,

        /// Supervisor mode
        pub supervisor: bool @ 13,

        /// Trace mode
        pub trace: bool @ 15,
    }
}

/// Full Motorola 680x0 register file
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct RegisterFile {
    /// Dx
    pub d: [u32; 8],

    /// Ax
    pub a: [u32; 7],

    /// User Stack Pointer
    pub usp: Address,

    /// Supervisor Stack Pointer
    pub ssp: Address,

    /// Status Register
    pub sr: RegisterSR,

    /// Program counter
    pub pc: Address,
}

impl RegisterFile {
    pub fn new() -> Self {
        Self {
            a: [0; 7],
            d: [0; 8],
            usp: 0,
            ssp: 0,
            sr: RegisterSR(0),
            pc: 0,
        }
    }

    pub fn read_a(&self, a: usize) -> u32 {
        if a == 7 {
            if self.sr.supervisor() {
                self.ssp
            } else {
                self.usp
            }
        } else {
            self.a[a]
        }
    }

    pub fn write_a(&mut self, a: usize, val: u32) {
        if a == 7 {
            if self.sr.supervisor() {
                self.ssp = val
            } else {
                self.usp = val
            }
        } else {
            self.a[a] = val
        }
    }

    pub fn read_d(&self, d: usize) -> u32 {
        self.d[d]
    }

    pub fn write_d(&mut self, d: usize, val: u32) {
        self.d[d] = val
    }
}

impl fmt::Display for RegisterFile {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "A: {:?} D: {:?} USP: {:06X} SSP: {:06X} PC: {:06X} SR: {:?}",
            self.a, self.d, self.usp, self.ssp, self.pc, self.sr
        )
    }
}
