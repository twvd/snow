//! Apple Ethernet NB Twisted-Pair Card (820-0511, ROM 341-1096)
//!
//! A DP83932 SONIC with 128 KiB of RAM on a NuBus card. The SONIC's DMA only
//! reaches the card's RAM: the guest's driver copies the frames between it
//! and the Mac's RAM. The address map and the wiring of the SONIC registers
//! follow MAME's model of the card (enetnbtp.cpp), not checked against the
//! hardware or Apple's driver. Its declaration ROM holds no driver, only what
//! the Slot Manager reads: Snow builds it at run time.

use std::fmt::Display;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::sonic::{Sonic, SonicDma};
use crate::bus::Address;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::emulator::EmuContext;
use crate::mac::declrom::{CAT_BOARD, CAT_NETWORK, DeclRom, Entry, SResource, TYPE_ETHERNET, id};
use crate::mac::nubus::{NubusSlot, bus_long, lanes, standard_offset};
use crate::net::EthernetPort;
use crate::tickable::{Tickable, Ticks};

/// Size of the RAM on the card
const RAM_SIZE: usize = 0x2_0000;

/// SONIC silicon revision: 4 for the DP83932 (Linux macsonic.c)
const SONIC_REVISION: u16 = 4;

/// Driver hardware id of the Ethernet sResource, as in ROM 341-1096
const DRVR_HW: u16 = 0x0118;

/// Board id, as in ROM 341-1096
const BOARD_ID: u32 = 0x57;

/// Byte lanes of the declaration ROM: lane 1 only
const ROM_BYTE_LANES: u8 = 0b0010;

/// The SONIC's 16-bit registers, one every two bytes
const SONIC_REGS: std::ops::RangeInclusive<Address> = 0x0C_0000..=0x0C_007F;

/// MAC address PROM, on byte lanes 1 and 3
const MAC_PROM: std::ops::RangeInclusive<Address> = 0x40_0000..=0x40_000F;

/// The card's RAM, as the SONIC and the slot see it: at the bottom of the
/// slot and again at $C20000
fn ram_offset(addr: Address) -> Option<usize> {
    match addr & 0xFF_FFFF {
        a @ (0x00_0000..=0x01_FFFF | 0xC2_0000..=0xC3_FFFF) => Some(a as usize % RAM_SIZE),
        _ => None,
    }
}

/// The card's RAM as the SONIC sees it
struct CardRam<'a>(&'a mut [u8]);

impl SonicDma for CardRam<'_> {
    fn read_byte(&mut self, addr: Address) -> u8 {
        ram_offset(addr).map_or(0xFF, |a| self.0[a])
    }

    fn write_byte(&mut self, addr: Address, val: u8) {
        if let Some(a) = ram_offset(addr) {
            self.0[a] = val;
        }
    }
}

/// A byte of the MAC address PROM: the bytes of the address with their bits
/// reversed, then a check byte which makes the eight bytes XOR to $FF.
///
/// MAME swaps the nibbles and reverses only the low one, which reads back
/// right only for bytes below $10, such as Apple's 08:00:07: Apple's .ENET
/// driver (in the declaration ROM 341-0470 of the LC card, which also drives
/// the Quadras) reverses all eight bits of each byte it reads.
fn prom_byte(mac: [u8; 6], index: usize) -> u8 {
    match index {
        0..=5 => mac[index].reverse_bits(),
        7 => !mac.iter().fold(0, |x, &b| x ^ b.reverse_bits()),
        _ => 0,
    }
}

/// The declaration ROM: as 341-1096, without the copyright notice
fn decl_rom() -> Vec<u8> {
    DeclRom {
        resources: vec![
            SResource {
                id: id::BOARD,
                entries: vec![
                    Entry::rsrc_type(CAT_BOARD, 0, 0, 0),
                    Entry::string(id::RSRC_NAME, "Apple Ethernet NB Twisted-Pair Card"),
                    Entry::Value(id::BOARD_ID, BOARD_ID),
                    Entry::exec_block(id::PRIMARY_INIT, 2),
                    Entry::List(
                        id::VENDOR_INFO,
                        vec![
                            Entry::string(id::VENDOR_ID, "Apple Computer, Inc."),
                            Entry::string(id::REV_LEVEL, "ROM rev. 1.0"),
                            Entry::string(id::PART_NUM, "341-1096"),
                        ],
                    ),
                    Entry::exec_block(id::SECONDARY_INIT, 4),
                ],
            },
            SResource {
                id: 0x81,
                entries: vec![
                    Entry::rsrc_type(CAT_NETWORK, TYPE_ETHERNET, 0, DRVR_HW),
                    Entry::string(id::RSRC_NAME, "Network_EtherNet_none_AppleNBTwistedPair"),
                    Entry::Value(id::RSRC_HW_DEV_ID, 1),
                ],
            },
        ],
        rev_level: 1,
        byte_lanes: ROM_BYTE_LANES,
    }
    .build()
}

