//! Apple Ethernet LC Card (820-0443) and Apple Ethernet LC Twisted-Pair Card
//! (820-0532), in the PDS slot of the LC family
//!
//! A SONIC on a 16-bit port, which bus-masters into the guest's RAM. The
//! address map comes from MAME's model of the cards (enetlc.cpp), and was
//! checked against Apple's .ENET driver in the declaration ROM 341-0470 of
//! the Ethernet LC Card: the SONIC registers at the base of the slot, one
//! per long word on its high half, the MAC address PROM at $400000, and
//! 16-bit DMA (the driver writes 3 to DCR, which leaves DW, the data width
//! bit, clear). The Twisted-Pair Card's ROM (341-0740) holds no driver, only
//! what the Slot Manager reads: Snow builds it at run time.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::mac_prom_byte;
use super::sonic::{Sonic, SonicDma};
use crate::bus::Address;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::mac::declrom::{CAT_BOARD, CAT_NETWORK, DeclRom, Entry, SResource, TYPE_ETHERNET, id};
use crate::net::EthernetPort;
use crate::tickable::Ticks;

/// SONIC silicon revision: 4 for the DP83932 (Linux macsonic.c)
const SONIC_REVISION: u16 = 4;

/// The SONIC registers, at the bottom of the slot
const SONIC_REGS: std::ops::RangeInclusive<Address> = 0x0_0000..=0x0_01FF;

/// MAC address PROM, where the 341-0470 driver reads it
const MAC_PROM: std::ops::RangeInclusive<Address> = 0x40_0000..=0x40_01FF;

/// MAC address PROM, where MAME and Linux (macsonic.c, for the Twisted-Pair
/// Card) also have it
const MAC_PROM_LOW: std::ops::RangeInclusive<Address> = 0x4_0000..=0x4_01FF;

/// What a word read of the MAC address PROM returns, as MAME has it: a word
/// read is "expected to return this magic value"
const MAC_PROM_WORD: u32 = 0x0028;

/// The address bits the card decodes: A19-A0, so that its 1 MB repeats over
/// the slot, as the 24-bit address map gives the slot only 1 MB. The MAC
/// address PROM at $400000 is the only thing outside it.
const DECODE_MASK: Address = 0x0F_FFFF;

/// What a read returns on the byte lanes that the card doesn't drive
const OPEN_BUS: u32 = 0xFFFF;

/// Driver hardware id of the Ethernet sResource, as in ROM 341-0740
const TP_DRVR_HW: u16 = 0x0119;

/// Board id, as in ROM 341-0740
const TP_BOARD_ID: u32 = 0x5D;

/// The declaration ROM of the Twisted-Pair Card: as 341-0740, without the
/// copyright notice
fn tp_decl_rom() -> Vec<u8> {
    DeclRom {
        resources: vec![
            SResource {
                id: id::BOARD,
                entries: vec![
                    Entry::rsrc_type(CAT_BOARD, 0, 0, 0),
                    Entry::string(id::RSRC_NAME, "Apple Ethernet LC Twisted-Pair Card"),
                    Entry::Value(id::BOARD_ID, TP_BOARD_ID),
                    Entry::exec_block(id::PRIMARY_INIT, 2),
                    Entry::List(
                        id::VENDOR_INFO,
                        vec![
                            Entry::string(id::VENDOR_ID, "Apple Computer, Inc."),
                            Entry::string(id::REV_LEVEL, "ROM rev. 1.0"),
                            Entry::string(id::PART_NUM, "341-0740"),
                        ],
                    ),
                    Entry::exec_block(id::SECONDARY_INIT, 4),
                ],
            },
            SResource {
                id: 0x81,
                entries: vec![
                    Entry::rsrc_type(CAT_NETWORK, TYPE_ETHERNET, 0, TP_DRVR_HW),
                    Entry::string(id::RSRC_NAME, "Network_EtherNet_none_AppleLCTwistedPair"),
                    Entry::Value(id::RSRC_HW_DEV_ID, 1),
                ],
            },
        ],
        rev_level: 1,
        byte_lanes: 0b1111,
    }
    .build()
}

