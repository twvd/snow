//! V8 gate array of the Macintosh LC and LC II
//!
//! What is particular to the V8: its address map, the layout of the RAM and
//! of the video RAM, and its video modes. Its registers are in `pseudovia`.

use super::bus::{IO_WAITS, Region};
use super::pseudovia::{RegisterExp, RegisterMonP};
use super::video::Mode;
use crate::bus::Address;
use crate::bus::access::{Bus68020, CpuBus};
use crate::mac::MacMonitor;
use crate::tickable::Ticks;

/// Start of the I/O area
pub(super) const IO_BASE: Address = 0xF0_0000;

/// Bits of the address which select a register of the V8
pub(super) const PSEUDOVIA_MASK: Address = 0x13;

/// The V8 starts its RAM cycles, and those of its own registers and of the
/// ASC, on a slot of this many clocks. A power of two.
pub(super) const CYCLE_SLOT: Ticks = 4;
/// Length of a RAM cycle from the start of its slot, in clocks
pub(super) const RAM_CYCLE: Ticks = 6;
/// Wait states of a RAM cycle that starts on a slot
pub(super) const RAM_WAITS: Ticks = RAM_CYCLE - Bus68020::CYCLE;
/// Wait states of a ROM bus cycle
pub(super) const ROM_WAITS: Ticks = 2;

/// Start of the window of the video RAM
const VRAM_BASE: Address = 0xF4_0000;

/// Monitors which the V8 supports
pub const MONITORS: &[MacMonitor] = &[MacMonitor::RGB12, MacMonitor::HiRes14];

/// Sizes of the video RAM SIMM
pub const VRAM_SIZES: &[usize] = &[256 * 1024, 512 * 1024];

/// Finds what an address selects.
///
/// The V8 only decodes A31 and A23-A0. A31 selects the expansion slot,
/// which is not implemented, and is ignored in the 24-bit address map
/// (`fc3` clear in data register B).
pub(super) fn decode(addr: Address, fc3: bool) -> Option<Region> {
    if fc3 && addr & (1 << 31) != 0 {
        return None;
    }

    let addr = addr & 0x00FF_FFFF;
    match addr {
        0x00_0000..=0x9F_FFFF => Some(Region::Ram(addr)),
        0xA0_0000..=0xAF_FFFF => Some(Region::Rom),
        0xF0_0000..=0xF3_FFFF => Some(Region::Io(addr & 0x0F_FFFF)),
        0xF4_0000..=0xFB_FFFF => Some(Region::Vram((addr - VRAM_BASE) as usize)),
        _ => None,
    }
}

/// Translates an address to an index in the RAM, which holds the SIMMs
/// followed by the RAM soldered on the logic board.
///
/// The SIMM bank starts at 0 and the soldered bank follows it, both with
/// the size configured in the expansion register of the V8.
pub(super) fn ram_index(
    exp: RegisterExp,
    simm_size: usize,
    soldered_size: usize,
    addr: Address,
) -> Option<usize> {
    const MB: usize = 1024 * 1024;

    let addr = addr as usize;
    let simm_bank = [0, 2 * MB, 4 * MB, 8 * MB][exp.ram_simm() as usize];
    if addr < simm_bank {
        return (addr < simm_size).then_some(addr);
    }

    let soldered_bank = if exp.ram_soldered_2mb() {
        2 * MB
    } else {
        4 * MB
    };
    // The ROM tells 2 MB of soldered RAM from 4 MB by writing in the lower half
    // of the bank configured as 4 MB, so 2 MB are taken to be in the upper half.
    // This has not been verified on hardware.
    let hole = soldered_bank.saturating_sub(soldered_size);
    let offset = addr - simm_bank;
    (hole..soldered_bank)
        .contains(&offset)
        .then(|| simm_size + offset - hole)
}

/// Translates an offset in the window of the video RAM to an index in the
/// video RAM, which is mirrored in its 512 KB window when it is smaller
pub(super) fn vram_index(vram_size: usize, offset: usize) -> usize {
    offset & (vram_size - 1)
}

/// Length of a frame in cycles of C16M
pub(super) fn frame_period(monitor: MacMonitor) -> Ticks {
    match monitor {
        // 640 x 407 dots at 15.6672 MHz (60.15 Hz)
        MacMonitor::RGB12 => 640 * 407,
        // 864 x 525 dots at 30.24 MHz (66.67 Hz)
        _ => 864 * 525 * 15_667_200 / 30_240_000,
    }
}

/// Video mode set in the expansion and monitor parameters registers
pub(super) fn video_mode(monitor: MacMonitor, exp: RegisterExp, monp: RegisterMonP) -> Mode {
    Mode {
        width: monitor.width(),
        height: monitor.height(),
        depth: monp.depth(),
        row_bytes: if exp.vram_256k() { 512 } else { 1024 },
        direct_palette: true,
    }
}

