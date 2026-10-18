use std::ops::{Deref, DerefMut};

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::mac::MacModel;
use crate::mac::via_common::{IRQ_CA2, IRQ_CB2, LineState, ViaCommon, ViaLine, ViaReg, ViaWiring};
use crate::tickable::{TickConverter, Tickable, Ticks};
use crate::types::Byte;

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

bitfield! {
    /// VIA Register A
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterA(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Expansion slot $9 IRQ
        pub v2irq1: bool @ 0,
        /// Expansion slot $A IRQ
        pub v2irq2: bool @ 1,
        /// Expansion slot $B IRQ
        pub v2irq3: bool @ 2,
        /// Expansion slot $C IRQ
        pub v2irq4: bool @ 3,
        /// Expansion slot $D IRQ
        pub v2irq5: bool @ 4,
        /// Expansion slot $E IRQ
        pub v2irq6: bool @ 5,

        pub v2irqs: u8 @ 0..=5,

        /// RAM-size (output)
        pub v2ram0: u8 @ 6..=7,
    }
}

bitfield! {
    /// VIA Register B
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// 0 = Disable CPU instruction/data cache
        pub v2cdis: bool @ 0,

        /// 0 = NuBus transactions locked out
        pub v2buslk: bool @ 1,

        /// 0 = power off
        pub v2poweroff: bool @ 2,

        /// PMMU/AMU control
        /// For AMU: 0 = 24-bit
        pub vfc3: bool @ 3,

        /// NuBus transfer acknowledge
        pub v2tmxa: u8 @ 4..=5,

        pub v2sndext: bool @ 6,
        pub v2vbl: bool @ 7,

    }
}

bitfield! {
    /// VIA Interrupt flag/enable registers
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterIRQ(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Cleared on Register A read/write
        pub scsi_drq: bool @ 0,

        /// Cleared on Register A read/write
        pub slot: bool @ 1,

        /// Keyboard data ready
        /// Cleared on read/write shift reg
        pub kbdready: bool @ 2,

        /// Cleared on Register B read/write
        pub scsi_irq: bool @ 3,

        /// Cleared on Register B read/write
        pub asc: bool @ 4,

        /// Timer T2
        /// Cleared on read of T2 counter LSB or write of T2 counter MSB
        pub t2: bool @ 5,

        /// Timer T1
        /// Cleared on read of T1 counter LSB or write of T1 counter MSB
        pub t1: bool @ 6,
    }
}

pub struct Via2Wiring;

impl ViaWiring for Via2Wiring {
    type RegisterA = RegisterA;
    type RegisterB = RegisterB;
    type RegisterIRQ = RegisterIRQ;
}

/// Synertek SY6522 Versatile Interface Adapter
#[derive(Serialize, Deserialize)]
pub struct Via2 {
    core: ViaCommon<Via2Wiring>,

    /// Runs the VIA at the speed of the bus of the real hardware, whatever
    /// the bus frequency is: the VIA is used as a timer
    bus_tick_converter: TickConverter,
}

impl Via2 {
    pub fn new(model: MacModel, bus_frequency: Ticks) -> Self {
        let mut core = ViaCommon::new();
        core.b_in = model.via2_b_in();

        // SCSI IRQ (CB2) and DRQ (CA2) are active high
        core.set_line(ViaLine::CB2, LineState::Low);
        core.set_line(ViaLine::CA2, LineState::Low);
        core.clear_irq(IRQ_CA2 | IRQ_CB2);

        Self {
            core,
            bus_tick_converter: TickConverter::new(bus_frequency),
        }
    }
}

impl Deref for Via2 {
    type Target = ViaCommon<Via2Wiring>;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl DerefMut for Via2 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

impl BusMember<Address> for Via2 {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        Some(self.core.read(ViaReg::from_addr(addr)))
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        self.core.write(ViaReg::from_addr(addr), val);
        Some(())
    }
}

impl Tickable<&dyn EmuContext> for Via2 {
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.bus_tick_converter.add_a_ticks(ticks);
        let via_ticks = self.bus_tick_converter.get_b_ticks(ctx.bus_frequency());
        if via_ticks > 0 {
            self.core.tick_timers(via_ticks)?;
            self.bus_tick_converter
                .subtract_b_ticks(via_ticks, ctx.bus_frequency());
        }
        Ok(ticks)
    }
}

impl Debuggable for Via2 {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_byte_bin;
        use crate::debuggable::*;

        let mut props = self.core.debug_properties_ports();
        props.push(dbgprop_byte_bin!("Shift register", self.core.sr));
        props.extend(self.core.debug_properties_control());
        props
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::via_common::{IRQ_CA1, IRQ_CB1};

    /// Register A with handshake
    const ADDR_A_HS: Address = 0x01 << 9;
    /// Register A without handshake (vBufA)
    const ADDR_A: Address = 0x0F << 9;
    const ADDR_PCR: Address = 0x0C << 9;
    const ADDR_B: Address = 0x00 << 9;

    #[test]
    fn reg_a_no_handshake_keeps_irq_flags() {
        let mut via = Via2::new(MacModel::MacII, 16_000_000);

        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.read(ADDR_A);
        assert!(via.ifr().slot() && via.ifr().scsi_drq());
        via.write(ADDR_A, 0);
        assert!(via.ifr().slot() && via.ifr().scsi_drq());
    }

    #[test]
    fn reg_a_handshake_clears_irq_flags() {
        let mut via = Via2::new(MacModel::MacII, 16_000_000);

        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.read(ADDR_A_HS);
        assert!(!via.ifr().slot() && !via.ifr().scsi_drq());

        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.write(ADDR_A_HS, 0);
        assert!(!via.ifr().slot() && !via.ifr().scsi_drq());
    }

    #[test]
    fn independent_ca2_keeps_irq_flag() {
        let mut via = Via2::new(MacModel::MacII, 16_000_000);

        // CA2 independent interrupt input, negative edge
        via.write(ADDR_PCR, 0x02);
        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.read(ADDR_A_HS);
        assert!(!via.ifr().slot() && via.ifr().scsi_drq());

        // Positive edge
        via.write(ADDR_PCR, 0x06);
        via.write(ADDR_A_HS, 0);
        assert!(via.ifr().scsi_drq());

        // Handshake output is not independent
        via.write(ADDR_PCR, 0x0A);
        via.read(ADDR_A_HS);
        assert!(!via.ifr().scsi_drq());
    }

    #[test]
    fn independent_cb2_keeps_irq_flag() {
        let mut via = Via2::new(MacModel::MacII, 16_000_000);

        via.set_irq(IRQ_CB2);
        via.set_irq(IRQ_CB1);
        via.read(ADDR_B);
        assert!(!via.ifr().scsi_irq() && !via.ifr().asc());

        // CB2 independent interrupt input, negative edge
        via.write(ADDR_PCR, 0x20);
        via.set_irq(IRQ_CB2);
        via.set_irq(IRQ_CB1);
        via.read(ADDR_B);
        assert!(via.ifr().scsi_irq() && !via.ifr().asc());

        // Positive edge
        via.write(ADDR_PCR, 0x60);
        via.write(ADDR_B, 0);
        assert!(via.ifr().scsi_irq());

        // Handshake output is not independent
        via.write(ADDR_PCR, 0xA0);
        via.read(ADDR_B);
        assert!(!via.ifr().scsi_irq());
    }
}
