//! VIA implementation for the Macintosh Portable/PowerBook 100

use std::ops::{Deref, DerefMut};

use crate::bus::Address;
use crate::bus::BusMember;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::mac::via_common::{ViaCommon, ViaReg, ViaWiring};
use crate::tickable::{Tickable, Ticks};
use crate::types::Byte;
use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

bitfield! {
    /// VIA Register A
    #[derive(Clone, Copy, Serialize, Deserialize)]
    pub struct RegisterA(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub pmd0: bool @ 0,
        pub pmd1: bool @ 1,
        pub pmd2: bool @ 2,
        pub pmd3: bool @ 3,
        pub pmd4: bool @ 4,
        pub pmd5: bool @ 5,
        pub pmd6: bool @ 6,
        pub pmd7: bool @ 7,
    }
}

bitfield! {
    /// VIA Register B
    #[derive(Clone, Copy, Serialize, Deserialize)]
    pub struct RegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub pmreq: bool @ 0,
        pub pmack: bool @ 1,
        pub test: bool @ 2,
        pub sync: bool @ 3,
        pub drivesel: bool @ 4,
        pub headsel: bool @ 5,
        pub sndext: bool @ 6,
        pub sccwrreq: bool @ 7,
        pub sndenb: bool @ 7,
    }
}

bitfield! {
    /// VIA Interrupt flag/enable registers
    #[derive(Clone, Copy, Serialize, Deserialize)]
    pub struct RegisterIRQ(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub onesec: bool @ 0,
        pub vblank: bool @ 1,
        pub scsi_irq: bool @ 3,
        pub pmgr: bool @ 4,
        pub t2: bool @ 5,
        pub t1: bool @ 6,
        pub irq: bool @ 7,
    }
}

pub struct PortableViaWiring;

impl ViaWiring for PortableViaWiring {
    type RegisterA = RegisterA;
    type RegisterB = RegisterB;
    type RegisterIRQ = RegisterIRQ;
}

#[derive(Serialize, Deserialize)]
pub struct Via {
    core: ViaCommon<PortableViaWiring>,
}

impl Via {
    pub fn new() -> Self {
        Self {
            core: ViaCommon::new(),
        }
    }

    pub fn reset(&mut self) {
        self.core.reset();
        self.core.a_in = RegisterA(0xFF);
        self.core.b_in = RegisterB(0xFF);
    }
}

impl Deref for Via {
    type Target = ViaCommon<PortableViaWiring>;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl DerefMut for Via {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

impl BusMember<Address> for Via {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        let reg = ViaReg::from_addr(addr);

        if reg == ViaReg::B {
            // The SCC write request bit is used for checking if there's data ready
            // from the SCC in critical sections where SCC interrupts are disabled.
            // Reporting 1 signals that there's no data ready.
            self.core.b_in.set_sccwrreq(true);
        }

        Some(self.core.read(reg))
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        self.core.write(ViaReg::from_addr(addr), val);
        Some(())
    }
}

/// Ticked with VIA clock ticks
impl Tickable for Via {
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        self.core.tick_timers(ticks)?;
        Ok(ticks)
    }
}

impl Debuggable for Via {
    fn get_debug_properties(&self) -> DebuggableProperties {
        let mut props = self.core.debug_properties_ports();
        props.extend(self.core.debug_properties_control());
        props
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::via_common::{IRQ_CA1, IRQ_CA2};

    /// Register A with handshake
    const ADDR_A_HS: Address = 0x01 << 9;
    /// Register A without handshake (vBufA)
    const ADDR_A: Address = 0x0F << 9;
    const ADDR_PCR: Address = 0x0C << 9;

    #[test]
    fn reg_a_no_handshake_keeps_irq_flags() {
        let mut via = Via::new();

        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.read(ADDR_A);
        assert!(via.ifr().vblank() && via.ifr().onesec());
        via.write(ADDR_A, 0);
        assert!(via.ifr().vblank() && via.ifr().onesec());
    }

    #[test]
    fn reg_a_handshake_clears_irq_flags() {
        let mut via = Via::new();

        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.read(ADDR_A_HS);
        assert!(!via.ifr().vblank() && !via.ifr().onesec());

        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.write(ADDR_A_HS, 0);
        assert!(!via.ifr().vblank() && !via.ifr().onesec());
    }

    #[test]
    fn independent_ca2_keeps_irq_flag() {
        let mut via = Via::new();

        // CA2 independent interrupt input, negative edge
        via.write(ADDR_PCR, 0x02);
        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.read(ADDR_A_HS);
        assert!(!via.ifr().vblank() && via.ifr().onesec());

        // Positive edge
        via.write(ADDR_PCR, 0x06);
        via.write(ADDR_A_HS, 0);
        assert!(via.ifr().onesec());

        // Handshake output is not independent
        via.write(ADDR_PCR, 0x0A);
        via.read(ADDR_A_HS);
        assert!(!via.ifr().onesec());
    }
}
