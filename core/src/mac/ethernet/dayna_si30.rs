//! DaynaPORT E/si30, the Ethernet card for the 030 PDS of the SE/30 and the
//! IIsi
//!
//! A SONIC-T (DP83934) on a 32-bit port, which bus-masters into the guest's
//! RAM with 32-bit DMA. Nothing documents the card: its address map comes
//! from Dayna's driver ("DaynaPORT E/si30 v1.5f0", 'enet' 890 in "Network
//! Resources SONIC" of the DaynaPORT 7.5.3 installer). For the card in slot
//! s, the driver finds the SONIC registers at $Fs s1 0000, one per long word
//! on its low half, and the MAC address 4 bytes into the declaration ROM
//! chip, at $Fs sF E004.
//!
//! The driver uses these addresses in both the 24-bit and the 32-bit mode. In
//! the 24-bit mode, the top byte is ignored: $Fs s1 0000 is $s1 0000, at
//! offset $10000 in the 1 MB that the 24-bit map gives the slot. In the
//! 32-bit mode, it is at offset $s1 0000 in the slot's 16 MB. Both reach the
//! registers because the card decodes only A19-A0, so that its 1 MB repeats
//! over the slot: the registers are at offset $10000, and the ROM chip ends
//! the 1 MB. NetBSD's sn driver puts Dayna's registers at $180000, where this
//! card doesn't answer; its $FFE004 for the MAC address agrees with Dayna's
//! driver.
//!
//! The driver directory of the card's ROM points outside the chip, so Mac OS
//! needs Dayna's driver. Snow builds the declaration ROM at run time, as in a
//! dump of the card's ROM.

use serde::{Deserialize, Serialize};

use super::low_word_written;
use super::sonic::Sonic;
use crate::bus::Address;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::mac::declrom::{CAT_BOARD, CAT_NETWORK, DeclRom, Entry, SResource, TYPE_ETHERNET, id};
use crate::mac::dma::DmaOps;
use crate::net::EthernetPort;
use crate::tickable::Ticks;

/// SONIC silicon revision: the DP83934 reports $0100 or $0101 (Linux
/// macsonic.c)
const SONIC_REVISION: u16 = 0x0100;

/// The address bits the card decodes, A19-A0: its 1 MB repeats over the slot
const DECODE_MASK: Address = 0x0F_FFFF;

/// The SONIC registers
const SONIC_REGS: std::ops::RangeInclusive<Address> = 0x01_0000..=0x01_00FF;

/// The declaration ROM chip, at the top of the megabyte on all four byte lanes
const ROM_SIZE: usize = 0x2000;
const ROM_BASE: Address = DECODE_MASK + 1 - ROM_SIZE as Address;

/// Where the MAC address is in the ROM chip
const ROM_MAC: std::ops::Range<usize> = 4..10;

/// Driver software and hardware ids of the Ethernet sResource, as in the
/// card's ROM
const DRVR_SW: u16 = 0x010B;
const DRVR_HW: u16 = 0x0100;

/// Board id, as in the card's ROM
const BOARD_ID: u32 = 0x37A;

/// The image of the declaration ROM chip, as in a dump of the card's ROM,
/// but without its driver directory and without the MAC address: reads of
/// the MAC address bytes return the port's address
fn decl_rom() -> Vec<u8> {
    DeclRom {
        resources: vec![
            SResource {
                id: id::BOARD,
                entries: vec![
                    Entry::rsrc_type(CAT_BOARD, 0, 0, 0),
                    Entry::string(id::RSRC_NAME, "DaynaPORT E/si30"),
                    Entry::Value(id::BOARD_ID, BOARD_ID),
                    Entry::List(
                        id::VENDOR_INFO,
                        vec![
                            Entry::string(id::VENDOR_ID, "Dayna Communications"),
                            Entry::string(id::REV_LEVEL, "2.0"),
                            Entry::string(id::PART_NUM, "DaynaPORT_E/si30"),
                        ],
                    ),
                ],
            },
            SResource {
                id: 0x80,
                entries: vec![
                    Entry::rsrc_type(CAT_NETWORK, TYPE_ETHERNET, DRVR_SW, DRVR_HW),
                    Entry::string(
                        id::RSRC_NAME,
                        "Network_Ethernet_DaynaCommunications_DaynaPORT_E/si30",
                    ),
                    Entry::Value(id::RSRC_FLAGS, 0),
                    Entry::Value(id::RSRC_HW_DEV_ID, 1),
                    // Entry $80 points to the MAC address, as in the card's ROM
                    Entry::ChipByte(0x80, ROM_MAC.start),
                ],
            },
        ],
        rev_level: 2,
        byte_lanes: 0b1111,
    }
    .build_chip(ROM_SIZE)
}

#[derive(Serialize, Deserialize)]
pub struct DaynaSi30 {
    sonic: Sonic,

    #[serde(skip, default = "decl_rom")]
    rom: Vec<u8>,
}

