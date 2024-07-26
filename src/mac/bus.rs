use super::via::Via;
use crate::bus::{Address, Bus, BusMember, IrqSource};
use crate::mac::video::Video;
use crate::tickable::{Tickable, Ticks};
use crate::types::Byte;

use anyhow::Result;

pub struct MacBus {
    rom: Vec<u8>,
    pub ram: Vec<u8>,
    pub via: Via,
    video: Video,
}

impl MacBus {
    const RAM_SIZE: usize = 512 * 1024;
    const ROM_SIZE: usize = 64 * 1024;

    pub fn new(rom: &[u8]) -> Self {
        assert_eq!(rom.len(), Self::ROM_SIZE);

        Self {
            rom: Vec::from(rom),
            ram: vec![0xFF; Self::RAM_SIZE],
            via: Via::new(),
            video: Video::new(),
        }
    }

    fn write_overlay(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            0x0060_0000..=0x007F_FFFF => {
                if val != 0xFF {
                    if (0x67_2700..=0x67_7C80).contains(&addr) {
                        println!("write to primary vram {:08X} = {}", addr, val);
                    }
                    if (0x67_A700..=0x67_FC80).contains(&addr) {
                        println!("write to alt vram {:08X} = {}", addr, val);
                    }
                }
                Some(self.ram[addr as usize & (Self::RAM_SIZE - 1)] = val)
            }
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.write(addr, val),
            _ => None,
        }
    }

    fn write_normal(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            0x0000_0000..=0x003F_FFFF => {
                if val != 0xFF {
                    if (0x7_2700..=0x7_7C80).contains(&addr) {
                        println!("write to primary vram {:08X} = {}", addr, val);
                    }
                    if (0x7_A700..=0x7_FC80).contains(&addr) {
                        println!("write to alt vram {:08X} = {}", addr, val);
                    }
                }
                Some(self.ram[addr as usize & (Self::RAM_SIZE - 1)] = val)
            }
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.write(addr, val),
            _ => None,
        }
    }

    fn read_overlay(&self, addr: Address) -> Option<Byte> {
        match addr {
            0x0000_0000..=0x000F_FFFF | 0x0040_0000..=0x004F_FFFF => {
                Some(self.rom[(addr & 0xFFFF) as usize])
            }
            0x0060_0000..=0x007F_FFFF => Some(self.ram[addr as usize & (Self::RAM_SIZE - 1)]),
            // IWD (ignore for now, too spammy)
            0x00DF_F000..=0x00DF_FFFF => Some(31),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.read(addr),

            _ => None,
        }
    }

    fn read_normal(&self, addr: Address) -> Option<Byte> {
        match addr {
            0x0000_0000..=0x003F_FFFF => Some(self.ram[addr as usize & (Self::RAM_SIZE - 1)]),
            0x0040_0000..=0x004F_FFFF => Some(self.rom[(addr & 0xFFFF) as usize]),
            // IWD (ignore for now, too spammy)
            0x00DF_F000..=0x00DF_FFFF => Some(31),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.read(addr),

            _ => None,
        }
    }
}

impl Bus<Address, Byte> for MacBus {
    fn get_mask(&self) -> Address {
        0x00FFFFFF
    }

    fn read(&self, addr: Address) -> Byte {
        let val = if self.via.a.overlay() {
            self.read_overlay(addr)
        } else {
            self.read_normal(addr)
        };

        if let Some(v) = val {
            v
        } else {
            println!("Read from unimplemented address: {:08X}", addr);
            0xFF
        }
    }

    fn write(&mut self, addr: Address, val: Byte) {
        let written = if self.via.a.overlay() {
            self.write_overlay(addr, val)
        } else {
            self.write_normal(addr, val)
        };
        if written.is_none() {
            println!("write: {:08X} {:02X}", addr, val);
        }
    }
}

impl Tickable for MacBus {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        assert_eq!(ticks, 1);

        // Pixel clock (15.6672 MHz) is roughly 2x CPU speed
        self.video.tick(2)?;

        // Sync VIA registers
        self.via.b.set_h4(self.video.in_hblank());

        // VBlank interrupt
        if self.video.get_clr_vblank() && self.via.irq_enable.vblank() {
            self.via.irq_flag.set_vblank(true);
        }

        Ok(1)
    }
}

impl IrqSource for MacBus {
    fn get_irq(&mut self) -> Option<u8> {
        // VIA IRQs
        if self.via.irq_flag.0 != 0 {
            return Some(1);
        }

        None
    }
}
