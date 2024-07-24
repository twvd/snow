use super::via::Via;
use crate::bus::{Address, Bus, BusMember};
use crate::cpu_m68k::Byte;
use crate::tickable::{Tickable, Ticks};

use anyhow::Result;

pub struct MacBus {
    rom: Vec<u8>,
    pub ram: Vec<u8>,
    via: Via,
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
        }
    }
}

impl Bus<Address, Byte> for MacBus {
    fn get_mask(&self) -> Address {
        0x00FFFFFF
    }

    fn read(&self, addr: Address) -> Byte {
        let val = match addr {
            0x0000_0000..=0x000F_FFFF | 0x0040_0000..=0x004F_FFFF => {
                Some(self.rom[(addr & 0xFFFF) as usize])
            }
            0x0060_0000..=0x007F_FFFF => Some(self.ram[addr as usize & (Self::RAM_SIZE - 1)]),
            // IWD (ignore for now, too spammy)
            0x00DF_F000..=0x00DF_FFFF => Some(0xFF),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.read(addr),

            _ => None,
        };

        if let Some(v) = val {
            v
        } else {
            println!("Read from unimplemented address: {:08X}", addr);
            0xFF
        }
    }

    fn write(&mut self, addr: Address, val: Byte) {
        let written = match addr {
            0x0060_0000..=0x007F_FFFF => Some(self.ram[addr as usize & (Self::RAM_SIZE - 1)] = val),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.write(addr, val),
            _ => None,
        };
        if written.is_none() {
            println!("write: {:08X} {:02X}", addr, val);
        }
    }
}

impl Tickable for MacBus {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        Ok(1)
    }
}