#[derive(Serialize, Deserialize)]
pub struct ApplNbSonicTp {
    sonic: Sonic,
    ram: Vec<u8>,

    #[serde(skip, default = "decl_rom")]
    rom: Vec<u8>,

    /// The high byte of a register that is written a byte at a time, kept
    /// until its low byte is written
    reg_high: Option<(usize, u8)>,
}

impl ApplNbSonicTp {
    pub fn new(port: EthernetPort) -> Self {
        Self {
            sonic: Sonic::new(SONIC_REVISION, port),
            ram: vec![0; RAM_SIZE],
            rom: decl_rom(),
            reg_high: None,
        }
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

    /// The declaration ROM byte at `addr`, at the top of the slot on lane 1
    fn read_rom(&self, addr: Address) -> Option<u8> {
        let below_top = 0xFF_FFFD_u32.checked_sub(addr & 0xFF_FFFF)? as usize;
        if addr % 4 != 1 || below_top / 4 >= self.rom.len() {
            return None;
        }
        Some(self.rom[self.rom.len() - 1 - below_top / 4])
    }
}

impl ApplNbSonicTp {
    /// The register at an offset in the SONIC's range, a word every two bytes
    fn reg(offset: Address) -> usize {
        ((offset & 0x7F) / 2) as usize
    }

    /// Writes a byte of a register. The high byte is kept until the low byte
    /// is written, which writes both to the register.
    fn write_reg_byte(&mut self, offset: Address, val: u8) {
        let reg = Self::reg(offset);
        if offset.is_multiple_of(2) {
            self.reg_high = Some((reg, val));
            return;
        }
        let high = match self.reg_high.take() {
            Some((r, high)) if r == reg => high,
            _ => (self.sonic.read(reg) >> 8) as u8,
        };
        self.write_reg(reg, (u16::from(high) << 8) | u16::from(val));
    }

    fn write_reg(&mut self, reg: usize, val: u16) {
        self.sonic.write(reg, val, &mut CardRam(&mut self.ram));
    }
}

impl NubusSlot for ApplNbSonicTp {
    fn read_sized(&mut self, addr: Address, size: u32) -> Option<u32> {
        let a = standard_offset(addr)?;
        if let Some(start) = ram_offset(a) {
            let bytes = self.ram.get(start..start + size as usize)?;
            return Some(bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b)));
        }
        let word = a & !3;
        let long = if SONIC_REGS.contains(&word) {
            // A long word holds two registers
            let reg = Self::reg(word);
            (u32::from(self.sonic.read(reg)) << 16) | u32::from(self.sonic.read(reg + 1))
        } else if MAC_PROM.contains(&word) {
            // The MAC address PROM is on byte lanes 1 and 3
            let mac = self.sonic.port().mac();
            let index = (word & 0xF) as usize / 2;
            (u32::from(prom_byte(mac, index)) << 16) | u32::from(prom_byte(mac, index + 1))
        } else {
            // The declaration ROM is on byte lane 1
            u32::from(self.read_rom(word + 1)?) << 16
        };
        Some(lanes(long, a, size))
    }

    fn write_sized(&mut self, addr: Address, size: u32, val: u32) -> Option<()> {
        let a = standard_offset(addr)?;
        if let Some(start) = ram_offset(a) {
            let bytes = self.ram.get_mut(start..start + size as usize)?;
            bytes.copy_from_slice(&val.to_be_bytes()[4 - size as usize..]);
            return Some(());
        }
        // Apple's driver ("Apple Ethernet NB" 1.0.1, 'en87') clears the word at $C00002
        // when it opens. MAME leaves that address unmapped, and nothing documents it. So
        // the card takes the writes to addresses it doesn't decode, instead of refusing
        // them with a bus error.
        if !SONIC_REGS.contains(&(a & !3)) {
            return Some(());
        }
        let long = bus_long(val, size, a);
        match size {
            1 => self.write_reg_byte(a, (long >> (8 * (3 - a % 4))) as u8),
            2 => self.write_reg(Self::reg(a), (long >> (16 * (1 - (a % 4) / 2))) as u16),
            _ => {
                let reg = Self::reg(a);
                self.write_reg(reg, (long >> 16) as u16);
                self.write_reg(reg + 1, long as u16);
            }
        }
        Some(())
    }

    fn get_irq(&self) -> bool {
        self.sonic.irq()
    }

    fn reset(&mut self) {
        self.sonic.reset();
        self.reg_high = None;
    }
}

