//! Ethernet controllers

pub mod appl_lc_sonic;
pub mod appl_nb_sonic_tp;
pub mod dayna_si30;
pub mod sonic;

/// A byte of the MAC address PROM: bytes 0-5 are the bytes of the address
/// with their bits reversed, byte 6 is 0, and byte 7 is a check byte that
/// makes the eight bytes XOR to $FF.
///
/// MAME swaps the nibbles of each byte and reverses the bits of the low one
/// only. That gives the right value only for bytes below $10, such as those
/// of Apple's 08:00:07 prefix. Apple's .ENET driver (in the declaration ROM
/// 341-0470 of the LC card, which also drives the Quadras' built-in
/// Ethernet) reverses all eight bits of each byte it reads.
pub(crate) fn mac_prom_byte(mac: [u8; 6], index: usize) -> u8 {
    match index {
        0..=5 => mac[index].reverse_bits(),
        7 => !mac.iter().fold(0, |x, &b| x ^ b.reverse_bits()),
        _ => 0,
    }
}
