//! Sonora gate array of the Macintosh LC III
//!
//! What is particular to the Sonora: its address map, which is 32-bit only,
//! the layout of the RAM and of the video RAM, and its video registers for
//! the mode, the depth and the sense lines of the monitor. The registers
//! which it shares with the V8 are in `pseudovia`.
//!
//! The 560x384 mode of the Apple IIe card is not implemented.

use super::bus::{IO_WAITS, Region};
use super::video::Mode;
use crate::bus::Address;
use crate::mac::MacMonitor;
use crate::tickable::Ticks;
use crate::types::Byte;

use serde::{Deserialize, Serialize};

/// Start of the I/O area
pub(super) const IO_BASE: Address = 0x50F0_0000;

/// Bits of the address which select a register of the Sonora
pub(super) const PSEUDOVIA_MASK: Address = 0x1F;

/// Wait states of a RAM read bus cycle
pub(super) const RAM_READ_WAITS: Ticks = 3;
/// Wait states of a RAM write bus cycle
pub(super) const RAM_WRITE_WAITS: Ticks = 0;
/// Wait states of a ROM bus cycle
pub(super) const ROM_WAITS: Ticks = 3;
/// Wait states of a video RAM bus cycle, not documented
pub(super) const VRAM_WAITS: Ticks = 1;

/// C16M ticks into a VIA cycle at which the Sonora releases the CPU
pub(super) const VIA_RELEASE: Ticks = 13;

/// Native ticks from the start of an SCC cycle to the start of the next one
pub(super) const SCC_INTERVAL: Ticks = 12;
/// Native ticks from the start of a SWIM cycle to the start of the next one
/// in ISM mode
pub(super) const SWIM_INTERVAL: Ticks = 10;

/// CPU ID register of the Macintosh LC III
pub(super) const CPU_ID_LCIII: u32 = 0xA55A_0001;

/// Monitors for the Sonora: 512x384, 640x480 (and 640x400, which the system
/// offers on the same monitor), 832x624. The Sonora also supports VGA and
/// the 15" Portrait monitors.
pub const MONITORS: &[MacMonitor] = &[MacMonitor::RGB12, MacMonitor::HiRes14, MacMonitor::RGB16];

/// Sizes of the video RAM: 512 KB are soldered, a SIMM adds 256 KB
pub const VRAM_SIZES: &[usize] = &[VRAM_SOLDERED, 768 * 1024];
const VRAM_SOLDERED: usize = 512 * 1024;

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

/// Translates an offset in the 1 MB window of the video RAM to an index in
/// the video RAM.
///
/// `cfg` is the video RAM configuration register. The SIMM of 256 KB follows
/// the soldered video RAM. When the register says that it has 512 KB, which
/// the ROM sets to find its size, an address line is missing and the bytes
/// repeat after 512. This is worked out from what the ROM does and has not
/// been verified on hardware.
pub(super) fn vram_index(cfg: u8, vram_size: usize, offset: usize) -> Option<usize> {
    let simm_size = vram_size - VRAM_SOLDERED;
    if offset < VRAM_SOLDERED {
        return Some(offset);
    }

    let offset = offset - VRAM_SOLDERED;
    match cfg & 3 {
        _ if simm_size == 0 => None,
        1 => (offset < simm_size).then_some(VRAM_SOLDERED + offset),
        2 => {
            let offset = ((offset >> 10) << 9) | (offset & 0x1FF);
            Some(VRAM_SOLDERED + offset % simm_size)
        }
        _ => None,
    }
}

const VIDEO_MODE: Address = 0;
const VIDEO_DEPTH: Address = 1;
const VIDEO_SENSE: Address = 2;

/// Video is blanked, in the mode register
const VIDEO_MODE_BLANK: u8 = 0x80;

/// Video registers of the Sonora
#[derive(Serialize, Deserialize)]
pub(super) struct VideoRegs {
    /// Mode register: monitor code and blanking
    mode: u8,

    /// Depth register
    depth: u8,

    /// Sense register: the sense lines which are driven low have their bit
    /// clear
    sense: u8,
}

impl VideoRegs {
    pub(super) fn new() -> Self {
        Self {
            mode: VIDEO_MODE_BLANK,
            depth: 0,
            sense: 7,
        }
    }

    /// Size of the picture and length of a frame in cycles of C16M, for
    /// the mode set in the mode register
    fn timing(&self) -> Option<(usize, usize, Ticks)> {
        match self.mode & 0x1F {
            // 12" RGB: 640 x 407 dots at 15.6672 MHz (60.15 Hz)
            0x02 => Some((512, 384, 640 * 407)),
            // 13" RGB: 864 x 525 dots at 30.24 MHz (66.67 Hz)
            0x06 => Some((640, 480, 864 * 525 * 15_667_200 / 30_240_000)),
            // 13" RGB with 400 lines, which leaves room for 16 bpp in 512 KB,
            // with the same timing
            0x0D => Some((640, 400, 864 * 525 * 15_667_200 / 30_240_000)),
            // 15" Portrait: 832 x 918 dots at 57.2832 MHz (75 Hz)
            0x01 => Some((640, 870, 832 * 918 * 15_667_200 / 57_283_200)),
            // 16" RGB: 1152 x 667 dots at 57.2832 MHz (74.55 Hz)
            0x09 => Some((832, 624, 1152 * 667 * 15_667_200 / 57_283_200)),
            // VGA: 800 x 525 dots at 25.175 MHz (59.94 Hz)
            0x0B => Some((640, 480, 800 * 525 * 15_667_200 / 25_175_000)),
            _ => None,
        }
    }

    /// Length of a frame in cycles of C16M
    pub(super) fn frame_period(&self) -> Ticks {
        self.timing().map_or(640 * 407, |(_, _, period)| period)
    }