#[derive(Serialize, Deserialize)]
pub struct ApplLcSonic {
    sonic: Sonic,

    /// Declaration ROM, at the top of the slot on all four byte lanes
    rom: Vec<u8>,
}

impl ApplLcSonic {
    /// The Ethernet LC Card, with the image of its declaration ROM
    pub fn new(port: EthernetPort, rom: Vec<u8>) -> Self {
        Self {
            sonic: Sonic::new(SONIC_REVISION, port),
            rom,
        }
    }

    /// The Ethernet LC Twisted-Pair Card
    pub fn new_twisted_pair(port: EthernetPort) -> Self {
        Self::new(port, tp_decl_rom())
    }

    /// Reconnects the host side after a state was loaded
    pub fn after_deserialize(&mut self) {
        self.sonic.after_deserialize();
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

    pub fn tick(&mut self, ticks: Ticks, frequency: Ticks, dma: &mut impl SonicDma) {
        self.sonic.tick(ticks, frequency, dma);
    }

    /// The SONIC register at `offset`, if `offset` is in the high half of a
    /// long word
    fn sonic_reg(offset: Address) -> Option<usize> {
        (SONIC_REGS.contains(&offset) && offset & 2 == 0).then_some((offset as usize >> 2) & 0x3F)
    }

    /// Reads `len` bytes (1 or 2) of the 16-bit port at `offset` in the slot
    pub fn read(&mut self, offset: Address, len: u32) -> u32 {
        let prom = if MAC_PROM.contains(&offset) {
            Some(offset - MAC_PROM.start())
        } else {
            MAC_PROM_LOW
                .contains(&(offset & DECODE_MASK))
                .then(|| (offset & DECODE_MASK) - MAC_PROM_LOW.start())
        };
        if let Some(index) = prom {
            return match len {
                2 => MAC_PROM_WORD,
                _ => mac_prom_byte(self.sonic.port().mac(), index as usize).into(),
            };
        }
        let offset = offset & DECODE_MASK;
        if let Some(reg) = Self::sonic_reg(offset) {
            let val = u32::from(self.sonic.read(reg));
            return match (len, offset & 1) {
                (2, _) => val,
                (_, 0) => val >> 8,
                _ => val & 0xFF,
            };
        }
        (offset..offset + len).fold(0, |v, a| (v << 8) | u32::from(self.read_rom(a)))
    }

    /// Writes `len` bytes (1 or 2) of the 16-bit port at `offset` in the slot
    pub fn write(&mut self, offset: Address, len: u32, val: u32, dma: &mut impl SonicDma) {
        let offset = offset & DECODE_MASK;
        let Some(reg) = Self::sonic_reg(offset) else {
            return;
        };
        let val = match (len, offset & 1) {
            (2, _) => val as u16,
            (_, 0) => ((val as u16) << 8) | (self.sonic.read(reg) & 0xFF),
            _ => (self.sonic.read(reg) & 0xFF00) | (val as u16 & 0xFF),
        };
        self.sonic.write(reg, val, dma);
    }

    /// The declaration ROM byte at `offset`, the ROM ending at the top of
    /// the 1 MB the card decodes
    fn read_rom(&self, offset: Address) -> u8 {
        let below_top = (DECODE_MASK - (offset & DECODE_MASK)) as usize;
        self.rom
            .len()
            .checked_sub(below_top + 1)
            .map_or(OPEN_BUS as u8, |i| self.rom[i])
    }
}

impl Debuggable for ApplLcSonic {
    fn get_debug_properties(&self) -> DebuggableProperties {
        self.sonic.get_debug_properties()
    }
}

/// Whether `rom` is a declaration ROM which the Ethernet LC Card can carry
pub fn check_rom(rom: &[u8]) -> Result<()> {
    if rom.len() != 0x8000 || !crate::mac::declrom::valid(rom) {
        anyhow::bail!("Not a 32 KB declaration ROM of the Apple Ethernet LC Card");
    }
    Ok(())
}

const fn fits_lc(mac: crate::mac::MacModel) -> bool {
    matches!(
        mac,
        crate::mac::MacModel::LC
            | crate::mac::MacModel::LCII
            | crate::mac::MacModel::LCIII
            | crate::mac::MacModel::LC475
    )
}

pub const INFO: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::AppleEthernetLc,
    name: "Apple Ethernet LC Card",
    bus: crate::emulator::devices::BusKind::LcPds,
    fits: fits_lc,
    function: crate::emulator::devices::FunctionKind::Ethernet,
    fixed: true,
    one_per_bus: false,
    // ROMs 341-0470 and 341-0842, both from the Macintosh LC Ethernet Adapter
    rom: Some(crate::emulator::devices::RomInfo {
        label: "Ethernet LC Card ROM",
        digests: &[
            "e4d460e1ed918232fdd39b0ee40c7e2daf6ad9ca57e779fd2d662886cfdcc67f",
            "cb6151d8ac8438c97c51bc1c32680773e030f0e6a3dbc713537c46c1c5e9ce21",
        ],
    }),
    path_keys: &["rom"],
    adds_buses: &[],
    monitors: &[],
    create: None,
};

