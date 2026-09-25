//! Normandy decoder implementation for the Macintosh Portable and PowerBook 100.
//! Also known as the "CPU GLU" or "Coarse Address Decode and GLU".
//! Aside from the address decoding duties, this chip also handled mapping and timing for the
//! SLIM card system.
//! SLIM card adapter support has also been included, though the SLIM hardware never released.

use crate::bus::{Address, BusMember};
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::emulator::comm::SlimSlotStatus;
use crate::mac::scsi::disk_image::DiskImage;
use crate::tickable::{Tickable, Ticks};
use anyhow::{Result, bail};
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

const IDLE_DTACK_DELAY: u8 = 64;
const SLIM_DTACK_DELAY: u8 = 16;
//const ROM_DTACK_DELAY: u8 = 2;

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
        /// SLIM card is read-only/write protected
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
        /// SLIM card is software write protected
        protect: bool @ 3,
    }
}

#[derive(Default)]
struct SlimCard {
    image: Option<Box<dyn DiskImage>>,
    write_protect: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Normandy {
    // Idle speed register
    pub idle_speed: bool,
    // SLIM DTACK load register
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
    #[serde(skip)]
    cards: [SlimCard; 2],

    pub dtack_counter: u8,
}

impl Normandy {
    pub(crate) fn new(slim: bool) -> Self {
        Self {
            idle_speed: false,
            slim_dtack: false,
            slim_mapper: vec![SlimMapper(0); 16],

            slim_adapter: SlimAdapter(0x00).with_installed(slim),
            slim1_status: SlimStatus(0x00),
            slim1_eject: SlimEject(0x00).with_eject(true),
            slim1_protect: SlimProtect(0),
            slim2_status: SlimStatus(0x00),
            slim2_eject: SlimEject(0x00).with_eject(true),
            slim2_protect: SlimProtect(0),

            slim_rom: vec![0; 0x10000],

            cards: Default::default(),
            dtack_counter: 0,
        }
    }

    /// Card images aren't serialized, so report both slots as empty after loading a state
    pub(crate) fn after_deserialize(&mut self) {
        self.slim1_status.set_inserted(false);
        self.slim1_status.set_readonly(false);
        self.slim2_status.set_inserted(false);
        self.slim2_status.set_readonly(false);
    }

    pub(crate) fn slim_installed(&self) -> bool {
        self.slim_adapter.installed()
    }

    pub(crate) fn slim_status(&self) -> [Option<SlimSlotStatus>; 2] {
        core::array::from_fn(|i| {
            let card = &self.cards[i];
            let image = card.image.as_ref()?;
            Some(SlimSlotStatus {
                image: image.image_path()?.to_path_buf(),
                size: image.byte_len(),
                write_protect: card.write_protect,
            })
        })
    }

    pub(crate) fn slim_insert(
        &mut self,
        slot: usize,
        image: Box<dyn DiskImage>,
        write_protect: bool,
    ) -> Result<()> {
        if !self.slim_installed() {
            bail!("SLIM card adapter not installed");
        }
        if slot >= self.cards.len() {
            bail!("Invalid SLIM slot {}", slot);
        }
        match slot {
            0 => {
                self.slim1_status.set_inserted(true);
                self.slim1_eject.set_eject(true);
                self.slim1_status.set_readonly(write_protect);
            }
            _ => {
                self.slim2_status.set_inserted(true);
                self.slim2_eject.set_eject(true);
                self.slim2_status.set_readonly(write_protect);
            }
        }
        self.cards[slot] = SlimCard {
            image: Some(image),
            write_protect,
        };
        Ok(())
    }

    pub(crate) fn slim_eject(&mut self, slot: usize) -> Result<()> {
        if slot >= self.cards.len() {
            bail!("Invalid SLIM slot {}", slot);
        }
        self.cards[slot].image = None;
        match slot {
            0 => {
                self.slim1_eject.set_eject(false);
            }
            _ => {
                self.slim2_eject.set_eject(false);
            }
        }
        Ok(())
    }