    /// Video mode set in the registers, if the video is not blanked. The
    /// lines of pixels follow each other in video RAM.
    pub(super) fn video_mode(&self) -> Option<Mode> {
        let (width, height, _) = self
            .timing()
            .filter(|_| self.mode & VIDEO_MODE_BLANK == 0 && self.depth <= 4)?;
        Some(Mode {
            width,
            height,
            depth: self.depth,
            row_bytes: (width << self.depth) / 8,
            // Whether the Sonora uses the colour table at 16 bpp has not
            // been verified
            direct_palette: false,
        })
    }

    pub(super) fn read(&self, addr: Address, monitor: MacMonitor) -> Byte {
        match addr & 7 {
            VIDEO_MODE => self.mode,
            VIDEO_DEPTH => self.depth,
            VIDEO_SENSE => {
                // What the lines read with nothing driven, then with each
                // of them driven low
                let sense = monitor.sense();
                let lines = (0..3)
                    .filter(|line| self.sense & (4 >> line) == 0)
                    .fold(sense[0], |lines, line| lines & sense[1 + line]);
                self.sense | (lines << 4)
            }
            // The other registers, among which the position of the beam,
            // are not implemented
            _ => 0,
        }
    }

    pub(super) fn write(&mut self, addr: Address, val: Byte) {
        match addr & 7 {
            VIDEO_MODE => self.mode = val & 0x9F,
            VIDEO_DEPTH => self.depth = val & 7,
            VIDEO_SENSE => self.sense = val & 0x0F,
            _ => (),
        }
    }
}

/// Wait states of an I/O cycle, measured with BusTiming on a real LC III
pub(super) fn io_waits(offset: Address, write: bool) -> Ticks {
    match offset {
        // SCC
        0x0_4000..=0x0_5FFF => 6,
        // SCSI
        0x0_6000..=0x0_7FFF | 0x1_0000..=0x1_3FFF if write => 1,
        0x0_6000..=0x0_7FFF | 0x1_0000..=0x1_3FFF => 3,
        // Sound and Sonora registers
        0x1_4000..=0x1_5FFF | 0x2_6000..=0x2_7FFF => 2,
        // SWIM
        0x1_6000..=0x1_7FFF => 3,
        _ => IO_WAITS,
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

    #[test]
    fn vram() {
        const K: usize = 1024;

        // Soldered video RAM only: nothing in the second half of the window
        assert_eq!(vram_index(6, 512 * K, 0), Some(0));
        assert_eq!(vram_index(6, 512 * K, 512 * K - 1), Some(512 * K - 1));
        assert_eq!(vram_index(6, 512 * K, 512 * K), None);

        // SIMM of 256 KB taken for one of 512 KB: the bytes repeat after 512
        assert_eq!(vram_index(6, 768 * K, 512 * K), Some(512 * K));
        assert_eq!(vram_index(6, 768 * K, 512 * K + 512), Some(512 * K));
        assert_eq!(vram_index(6, 768 * K, 512 * K + 1024), Some(512 * K + 512));
        // And with its real size
        assert_eq!(vram_index(5, 768 * K, 512 * K + 512), Some(512 * K + 512));
        assert_eq!(vram_index(5, 768 * K, 768 * K), None);
        // Not configured
        assert_eq!(vram_index(4, 768 * K, 512 * K), None);
    }

    #[test]
    fn video_sense() {
        // Nothing driven: the lines of the monitor
        let mut video = VideoRegs::new();
        video.write(VIDEO_SENSE, 7);
        assert_eq!(video.read(VIDEO_SENSE, MacMonitor::HiRes14), 0x67);

        // The 16" monitor reads as 7 and connects lines to each other
        assert_eq!(video.read(VIDEO_SENSE, MacMonitor::RGB16), 0x77);
        video.write(VIDEO_SENSE, 3);
        assert_eq!(video.read(VIDEO_SENSE, MacMonitor::RGB16), 0x23);
        video.write(VIDEO_SENSE, 5);
        assert_eq!(video.read(VIDEO_SENSE, MacMonitor::RGB16), 0x55);
        video.write(VIDEO_SENSE, 6);
        assert_eq!(video.read(VIDEO_SENSE, MacMonitor::RGB16), 0x26);
    }

    #[test]
    fn video_modes() {
        let mut video = VideoRegs::new();

        // No mode after reset
        assert!(video.timing().is_none());
        assert!(video.video_mode().is_none());

        // 832x624 at 1 bpp: the lines are 104 bytes each
        video.write(VIDEO_MODE, 0x09);
        video.write(VIDEO_DEPTH, 0);
        let mode = video.video_mode().unwrap();
        assert_eq!((mode.width, mode.height), (832, 624));
        assert_eq!((mode.depth, mode.row_bytes), (0, 104));

        // 640x400 at 16 bpp
        video.write(VIDEO_MODE, 0x0D);
        video.write(VIDEO_DEPTH, 4);
        let mode = video.video_mode().unwrap();
        assert_eq!((mode.width, mode.height), (640, 400));
        assert_eq!((mode.depth, mode.row_bytes), (4, 1280));

        // Blanked
        video.write(VIDEO_MODE, 0x8D);
        assert!(video.video_mode().is_none());
    }

    #[test]
    fn frame_rate() {
        // Frames in a second of C16M
        let frames = |mode| {
            let mut video = VideoRegs::new();
            video.write(VIDEO_MODE, mode);
            15_667_200 / video.frame_period()
        };
        assert_eq!(frames(0x02), 60);
        assert_eq!(frames(0x06), 66);
        assert_eq!(frames(0x0D), 66);
        assert_eq!(frames(0x01), 75);
        assert_eq!(frames(0x09), 74);
        assert_eq!(frames(0x0B), 59);
    }
}
