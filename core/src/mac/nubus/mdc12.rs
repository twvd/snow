use crate::bus::{Address, BusMember};

/// Macintosh Display Card 1.2.341-0868
pub struct Mdc12 {
    rom: Vec<u8>,
}

impl Mdc12 {
    pub fn new() -> Self {
        Self {
            rom: std::fs::read("341-0868.bin").expect("Graphics card ROM file"),
        }
    }
}

impl BusMember<Address> for Mdc12 {
    fn read(&mut self, addr: Address) -> Option<u8> {
        // Assume normal slot, not super slot
        match addr & 0xFF_FFFF {
            0x20_0000..=0x20_000F => {
                // ?
                None
            }
            0x20_0100..=0x20_01FF => {
                // CRTC
                None
            }
            0xFE_0000..=0xFF_FFFF => {
                // ROM (4 byte lanes)
                // Occupy byte lane 3
                if addr % 4 == 3 {
                    log::debug!(
                        "ROM read {:06X} = {:02X}",
                        addr & 0xFF_FFFF,
                        self.rom[((addr - 0xFE_0000) / 4) as usize]
                    );
                    Some(self.rom[((addr - 0xFE_0000) / 4) as usize])
                } else {
                    Some(0xFF)
                }
            }
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        None
    }
}