    fn slim_read(&self, slot: usize, offset: usize) -> Option<u8> {
        let image = self.cards[slot].image.as_ref()?;
        if offset >= image.byte_len() {
            return None;
        }
        match image.media_bytes() {
            Some(bytes) => Some(bytes[offset]),
            None => Some(image.read_bytes(offset, 1)[0]),
        }
    }

    fn slim_write(&mut self, slot: usize, offset: usize, val: u8) {
        let card = &mut self.cards[slot];
        if card.write_protect {
            return;
        }
        if let Some(image) = card.image.as_mut()
            && offset < image.byte_len()
        {
            image.write_bytes(offset, &[val]);
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
                } else if !self.slim_dtack && (0x0050_0000..=0x008F_FFFF).contains(&addr) {
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
            }
            _ => false,
        }
    }
}

impl BusMember<Address> for Normandy {
    fn read(&mut self, addr: Address) -> Option<u8> {
        match addr {
            // SLIM card 1 space
            0x50_0000..=0x6F_FFFF => self.slim_read(0, (addr - 0x50_0000) as usize),
            // SLIM card 2 space
            0x70_0000..=0x8F_FFFF => self.slim_read(1, (addr - 0x70_0000) as usize),
            // SLIM adapter ROM
            0xE0_0000..=0xE0_FFFF => Some(0x00),
            // SLIM adapter registers
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
            // Normandy SLIM registers
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
                    Some(self.slim_adapter.0)
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
            // SLIM card 1 space
            0x50_0000..=0x6F_FFFF => {
                if !self.slim1_protect.protect() {
                    self.slim_write(0, (addr - 0x50_0000) as usize, val);
                }
                Some(())
            }
            // SLIM card 2 space
            0x70_0000..=0x8F_FFFF => {
                if !self.slim2_protect.protect() {
                    self.slim_write(1, (addr - 0x70_0000) as usize, val);
                }
                Some(())
            }
            // SLIM adapter registers
            0xF0_0000..=0xF0_FFFF => match addr {
                0xF0_0010 => Some(()),
                0xF0_0011 => {
                    self.slim1_eject.set_eject(SlimEject(val).eject());
                    Some(())
                }
                0xF0_0020 => Some(()),
                0xF0_0021 => {
                    self.slim1_protect.set_protect(SlimProtect(val).protect());
                    Some(())
                }
                0xF0_0040 => Some(()),
                0xF0_0041 => {
                    self.slim2_eject.set_eject(SlimEject(val).eject());
                    Some(())
                }
                0xF0_0050 => Some(()),
                0xF0_0051 => {
                    self.slim2_protect.set_protect(SlimProtect(val).protect());
                    Some(())
                }
                _ => None,
            },
            // Normandy SLIM registers
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
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        if !self.slim1_eject.eject() && self.slim1_status.inserted() {
            self.slim_eject(0)?;
            self.slim1_status.set_inserted(false);
            self.slim1_status.set_readonly(false);
        }
        if !self.slim2_eject.eject() && self.slim2_status.inserted() {
            self.slim_eject(1)?;
            self.slim2_status.set_inserted(false);
            self.slim2_status.set_readonly(false);
        }
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
            dbgprop_bool!("Slim Adapter Installed", self.slim_adapter.installed()),
            dbgprop_bool!("Slim 1 Inserted", self.slim1_status.inserted()),
            dbgprop_bool!("Slim 2 Inserted", self.slim2_status.inserted()),
            dbgprop_bool!("Slim 1 Read Only", self.slim1_status.readonly()),
            dbgprop_bool!("Slim 2 Read Only", self.slim2_status.readonly()),
            dbgprop_bool!("Slim 1 Protected", self.slim1_protect.protect()),
            dbgprop_bool!("Slim 2 Protected", self.slim2_protect.protect()),
            dbgprop_bool!("Slim 1 Ejecting", !self.slim1_eject.eject()),
            dbgprop_bool!("Slim 2 Ejecting", !self.slim2_eject.eject()),
        ]
    }
}
