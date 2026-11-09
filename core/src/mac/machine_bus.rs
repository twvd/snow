//! One bus for the machines with the same CPU data bus
//!
//! It passes each access on to the bus of the machine. The CPU core is compiled
//! against these, so that the machines with the same CPU share one copy of it.
//! The compact Macs and the Portable have no choice of CPU, and keep their own
//! buses.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::access::{Bus68020, Bus68040, BusAccess, BusResponse};
use crate::bus::{Address, Bus, InspectRun, InspectableBus, IrqSource};
use crate::mac::macii::bus::MacIIBus;
use crate::mac::maclc::lc_475_bus::Lc475Bus;
use crate::mac::maclc::lc_i_bus::LcIBus;
use crate::mac::maclc::lc_iii_bus::LcIIIBus;
use crate::mac::maclc::pds_iie::PdsIIeCard;
use crate::mac::quadra::bus::Quadra700Bus;
use crate::mac::quadra::q800_bus::Quadra800Bus;
use crate::renderer::channel::ChannelRenderer;
use crate::tickable::{Tickable, Ticks};

macro_rules! machine_bus {
    (
        $(#[$doc:meta])*
        $name:ident, $cpu:ty,
        $($variant:ident($bus:ty, $get:ident, $get_mut:ident)),+ $(,)?
    ) => {
        $(#[$doc])*
        // Not boxed, which would add a pointer to follow on each access
        #[allow(clippy::large_enum_variant)]
        #[derive(Serialize, Deserialize)]
        pub enum $name {
            $($variant($bus),)+
        }

        impl $name {
            $(
                pub fn $get(&self) -> &$bus {
                    match self {
                        Self::$variant(bus) => bus,
                        _ => unreachable!(),
                    }
                }

                pub fn $get_mut(&mut self) -> &mut $bus {
                    match self {
                        Self::$variant(bus) => bus,
                        _ => unreachable!(),
                    }
                }
            )+
        }

        impl Bus<Address, u8> for $name {
            type Cpu = $cpu;

            fn get_mask(&self) -> Address {
                match self {
                    $(Self::$variant(bus) => bus.get_mask(),)+
                }
            }

            #[inline(always)]
            fn access(&mut self, acc: BusAccess<$cpu>) -> Result<BusResponse> {
                match self {
                    $(Self::$variant(bus) => bus.access(acc),)+
                }
            }

            fn reset(&mut self, hard: bool) -> Result<bool> {
                match self {
                    $(Self::$variant(bus) => bus.reset(hard),)+
                }
            }

            fn take_reset_request(&mut self) -> bool {
                match self {
                    $(Self::$variant(bus) => bus.take_reset_request(),)+
                }
            }

            #[inline(always)]
            fn localtalk_poll_needed(&self) -> bool {
                match self {
                    $(Self::$variant(bus) => bus.localtalk_poll_needed(),)+
                }
            }

            #[inline(always)]
            fn cpu_tick(&mut self, ticks: Ticks) -> Result<()> {
                match self {
                    $(Self::$variant(bus) => bus.cpu_tick(ticks),)+
                }
            }
        }

        impl Tickable for $name {
            #[inline(always)]
            fn tick(&mut self, ticks: Ticks, ctx: ()) -> Result<Ticks> {
                match self {
                    $(Self::$variant(bus) => bus.tick(ticks, ctx),)+
                }
            }
        }

        impl IrqSource for $name {
            #[inline(always)]
            fn get_irq(&mut self) -> Option<u8> {
                match self {
                    $(Self::$variant(bus) => bus.get_irq(),)+
                }
            }
        }

        impl InspectableBus<Address, u8> for $name {
            fn inspect_read(&mut self, addr: Address) -> Option<u8> {
                match self {
                    $(Self::$variant(bus) => bus.inspect_read(addr),)+
                }
            }

            fn inspect_write(&mut self, addr: Address, val: u8) -> Option<()> {
                match self {
                    $(Self::$variant(bus) => bus.inspect_write(addr, val),)+
                }
            }

            fn inspect_read_bulk(&mut self, addr: Address, out: &mut [u8]) -> InspectRun {
                match self {
                    $(Self::$variant(bus) => bus.inspect_read_bulk(addr, out),)+
                }
            }

            fn inspect_generation(&self) -> u32 {
                match self {
                    $(Self::$variant(bus) => bus.inspect_generation(),)+
                }
            }
        }
    };
}

machine_bus!(
    /// The bus of a machine with a 68020 or a 68030
    MachineBus020,
    Bus68020,
    MacII(MacIIBus<ChannelRenderer>, macii, macii_mut),
    LcI(LcIBus<ChannelRenderer>, lc_i, lc_i_mut),
    LcIII(LcIIIBus<ChannelRenderer>, lc_iii, lc_iii_mut),
);

impl MachineBus020 {
    /// The Apple IIe Card in the PDS of an LC
    pub fn iie_card_mut(&mut self) -> Option<&mut PdsIIeCard> {
        match self {
            Self::MacII(_) => None,
            Self::LcI(bus) => bus.iie_card_mut(),
            Self::LcIII(bus) => bus.iie_card_mut(),
        }
    }
}

machine_bus!(
    /// The bus of a machine with a 68040
    MachineBus040,
    Bus68040,
    Quadra700(Quadra700Bus<ChannelRenderer>, quadra700, quadra700_mut),
    Lc475(Lc475Bus<ChannelRenderer>, lc475, lc475_mut),
    Quadra800(Quadra800Bus<ChannelRenderer>, quadra800, quadra800_mut),
);

impl MachineBus040 {
    /// The Apple IIe Card in the PDS of an LC 475
    pub fn iie_card_mut(&mut self) -> Option<&mut PdsIIeCard> {
        match self {
            Self::Quadra700(_) | Self::Quadra800(_) => None,
            Self::Lc475(bus) => bus.iie_card_mut(),
        }
    }
}
