//! Sonora gate array of the Macintosh LC III
//!
//! What is particular to the Sonora: its address map, which is 32-bit only,
//! and the layout of the RAM. The registers which it shares with the V8 are
//! in `pseudovia`.

use super::bus::Region;
use crate::bus::Address;

/// Start of the I/O area
pub(super) const IO_BASE: Address = 0x50F0_0000;

/// Bits of the address which select a register of the Sonora
pub(super) const PSEUDOVIA_MASK: Address = 0x1F;

/// CPU ID register of the Macintosh LC III
pub(super) const CPU_ID_LCIII: u32 = 0xA55A_0001;

/// Finds what an address selects
pub(super) fn decode(addr: Address) -> Option<Region> {
    match addr {
        0x0000_0000..=0x3FFF_FFFF => Some(Region::Ram(addr)),
        0x4000_0000..=0x4FFF_FFFF => Some(Region::Rom),
        0x5000_0000..=0x50FF_FFFF => Some(Region::Io(addr & 0x0F_FFFF)),
        0x5FFF_FFFC..=0x5FFF_FFFF => Some(Region::CpuId(addr as usize & 3)),
        0x6000_0000..=0x6FFF_FFFF => Some(Region::Vram(addr as usize & 0x0F_FFFF)),
        _ => None,
    }
}

/// Translates an address to an index in the RAM, which holds the SIMM
/// followed by the RAM soldered on the logic board. The Sonora has the
/// soldered RAM in a first bank, then two banks for the SIMM.
///
/// `cfg` is the RAM configuration register. With the size code 7, which the
/// ROM sets to find the size of the SIMM, the banks are 16 MB apart. With
/// the code of a size, the SIMM follows the soldered RAM. This is worked out
/// from what the ROM does and has not been verified on hardware.
pub(super) fn ram_index(
    cfg: u8,
    simm_size: usize,
    soldered_size: usize,
    addr: Address,
) -> Option<usize> {
    const MB: usize = 1024 * 1024;

    let addr = addr as usize;
    let soldered_bank = if cfg & 0x40 != 0 { soldered_size } else { 0 };
    if addr < soldered_bank {
        return Some(simm_size + addr);
    }

    match cfg & 0x07 {
        0 => None,
        7 => {
            // The SIMMs of 2, 8 and 32 MB have two banks
            let banks = if simm_size.trailing_zeros() % 2 == 1 {
                2
            } else {
                1
            };
            let bank_size = simm_size / banks;
            let (bank, offset) = (addr / (16 * MB), addr % (16 * MB));
            ((1..=banks).contains(&bank) && offset < bank_size)
                .then(|| (bank - 1) * bank_size + offset)
        }
        code => {
            let offset = addr - soldered_bank;
            (offset < simm_size.min(MB << (code - 1))).then_some(offset)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1024 * 1024;

    /// Index with 4 MB of soldered RAM
    fn index(cfg: u8, simm_mb: usize, addr: Address) -> Option<usize> {
        ram_index(cfg, simm_mb * MB, 4 * MB, addr)
    }

    #[test]
    fn ram_split() {
        // Banks 16 MB apart: soldered RAM, then the two banks of a 32 MB SIMM
        assert_eq!(index(0x7F, 32, 0x000_0000), Some(32 * MB));
        assert_eq!(index(0x7F, 32, 0x03F_FFFF), Some(36 * MB - 1));
        assert_eq!(index(0x7F, 32, 0x040_0000), None);
        assert_eq!(index(0x7F, 32, 0x100_0000), Some(0));
        assert_eq!(index(0x7F, 32, 0x200_0000), Some(16 * MB));
        assert_eq!(index(0x7F, 32, 0x300_0000), None);
        // A SIMM of 4 MB has one bank
        assert_eq!(index(0x7F, 4, 0x13F_FFFF), Some(4 * MB - 1));
        assert_eq!(index(0x7F, 4, 0x140_0000), None);
        assert_eq!(index(0x7F, 4, 0x200_0000), None);
        // A SIMM of 2 MB has two
        assert_eq!(index(0x7F, 2, 0x110_0000), None);
        assert_eq!(index(0x7F, 2, 0x200_0000), Some(MB));
    }

    #[test]
    fn ram_merged() {
        // The SIMM follows the soldered RAM
        assert_eq!(index(0x46, 32, 0x000_0000), Some(32 * MB));
        assert_eq!(index(0x46, 32, 0x040_0000), Some(0));
        assert_eq!(index(0x46, 32, 0x23F_FFFF), Some(32 * MB - 1));
        assert_eq!(index(0x46, 32, 0x240_0000), None);
        // No SIMM
        assert_eq!(index(0x40, 0, 0x03F_FFFF), Some(4 * MB - 1));
        assert_eq!(index(0x40, 0, 0x040_0000), None);
        // No RAM before the register is written
        assert_eq!(index(0x00, 32, 0x000_0000), None);
    }
}