impl DaynaSi30 {
    pub fn new(port: EthernetPort) -> Self {
        Self {
            sonic: Sonic::new(SONIC_REVISION, port),
            rom: decl_rom(),
        }
    }

    pub fn port(&self) -> &EthernetPort {
        self.sonic.port()
    }

    pub fn port_mut(&mut self) -> &mut EthernetPort {
        self.sonic.port_mut()
    }

    pub fn irq(&self) -> bool {
        self.sonic.irq()
    }

    pub fn reset(&mut self) {
        self.sonic.reset();
    }

    pub fn tick(&mut self, ticks: Ticks, frequency: Ticks, dma: &mut dyn DmaOps) {
        self.sonic.tick(ticks, frequency, dma);
    }

    /// The SONIC register at `offset`, if `offset` is in the low half of a
    /// long word
    fn sonic_reg(offset: Address) -> Option<usize> {
        (offset & 2 != 0).then_some((offset as usize >> 2) & 0x3F)
    }

    /// Reads `len` bytes at `offset` in the slot, one cycle of the CPU. None where the card
    /// doesn't answer.
    pub fn read(&self, offset: Address, len: u32) -> Option<u32> {
        (offset..offset + len).try_fold(0, |v, a| Some((v << 8) | u32::from(self.read_byte(a)?)))
    }

    fn read_byte(&self, offset: Address) -> Option<u8> {
        let offset = offset & DECODE_MASK;
        if SONIC_REGS.contains(&offset) {
            // The card doesn't drive D31-D16: the high half of the long word reads 0
            let Some(reg) = Self::sonic_reg(offset) else {
                return Some(0);
            };
            let [high, low] = self.sonic.read(reg).to_be_bytes();
            return Some(if offset & 1 == 0 { high } else { low });
        }
        let byte = offset.checked_sub(ROM_BASE)? as usize;
        if ROM_MAC.contains(&byte) {
            return Some(self.sonic.port().mac()[byte - ROM_MAC.start]);
        }
        self.rom.get(byte).copied()
    }

    /// Writes `len` bytes of `val` at `offset` in the slot, one cycle of the CPU. The SONIC is on
    /// D15-D0: what the cycle puts on D31-D16 is ignored.
    pub fn write(&mut self, offset: Address, len: u32, val: u32) -> Option<()> {
        let offset = offset & DECODE_MASK;
        if !SONIC_REGS.contains(&offset) {
            return None;
        }
        if let Some(value) = low_word_written(offset, len, val) {
            self.sonic.write((offset as usize >> 2) & 0x3F, value);
        }
        Some(())
    }
}

impl Debuggable for DaynaSi30 {
    fn get_debug_properties(&self) -> DebuggableProperties {
        self.sonic.get_debug_properties()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> DaynaSi30 {
        DaynaSi30::new(EthernetPort::new([0x00, 0x80, 0x19, 0x12, 0x34, 0x56]))
    }

    #[test]
    fn registers_on_the_low_half_of_each_long() {
        let mut c = card();
        // Receive Sequence Counter (register $2B): read/write, no side effects
        let a = 0x01_0000 + 0x2B * 4 + 2;
        c.write(a, 2, 0x1234).unwrap();
        assert_eq!(c.read(a, 2), Some(0x1234));
        assert_eq!(c.read(a + 1, 1), Some(0x34));
        assert_eq!(c.read(a - 2, 4), Some(0x1234));
        // D31-D16 read 0 and ignore writes
        assert_eq!(c.read(a - 2, 2), Some(0));
        c.write(a - 2, 2, 0x5678).unwrap();
        assert_eq!(c.read(a, 2), Some(0x1234));
        // A byte written alone sets both bytes
        c.write(a + 1, 1, 0x56).unwrap();
        assert_eq!(c.read(a, 2), Some(0x5656));
        // Dayna's driver reaches them at $s10000 in slot s, in both address modes
        assert_eq!(c.read(0x91_0000 + 0x2B * 4 + 2, 2), Some(0x5656));
    }

    #[test]
    fn mac_address_in_the_rom() {
        let c = card();
        let mac: Vec<u8> = (0..6)
            .map(|i| c.read(0xFF_E004 + i, 1).unwrap() as u8)
            .collect();
        assert_eq!(mac, [0x00, 0x80, 0x19, 0x12, 0x34, 0x56]);
    }

    #[test]
    fn declaration_rom_at_the_top() {
        let c = card();
        assert_eq!(c.read(0xFF_FFFF, 1), Some(0x0F));
        assert_eq!(c.read(0xFF_FFFA, 4), Some(0x5A93_2BC7));
        assert_eq!(c.read(0xFF_DFFF, 1), None);
        let len = c.read(0xFF_FFF0, 4).unwrap();
        let image: Vec<u8> = (0..len)
            .map(|i| c.read(0x100_0000 - len + i, 1).unwrap() as u8)
            .collect();
        assert!(crate::mac::declrom::valid(&image));
    }
}
