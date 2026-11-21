//! The two VIAs of the Lisa I/O board

use std::ops::{Deref, DerefMut};

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::mac::via_common::{ViaCommon, ViaReg, ViaWiring};
use crate::tickable::{Tickable, Ticks};

bitfield! {
    /// Port B of the keyboard VIA
    #[derive(Clone, Copy, Serialize, Deserialize)]
    pub struct KeyboardRegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Low holds the keyboard and mouse interfaces in reset
        pub not_reset: bool @ 0,
        pub volume: u8 @ 1..=3,
        /// Floppy controller wants attention
        pub fdir: bool @ 4,
        /// COPS ready, active low
        pub crdy: bool @ 6,
        /// Parallel port controller reset, active low
        pub not_cres: bool @ 7,
    }
}

bitfield! {
    /// Port B of the parallel port VIA
    #[derive(Clone, Copy, Serialize, Deserialize)]
    pub struct ParallelRegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Open cable detect: high is nothing attached
        pub ocd: bool @ 0,
        /// Busy, active low
        pub not_bsy: bool @ 1,
        pub not_den: bool @ 2,
        pub drw: bool @ 3,
        pub not_cmd: bool @ 4,
        pub parity: bool @ 5,
        /// Disk diagnostic from the floppy controller
        pub disk_diag: bool @ 6,
        /// Contrast latch write
        pub wcnt: bool @ 7,
    }
}

/// Port A and the interrupt registers have no machine specific bits
pub struct KeyboardViaWiring;

impl ViaWiring for KeyboardViaWiring {
    type RegisterA = u8;
    type RegisterB = KeyboardRegisterB;
    type RegisterIRQ = u8;
}

pub struct ParallelViaWiring;

impl ViaWiring for ParallelViaWiring {
    type RegisterA = u8;
    type RegisterB = ParallelRegisterB;
    type RegisterIRQ = u8;
}

/// A VIA of the Lisa
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct LisaVia<W: ViaWiring> {
    core: ViaCommon<W>,
}

pub type KeyboardVia = LisaVia<KeyboardViaWiring>;
pub type ParallelVia = LisaVia<ParallelViaWiring>;

impl<W: ViaWiring> LisaVia<W> {
    pub fn new() -> Self {
        Self {
            core: ViaCommon::new(),
        }
    }

    /// The register with this number
    pub fn register(number: u32) -> ViaReg {
        ViaReg::from_addr((number & 0x0F) << 9)
    }
}

impl<W: ViaWiring> Deref for LisaVia<W> {
    type Target = ViaCommon<W>;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl<W: ViaWiring> DerefMut for LisaVia<W> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

impl<W: ViaWiring> Tickable for LisaVia<W> {
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        self.core.tick_timers(ticks)?;
        Ok(ticks)
    }
}

impl<W: ViaWiring> Debuggable for LisaVia<W> {
    fn get_debug_properties(&self) -> DebuggableProperties {
        let mut props = self.core.debug_properties_ports();
        props.extend(self.core.debug_properties_control());
        props
    }
}
