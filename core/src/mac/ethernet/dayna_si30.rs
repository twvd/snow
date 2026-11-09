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

use super::sonic::{Sonic, SonicDma};
use crate::bus::Address;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::mac::declrom::{CAT_BOARD, CAT_NETWORK, DeclRom, Entry, SResource, TYPE_ETHERNET, id};
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

    /// The high byte of a register write, kept until its low byte is written:
    /// the PDS slot writes a byte at a time
    reg_high: Option<(usize, u8)>,
}

impl DaynaSi30 {
    pub fn new(port: EthernetPort) -> Self {
        Self {
            sonic: Sonic::new(SONIC_REVISION, port),
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

    pub fn irq(&self) -> bool {
        self.sonic.irq()
    }

    pub fn reset(&mut self) {
        self.sonic.reset();
        self.reg_high = None;
    }

    pub fn tick(&mut self, ticks: Ticks, frequency: Ticks, dma: &mut impl SonicDma) {
        self.sonic.tick(ticks, frequency, dma);
    }

    /// The SONIC register at `offset`, if `offset` is in the low half of a
    /// long word
    fn sonic_reg(offset: Address) -> Option<usize> {
        (offset & 2 != 0).then_some((offset as usize >> 2) & 0x3F)
    }

    /// Reads the byte at `offset` in the slot
    pub fn read(&self, offset: Address) -> Option<u8> {
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

    /// Writes the byte at `offset` in the slot
    pub fn write(&mut self, offset: Address, val: u8, dma: &mut impl SonicDma) -> Option<()> {
        let offset = offset & DECODE_MASK;
        if !SONIC_REGS.contains(&offset) {
            return None;
        }
        let Some(reg) = Self::sonic_reg(offset) else {
            // The SONIC isn't connected to D31-D16: writes to the high half of the
            // long word are ignored
            return Some(());
        };
        if offset & 1 == 0 {
            self.reg_high = Some((reg, val));
        } else {
            let high = match self.reg_high.take() {
                Some((r, high)) if r == reg => high,
                _ => (self.sonic.read(reg) >> 8) as u8,
            };
            self.sonic
                .write(reg, (u16::from(high) << 8) | u16::from(val), dma);
        }
        Some(())
    }
}

impl Debuggable for DaynaSi30 {
    fn get_debug_properties(&self) -> DebuggableProperties {
        self.sonic.get_debug_properties()
    }
}

const fn fits_se30(mac: crate::mac::MacModel) -> bool {
    matches!(mac, crate::mac::MacModel::SE30)
}

pub const INFO: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::DaynaPortEsi30,
    name: "DaynaPORT E/si30",
    bus: crate::emulator::devices::BusKind::Se30Pds,
    fits: fits_se30,
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

    struct NoDma;

    impl SonicDma for NoDma {
        fn read_byte(&mut self, _addr: Address) -> u8 {
            0xFF
        }

        fn write_byte(&mut self, _addr: Address, _val: u8) {}
    }

    fn card() -> DaynaSi30 {
        DaynaSi30::new(EthernetPort::new([0x00, 0x80, 0x19, 0x12, 0x34, 0x56]))
    }

    #[test]
    fn registers_on_the_low_half_of_each_long() {
        let mut c = card();
        // Receive Sequence Counter (register $2B): read/write, no side effects
        let a = 0x01_0000 + 0x2B * 4 + 2;
        c.write(a, 0x12, &mut NoDma).unwrap();
        c.write(a + 1, 0x34, &mut NoDma).unwrap();
        assert_eq!(c.read(a), Some(0x12));
        assert_eq!(c.read(a + 1), Some(0x34));
        // D31-D16 read 0 and ignore writes
        assert_eq!(c.read(a - 2), Some(0));
        c.write(a - 2, 0x56, &mut NoDma).unwrap();
        assert_eq!(c.read(a), Some(0x12));
        // Dayna's driver reaches them at $s10000 in slot s, in both address modes
        assert_eq!(c.read(0x91_0000 + 0x2B * 4 + 3), Some(0x34));
    }

    #[test]
    fn mac_address_in_the_rom() {
        let c = card();
        let mac: Vec<u8> = (0..6).map(|i| c.read(0xFF_E004 + i).unwrap()).collect();
        assert_eq!(mac, [0x00, 0x80, 0x19, 0x12, 0x34, 0x56]);
    }

    #[test]
    fn declaration_rom_at_the_top() {
        let c = card();
        assert_eq!(c.read(0xFF_FFFF), Some(0x0F));
        let test: Vec<u8> = (0..4).map(|i| c.read(0xFF_FFFA + i).unwrap()).collect();
        assert_eq!(test, [0x5A, 0x93, 0x2B, 0xC7]);
        assert_eq!(c.read(0xFF_DFFF), None);
        let len = u32::from_be_bytes(core::array::from_fn(|i| {
            c.read(0xFF_FFEC + 4 + i as Address).unwrap()
        }));
        let image: Vec<u8> = (0..len)
            .map(|i| c.read(0x100_0000 - len + i).unwrap())
            .collect();
        assert!(crate::mac::declrom::valid(&image));
    }
}
