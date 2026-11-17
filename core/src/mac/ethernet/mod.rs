//! Ethernet controllers

pub mod appl_nb_sonic_tp;
pub mod built_in;
pub mod sonic;

use crate::bus::Address;
use crate::devices::EthernetInfo;

/// The maker's prefix of the Ethernet addresses of Apple's adapters
pub(crate) const APPLE: EthernetInfo = EthernetInfo {
    oui: [0x08, 0x00, 0x07],
};

/// A byte of the MAC address PROM: bytes 0-5 are the bytes of the address with their bits
/// reversed, byte 6 is 0, and byte 7 is a check byte that makes the eight bytes XOR to $FF, as
/// QEMU builds it.
///
/// MAME swaps the nibbles of each byte and reverses the bits of the low one only, which gives the
/// right value only for bytes below $10, such as those of Apple's 08:00:07 prefix. Apple's .ENET
/// driver (in the declaration ROM 341-0470 of the LC card, which also drives the Quadras' built-in
/// Ethernet) reverses all eight bits of each byte it reads.
pub(crate) fn mac_prom_byte(mac: [u8; 6], index: usize) -> u8 {
    match index {
        0..=5 => mac[index].reverse_bits(),
        7 => !mac.iter().fold(0, |x, &b| x ^ b.reverse_bits()),
        _ => 0,
    }
}

/// The value a CPU write puts on D15-D0, for a 16-bit register wired to those lines of a long
/// word: `len` bytes of `val` at `addr`. None if the write doesn't reach them.
///
/// The 68020 and 68030 drive a byte they write on all four byte lanes (MC68030 User's Manual,
/// table 7-5), so a byte written alone sets both bytes of the register. The 68040 drives
/// undefined data on the lanes it doesn't write (MC68040 User's Manual, table 7-1): the same is
/// done for it, so that every machine and card behaves alike. No known driver writes a byte to
/// these registers.
pub(crate) fn low_word_written(addr: Address, len: u32, val: u32) -> Option<u16> {
    let byte = |a: Address| (val >> (8 * (addr + len - 1 - a))) as u8;
    let high = (addr..addr + len).find(|a| a & 3 == 2).map(byte);
    let low = (addr..addr + len).find(|a| a & 3 == 3).map(byte);
    match (high, low) {
        (Some(high), Some(low)) => Some(u16::from_be_bytes([high, low])),
        (Some(b), None) | (None, Some(b)) => Some(u16::from_be_bytes([b, b])),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_register_on_the_low_lanes_takes_what_the_cpu_writes() {
        assert_eq!(low_word_written(0x100, 4, 0x1234_5678), Some(0x5678));
        assert_eq!(low_word_written(0x102, 2, 0xABCD), Some(0xABCD));
        assert_eq!(low_word_written(0x100, 2, 0xABCD), None);
        assert_eq!(low_word_written(0x102, 1, 0x5A), Some(0x5A5A));
        assert_eq!(low_word_written(0x103, 1, 0xA5), Some(0xA5A5));
        assert_eq!(low_word_written(0x101, 2, 0x1234), Some(0x3434));
    }

    #[test]
    fn the_mac_prom_xors_to_ff() {
        let mac = [0x08, 0x00, 0x07, 0x12, 0x34, 0x56];
        let prom: Vec<u8> = (0..8).map(|i| mac_prom_byte(mac, i)).collect();
        assert_eq!(prom[..3], [0x10, 0x00, 0xE0]);
        assert_eq!(prom.iter().fold(0, |x, b| x ^ b), 0xFF);
    }
}
