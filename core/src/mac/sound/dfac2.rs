//! DFAC II, the sound input and output gain and filter chip
//!
//! On the IIC bus at $DE, which the LC 475 ROM uses. The first byte after the
//! address selects a register, and the following ones go to the next registers,
//! which also applies to reads. The registers are stored and have no effect on
//! the sound yet: their meaning isn't known beyond MAME's guesses. The ROM writes
//! $47 to $0C and $41 to $0F before the chime, and 7.5.5 writes $80 to $02 and $08
//! to $0D.

use log::debug;
use serde::{Deserialize, Serialize};

use crate::debuggable::Debuggable;
use crate::mac::iic::IicBus;

/// Address on the IIC bus, with the read flag clear
const ADDRESS: u8 = 0xDE;

const REGS: usize = 16;

#[derive(Default, Serialize, Deserialize)]
pub struct Dfac2 {
    regs: [u8; REGS],
    /// Register of the next access
    reg: u8,
    /// The transaction is addressed to this chip
    selected: bool,
    /// The next byte written selects a register
    want_reg: bool,
}

impl Dfac2 {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn next_reg(&mut self) -> usize {
        let reg = usize::from(self.reg) % REGS;
        self.reg = self.reg.wrapping_add(1);
        reg
    }
}

impl IicBus for Dfac2 {
    fn start(&mut self, address: u8) -> bool {
        self.selected = address & !1 == ADDRESS;
        self.want_reg = self.selected;
        if !self.selected {
            debug!("No IIC device at {:02X}", address);
        }
        self.selected
    }

    fn write(&mut self, val: u8) -> bool {
        if !self.selected {
            return false;
        }
        if self.want_reg {
            self.want_reg = false;
            self.reg = val;
        } else {
            debug!("DFAC II register {:02X} = {:02X}", self.reg, val);
            let reg = self.next_reg();
            self.regs[reg] = val;
        }
        true
    }

    fn read(&mut self) -> u8 {
        if !self.selected {
            return 0xFF;
        }
        let reg = self.next_reg();
        self.regs[reg]
    }
}

impl Debuggable for Dfac2 {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_byte;
        use crate::debuggable::*;

        self.regs
            .iter()
            .enumerate()
            .map(|(i, &val)| dbgprop_byte!(format!("Register {:02X}", i), val))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers() {
        let mut dfac = Dfac2::default();

        assert!(!dfac.start(0x50));
        assert!(!dfac.write(0x00));
        assert_eq!(dfac.read(), 0xFF);

        assert!(dfac.start(0xDE));
        assert!(dfac.write(0x0C));
        assert!(dfac.write(0x47));
        assert!(dfac.write(0x2A));

        // A read also selects the register with the first byte
        assert!(dfac.start(0xDF));
        assert!(dfac.write(0x0C));
        assert_eq!(dfac.read(), 0x47);
        assert_eq!(dfac.read(), 0x2A);
    }
}
