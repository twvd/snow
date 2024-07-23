use crate::bus::{Address, Bus};
use crate::cpu_m68k::Byte;
use crate::tickable::{Tickable, Ticks};

use anyhow::Result;

pub struct MacBus {
    rom: Vec<u8>,
    ram: Vec<u8>,
}

impl MacBus {
    const RAM_SIZE: usize = 512 * 1024;
    const ROM_SIZE: usize = 64 * 1024;

    pub fn new(rom: &[u8]) -> Self {
        assert_eq!(rom.len(), Self::ROM_SIZE);

        Self {
            rom: Vec::from(rom),
            ram: vec![0xFF; Self::RAM_SIZE],
        }
    }
}

impl Bus<Address, Byte> for MacBus {
    fn get_mask(&self) -> Address {
        0x00FFFFFF
    }

    fn read(&self, addr: Address) -> Byte {
        match addr {
            0x0000_0000..=0x000F_FFFF | 0x0040_0000..=0x004F_FFFF => {
                self.rom[(addr & 0xFFFF) as usize]
            }
            0x0060_0000..=0x007F_FFFF => self.ram[addr as usize & (Self::RAM_SIZE - 1)],
            _ => {
                println!("Read from unimplemented address: {:08X}", addr);
                0xFF
            }
        }
    }

    fn write(&mut self, addr: Address, val: Byte) {
        match addr {
            0x0060_0000..=0x007F_FFFF => self.ram[addr as usize & (Self::RAM_SIZE - 1)] = val,
            _ => println!("write: {:08X} {:02X}", addr, val),
        }
    }
}

impl Tickable for MacBus {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        Ok(1)
    }
}