/// Wait states of an I/O cycle, measured with BusTiming on a real LC II
pub(super) fn io_waits(offset: Address, write: bool) -> Ticks {
    match offset {
        // SCC
        0x0_4000..=0x0_5FFF => 10,
        // SCSI
        0x0_6000..=0x0_7FFF | 0x1_0000..=0x1_3FFF if write => 0,
        0x0_6000..=0x0_7FFF | 0x1_0000..=0x1_3FFF => 2,
        // Sound and V8 registers, in the V8
        0x1_4000..=0x1_5FFF | 0x2_6000..=0x2_7FFF => RAM_WAITS,
        // SWIM
        0x1_6000..=0x1_7FFF => 4,
        _ => IO_WAITS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1024 * 1024;

    fn index(exp: u8, ram_mb: usize, addr: Address) -> Option<usize> {
        ram_index(RegisterExp(exp), (ram_mb - 2) * MB, 2 * MB, addr)
    }

    #[test]
    fn ram_soldered_only() {
        // No SIMM bank, soldered bank of 2 MB
        assert_eq!(index(0x20, 2, 0x00_0000), Some(0));
        assert_eq!(index(0x20, 2, 0x1F_FFFF), Some(2 * MB - 1));
        assert_eq!(index(0x20, 2, 0x20_0000), None);
        assert_eq!(index(0x20, 2, 0x80_0000), None);
    }

    #[test]
    fn ram_soldered_size_probe() {
        // Soldered bank of 4 MB: the lower half does not respond
        assert_eq!(index(0x00, 2, 0x1F_FFFE), None);
        assert_eq!(index(0x00, 2, 0x20_0000), Some(0));
        assert_eq!(index(0xC0, 10, 0x80_0000), None);
        // Soldered bank of 2 MB after a SIMM bank of 8 MB
        assert_eq!(index(0xE0, 10, 0x80_0000), Some(8 * MB));
        assert_eq!(index(0xE0, 10, 0x9F_FFFF), Some(10 * MB - 1));
    }

    #[test]
    fn ram_simm() {
        // SIMM bank of 8 MB with 2 MB fitted
        assert_eq!(index(0xE0, 4, 0x00_0000), Some(0));
        assert_eq!(index(0xE0, 4, 0x1F_FFFF), Some(2 * MB - 1));
        assert_eq!(index(0xE0, 4, 0x20_0000), None);
        assert_eq!(index(0xE0, 4, 0x80_0000), Some(2 * MB));
        // SIMM bank of 2 MB, the soldered bank follows it
        assert_eq!(index(0x60, 4, 0x1F_FFFF), Some(2 * MB - 1));
        assert_eq!(index(0x60, 4, 0x20_0000), Some(2 * MB));
        assert_eq!(index(0x60, 4, 0x3F_FFFF), Some(4 * MB - 1));
        assert_eq!(index(0x60, 4, 0x40_0000), None);
    }

    /// Index with the 4 MB of soldered RAM of the Macintosh LC II
    fn index_lcii(exp: u8, simm_mb: usize, addr: Address) -> Option<usize> {
        ram_index(RegisterExp(exp), simm_mb * MB, 4 * MB, addr)
    }

    #[test]
    fn ram_lcii_soldered_only() {
        // Soldered bank of 4 MB: all of it responds
        assert_eq!(index_lcii(0x00, 0, 0x00_0000), Some(0));
        assert_eq!(index_lcii(0x00, 0, 0x1F_FFFE), Some(2 * MB - 2));
        assert_eq!(index_lcii(0x00, 0, 0x3F_FFFF), Some(4 * MB - 1));
        assert_eq!(index_lcii(0x00, 0, 0x40_0000), None);
    }

    #[test]
    fn ram_lcii_simm() {
        // SIMM bank of 8 MB: the soldered bank follows it at 8 MB, where
        // only its first 2 MB are in the RAM area
        assert_eq!(index_lcii(0xC0, 8, 0x7F_FFFF), Some(8 * MB - 1));
        assert_eq!(index_lcii(0xC0, 8, 0x80_0000), Some(8 * MB));
        assert_eq!(index_lcii(0xC0, 8, 0x9F_FFFF), Some(10 * MB - 1));
        // SIMM bank of 8 MB with 2 MB fitted
        assert_eq!(index_lcii(0xC0, 2, 0x20_0000), None);
        assert_eq!(index_lcii(0xC0, 2, 0x80_0000), Some(2 * MB));
        // SIMM banks of 4 MB and 2 MB
        assert_eq!(index_lcii(0x80, 4, 0x40_0000), Some(4 * MB));
        assert_eq!(index_lcii(0x80, 4, 0x7F_FFFF), Some(8 * MB - 1));
        assert_eq!(index_lcii(0x40, 2, 0x20_0000), Some(2 * MB));
        assert_eq!(index_lcii(0x40, 2, 0x5F_FFFF), Some(6 * MB - 1));
        assert_eq!(index_lcii(0x40, 2, 0x60_0000), None);
    }

    #[test]
    fn vram_mirror() {
        assert_eq!(vram_index(256 * 1024, 256 * 1024), 0);
        assert_eq!(vram_index(512 * 1024, 256 * 1024), 256 * 1024);
        assert_eq!(vram_index(512 * 1024, 512 * 1024 + 1), 1);
    }

    #[test]
    fn frame_rate() {
        // Frames in a second of C16M
        let frames = |monitor| 15_667_200 / frame_period(monitor);
        assert_eq!(frames(MacMonitor::RGB12), 60);
        assert_eq!(frames(MacMonitor::HiRes14), 66);
    }
}