impl Tickable<&dyn EmuContext> for ApplNbSonicTp {
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.sonic
            .tick(ticks, ctx.bus_frequency(), &mut CardRam(&mut self.ram));
        Ok(ticks)
    }
}

impl Debuggable for ApplNbSonicTp {
    fn get_debug_properties(&self) -> DebuggableProperties {
        self.sonic.get_debug_properties()
    }
}

impl Display for ApplNbSonicTp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Apple Ethernet NB Twisted-Pair Card")
    }
}

pub const INFO: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::AppleEthernetNbTp,
    name: "Apple Ethernet NB Twisted-Pair Card",
    bus: crate::emulator::devices::BusKind::Nubus,
    fits: crate::emulator::devices::ModelInfo::fits_all,
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

    /// The card's standard slot space, as the bus gives a card its addresses
    const SLOT: Address = 0xF000_0000;

    fn card() -> ApplNbSonicTp {
        ApplNbSonicTp::new(EthernetPort::new([0x08, 0x00, 0x07, 0x12, 0x34, 0x56]))
    }

    #[test]
    fn ram_and_its_mirror() {
        let mut c = card();
        c.write_sized(SLOT | 0x00_1234, 1, 0xA5).unwrap();
        assert_eq!(c.read_sized(SLOT | 0xC2_1234, 1), Some(0xA5));
        c.write_sized(SLOT | 0xC3_FFFC, 4, 0x1234_565A).unwrap();
        assert_eq!(c.read_sized(SLOT | 0x01_FFFC, 4), Some(0x1234_565A));
    }

    #[test]
    fn registers_a_word_every_two_bytes() {
        let mut c = card();
        // Receive Sequence Counter (register $2B): read/write, no side effects
        let a = 0x0C_0000 + 0x2B * 2;
        c.write_sized(SLOT | a, 2, 0x1234).unwrap();
        assert_eq!(c.read_sized(SLOT | a, 2), Some(0x1234));
        assert_eq!(c.read_sized(SLOT | a, 1), Some(0x12));
        assert_eq!(c.read_sized(SLOT | a + 1, 1), Some(0x34));
        // A byte at a time, high byte first
        c.write_sized(SLOT | a, 1, 0x56).unwrap();
        c.write_sized(SLOT | a + 1, 1, 0x78).unwrap();
        assert_eq!(c.read_sized(SLOT | a, 2), Some(0x5678));
    }

    #[test]
    fn mac_prom() {
        let mut c = card();
        let mac = c.port().mac();
        let bytes: Vec<u8> = (0..8)
            .map(|k| c.read_sized(SLOT | 0x40_0001 + 2 * k, 1).unwrap() as u8)
            .collect();
        // The address bytes with their bits reversed: 08 -> 10, 00 -> 00, 07 -> E0
        assert_eq!(bytes[..3], [0x10, 0x00, 0xE0]);
        assert_eq!(bytes[3], prom_byte(mac, 3));
        assert_eq!(bytes[6], 0);
        assert_eq!(bytes[..6].iter().fold(bytes[7], |x, &b| x ^ b), 0xFF);
    }

    #[test]
    fn declaration_rom_at_the_top_on_lane_1() {
        let mut c = card();
        let rom = decl_rom();
        // The ByteLanes field is the last byte of the slot, and the TestPattern is
        // just below it
        assert_eq!(c.read_sized(SLOT | 0xFF_FFFD, 1), Some(0xD2));
        assert_eq!(c.read_sized(SLOT | 0xFF_FFFE, 1), Some(0));
        let test: Vec<u32> = (2..6)
            .rev()
            .map(|n| c.read_sized(SLOT | 0xFF_FFFD - 4 * n, 1).unwrap())
            .collect();
        assert_eq!(test, [0x5A, 0x93, 0x2B, 0xC7]);
        assert_eq!(
            c.read_sized(SLOT | 0xFF_FFFD - 4 * (rom.len() as u32 - 1), 1),
            Some(u32::from(rom[0]))
        );
    }
}
