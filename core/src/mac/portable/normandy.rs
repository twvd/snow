//! Normandy decoder implementation for the Macintosh Portable and PowerBook 100.
//! Also known as the "CPU GLU" or "Coarse Address Decode and GLU".
//! Aside from the address decoding duties, this chip also handled mapping and timing for the
//! SLIM card system.

use crate::bus::{Address, BusMember};
use crate::dbgprop_bool;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::tickable::{Tickable, Ticks};
use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

const IDLE_DTACK_DELAY: u8 = 64;
const SLIM_DTACK_DELAY: u8 = 16;
const ROM_DTACK_DELAY: u8 = 2;

bitfield! {
    #[derive(Clone, Serialize, Deserialize)]
    struct SlimMapper(u8): {
        bit0: bool @ 0,
        bit1: bool @ 1,
        /// Controls whether the memory range is mapped to SLIM_CS0 or SLIM_CS1
        bit2: bool @ 2,
    }
}

bitfield! {
    #[derive(Serialize, Deserialize)]
    struct SlimAdapter(u8): {
        /// SLIM card adapter installed
        installed: bool @ 3,
    }
}

bitfield! {
    #[derive(Serialize, Deserialize)]
    struct SlimStatus(u8): {
        /// SLIM card is read-only
        readonly: bool @ 2,
        /// SLIM card is inserted
        inserted: bool @ 3,
    }
}

bitfield! {
    #[derive(Serialize, Deserialize)]
    struct SlimEject(u8): {
        /// Low to eject
        eject: bool @ 3,
    }
}

bitfield! {
    #[derive(Serialize, Deserialize)]
    struct SlimProtect(u8): {
        /// SLIM card is write protected
        protect: bool @ 3,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Normandy {
    // Idle speed register
    pub idle_speed: bool,
    // SLIM DTACK loag register
    pub slim_dtack: bool,
    slim_mapper: Vec<SlimMapper>,

    slim_adapter: SlimAdapter,
    slim1_status: SlimStatus,
    slim1_eject: SlimEject,
    slim1_protect: SlimProtect,
    slim2_status: SlimStatus,
    slim2_eject: SlimEject,
    slim2_protect: SlimProtect,

    slim_rom: Vec<u8>,

    pub dtack_counter: u8,
}

impl Normandy {
    pub(crate) fn new() -> Self {
        Self {
            idle_speed: false,
            slim_dtack: false,
            slim_mapper: vec![SlimMapper(0); 16],

            slim_adapter: SlimAdapter(0x00),
            slim1_status: SlimStatus(0x08),
            slim1_eject: SlimEject(0x08),
            slim1_protect: SlimProtect(0),
            slim2_status: SlimStatus(0x08),
            slim2_eject: SlimEject(0x08),
            slim2_protect: SlimProtect(0),

            slim_rom: vec![0; 0x10000],

            dtack_counter: 0,
        }
    }

    pub(crate) fn waitstate(&mut self, addr: Address) -> bool {
        match addr {
            0x0000_0000..=0x008F_FFFF => {
                if self.idle_speed {
                    match self.dtack_counter {
                        0 => {
                            self.dtack_counter = IDLE_DTACK_DELAY;
                            true
                        }
                        1 => {
                            self.dtack_counter = 0;
                            false
                        }
                        _ => {
                            self.dtack_counter -= 1;
                            true
                        }
                    }
                } else if !self.slim_dtack & (0x0050_0000..=0x0050_FFFF).contains(&addr) {
                    match self.dtack_counter {
                        0 => {
                            self.dtack_counter = SLIM_DTACK_DELAY;
                            true
                        }
                        1 => {
                            self.dtack_counter = 0;
                            false
                        }
                        _ => {
                            self.dtack_counter -= 1;
                            true
                        }
                    }
                } else {
                    false
                }
            },
            // 0x0090_0000..=0x0090_FFFF => match self.dtack_counter {
            //     0 => {
            //         self.dtack_counter = ROM_DTACK_DELAY;
            //         true
            //     }
            //     1 => {
            //         self.dtack_counter = 0;
            //         false
            //     }
            //     _ => {
            //         self.dtack_counter -= 1;
            //         true
            //     }
            // },
            _ => false,
        }
    }
}

impl BusMember<Address> for Normandy {
    fn read(&mut self, addr: Address) -> Option<u8> {
        match addr {
            // SLIM adapter ROM
            0xE0_0000..=0xE0_FFFF => Some(0x00),
            0xF0_0000..=0xF0_FFFF => {
                if self.slim_adapter.installed() {
                    match addr {
                        0xF0_0000 => Some(0x00),
                        0xF0_0001 => Some(self.slim1_status.0),
                        0xF0_0011 => Some(self.slim1_eject.0),
                        0xF0_0020 => Some(0x00),
                        0xF0_0021 => Some(self.slim1_protect.0),
                        0xF0_0030 => Some(0x00),
                        0xF0_0031 => Some(self.slim2_status.0),
                        0xF0_0041 => Some(self.slim2_eject.0),
                        0xF0_0050 => Some(0x00),
                        0xF0_0051 => Some(self.slim2_protect.0),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            0xFC_0000..=0xFC_FFFF => match addr & 0x21F {
                0x000..=0x01F => {
                    if addr & 0x1 != 0 {
                        Some(self.slim_mapper[((addr & 0x1F) >> 1) as usize].0)
                    } else {
                        Some(0x00)
                    }
                }
                0x200..=0x201 => {
                    self.slim_dtack = true;
                    if self.slim_adapter.installed() {
                        Some(0x08)
                    } else {
                        Some(0x00)
                    }
                }
                0x202..=0x203 => Some(0x00),
                _ => None,
            },
            // Idle speed register
            0xFE_0000..=0xFE_FFFF => match addr & 0x202 {
                0x000 => {
                    self.idle_speed = false;
                    Some(0xFF)
                }
                0x002 => {
                    self.idle_speed = true;
                    Some(0xFF)
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        match addr {
            0xF0_0000..=0xF0_FFFF => Some(()),
            0xFC_0000..=0xFC_FFFF => match addr & 0x21F {
                0x000..=0x01F => {
                    if addr & 0x1 != 0 {
                        self.slim_mapper[((addr & 0x1F) >> 1) as usize].0 = val & 0x7;
                    }
                    Some(())
                }
                0x200..=0x201 => {
                    self.slim_dtack = true;
                    Some(())
                }
                0x202..=0x203 => Some(()),
                _ => None,
            },
            // Idle speed register
            0xFE_0000..=0xFE_FFFF => match addr & 0x202 {
                0x000 => {
                    self.idle_speed = false;
                    Some(())
                }
                0x002 => {
                    self.idle_speed = true;
                    Some(())
                }
                _ => None,
            },
            _ => None,
        }
    }
}

impl Tickable for Normandy {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        Ok(ticks)
    }
}

impl Debuggable for Normandy {
    fn get_debug_properties(&self) -> DebuggableProperties {
        use crate::dbgprop_bool;
        use crate::debuggable::*;

        vec![
            dbgprop_bool!("Idle", self.idle_speed),
            dbgprop_bool!("Slim DTACK", self.slim_dtack),
        ]
    }
}