pub const INFO_TP: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::AppleEthernetLcTp,
    name: "Apple Ethernet LC Twisted-Pair Card",
    bus: crate::emulator::devices::BusKind::LcPds,
    fits: fits_lc,
    function: crate::emulator::devices::FunctionKind::Ethernet,
    fixed: true,
    one_per_bus: false,
    rom: None,
    path_keys: &[],
    adds_buses: &[],
    monitors: &[],
    create: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// No DMA
    struct NoDma;

    impl SonicDma for NoDma {
        fn read_byte(&mut self, _addr: Address) -> u8 {
            0xFF
        }

        fn write_byte(&mut self, _addr: Address, _val: u8) {}
    }

    fn card() -> ApplLcSonic {
        ApplLcSonic::new_twisted_pair(EthernetPort::new([0x08, 0x00, 0x07, 0x12, 0x34, 0x56]))
    }

    #[test]
    fn registers_on_the_high_half_of_each_long() {
        let mut c = card();
        // Receive Sequence Counter (register $2B): read/write, no side effects
        let a = 0x2B * 4;
        c.write(a, 2, 0x1234, &mut NoDma);
        assert_eq!(c.read(a, 2), 0x1234);
        assert_eq!(c.read(a, 1), 0x12);
        assert_eq!(c.read(a + 1, 1), 0x34);
        c.write(a + 1, 1, 0x56, &mut NoDma);
        assert_eq!(c.read(a, 2), 0x1256);
        // Writes to the low half of the long word don't reach the register
        c.write(a + 2, 2, 0xABCD, &mut NoDma);
        assert_eq!(c.read(a, 2), 0x1256);
        // The card's 1 MB repeats over the slot
        assert_eq!(c.read(0x10_0000 + a, 2), 0x1256);
    }

    #[test]
    fn mac_prom() {
        let mut c = card();
        for base in [0x40_0000, 0x4_0000] {
            let bytes: Vec<u8> = (0..8).map(|k| c.read(base + k, 1) as u8).collect();
            // The address bytes with their bits reversed: 08 -> 10, 00 -> 00,
            // 07 -> E0, 12 -> 48
            assert_eq!(bytes[..4], [0x10, 0x00, 0xE0, 0x48]);
            assert_eq!(bytes[6], 0);
            assert_eq!(bytes.iter().fold(0, |x, &b| x ^ b), 0xFF);
            assert_eq!(c.read(base, 2), MAC_PROM_WORD);
        }
    }

    #[test]
    fn declaration_rom_at_the_top_on_all_lanes() {
        let mut c = card();
        let rom = tp_decl_rom();
        assert!(crate::mac::declrom::valid(&rom));
        // The ByteLanes field is the last byte of the card's 1 MB, and the
        // TestPattern is just below it
        assert_eq!(c.read(0xFF_FFFF, 1), 0x0F);
        assert_eq!(c.read(0xFF_FFFA, 2), 0x5A93);
        assert_eq!(c.read(0x0F_FFFF, 1), 0x0F);
        let bottom = 0x10_0000 - rom.len() as Address;
        assert_eq!(c.read(bottom, 1), u32::from(rom[0]));
        assert_eq!(c.read(bottom - 1, 1), 0xFF);
    }
}
