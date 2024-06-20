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
