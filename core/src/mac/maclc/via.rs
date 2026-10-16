use std::ops::{Deref, DerefMut};

use crate::bus::{Address, BusMember};
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::emulator::EmuContext;
use crate::mac::MacModel;
use crate::mac::via_common::{ViaCommon, ViaReg, ViaWiring};
use crate::tickable::{TickConverter, Tickable, Ticks};
use crate::types::Byte;

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

/// The frequency at which to run the VIA.
///
/// Since the VIA is used as a timer, it is important to run at a consistent
/// speed regardless of CPU/bus frequency.
const VIA_FREQUENCY: Ticks = 16_000_000;

bitfield! {
    /// VIA Register A
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterA(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// CPU identification bit 0
        pub cpuid0: bool @ 1,

        /// CPU identification bit 1
        pub cpuid1: bool @ 2,

        /// Synchronous modem
        pub sync: bool @ 3,

        /// CPU identification bit 2
        pub cpuid2: bool @ 4,

        /// Floppy head select
        pub headsel: bool @ 5,

        /// CPU identification bit 3
        pub cpuid3: bool @ 6,

        /// SCC Wait/Request
        pub sccwrreq: bool @ 7,
    }
}

bitfield! {
    /// VIA Register B
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Egret transceiver session (input, active low)
        pub xcvr_ses: bool @ 3,

        /// Egret VIA full (output)
        pub via_full: bool @ 4,

        /// Egret system session (output)
        pub sys_ses: bool @ 5,
    }
}

bitfield! {
    /// VIA Interrupt flag/enable registers
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterIRQ(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// 60.15 Hz interrupt
        /// Cleared on Register A read/write
        pub tick60: bool @ 1,

        /// Egret shift register
        /// Cleared on read/write shift reg
        pub egret: bool @ 2,

        /// Timer T2
        /// Cleared on read of T2 counter LSB or write of T2 counter MSB
        pub t2: bool @ 5,

        /// Timer T1
        /// Cleared on read of T1 counter LSB or write of T1 counter MSB
        pub t1: bool @ 6,
    }
}

pub struct LcViaWiring;

impl ViaWiring for LcViaWiring {
    type RegisterA = RegisterA;
    type RegisterB = RegisterB;
    type RegisterIRQ = RegisterIRQ;
}

/// VIA 1 of the Macintosh LC
#[derive(Serialize, Deserialize)]
pub struct Via {
    core: ViaCommon<LcViaWiring>,

    bus_tick_converter: TickConverter<VIA_FREQUENCY>,
}

impl Via {
    pub fn new(model: MacModel) -> Self {
        let mut core = ViaCommon::new();
        core.a_in = RegisterA(model.via1_a_in().0);
        // The session lines to the Egret read as negated while they are not
        // driven, they are assumed to be pulled down.
        core.b_in = RegisterB(0xFF).with_via_full(false).with_sys_ses(false);

        Self {
            core,
            bus_tick_converter: Default::default(),
        }
    }
}

impl Deref for Via {
    type Target = ViaCommon<LcViaWiring>;

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
        Some(self.core.read(ViaReg::from_addr(addr)))
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        self.core.write(ViaReg::from_addr(addr), val);
        Some(())
    }
}

impl Tickable<&dyn EmuContext> for Via {
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

impl Debuggable for Via {
    fn get_debug_properties(&self) -> DebuggableProperties {
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

    const ADDR_DDRA: Address = 0x03 << 9;
    const ADDR_A: Address = 0x0F << 9;

    #[test]
    fn cpu_id() {
        let mut via = Via::new(MacModel::LC);

        // PA6, PA4 and PA2 high, PA1 low
        assert_eq!(via.read(ADDR_A).unwrap() & 0x56, 0x54);

        // Still readable through the outputs set up by the ROM
        via.write(ADDR_DDRA, 0x2F);
        via.write(ADDR_A, 0x26);
        assert_eq!(via.read(ADDR_A).unwrap() & 0x50, 0x50);
        assert!(via.pa().headsel());
    }
}
