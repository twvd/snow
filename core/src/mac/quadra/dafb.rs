//! Apple DAFB (Direct Access Frame Buffer)
//!
//! Integrated graphics used in several Quadra models. Address space and interrupt
//! line sits in a NuBus slot, but has no declaration ROM.
//!
//! References:
//! * https://bitsavers.org/pdf/apple/mac/video/DAFBII_ACDC.pdf
//! * https://68kmla.org/bb/threads/24-bit-graphics-on-wombats-dafb.45639/

use std::fmt::Display;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::mac::MacMonitor;
use crate::renderer::{DisplayBuffer, Renderer, render_packed};
use crate::tickable::{Tickable, Ticks};
use crate::types::Field32;

/// Which chip the frame buffer controller is in
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DafbKind {
    /// DAFB of the Quadra 700, with the AC842 RAMDAC
    Dafb,
    /// DAFB II cell of MEMCjr or djMEMC, with the Antelope RAMDAC (LC 475,
    /// Quadra 800)
    MemcJr,
}

/// Monitors which the DAFB of the Quadra 700 supports
pub const MONITORS: &[MacMonitor] = &[
    MacMonitor::RGB12,
    MacMonitor::HiRes14,
    MacMonitor::RGB16,
    MacMonitor::RGB21,
    MacMonitor::PortraitBW,
    MacMonitor::TwoPageBW,
    MacMonitor::VGA,
];

/// Monitors which the DAFB II cell of MEMCjr and djMEMC supports, the same and
/// the 19" RGB
pub const MONITORS_MEMCJR: &[MacMonitor] = &[
    MacMonitor::RGB12,
    MacMonitor::HiRes14,
    MacMonitor::RGB16,
    MacMonitor::RGB19,
    MacMonitor::RGB21,
    MacMonitor::PortraitBW,
    MacMonitor::TwoPageBW,
    MacMonitor::VGA,
];

#[derive(Debug, Eq, PartialEq, Clone, Copy, strum::IntoStaticStr, Serialize, Deserialize)]
pub enum Bpp {
    /// 1bpp (black & white)
    One,

    /// 2bpp paletted (4 colors)
    Two,

    /// 4bpp paletted (16 colors)
    Four,

    /// 8bpp paletted (256 colors)
    Eight,

    /// 16-bit direct color, x555 ('Thousands' of colors, Antelope)
    Sixteen,

    /// 24-bit direct color ('Millions' of colors)
    TwentyFour,
}

/// Geometry of the currently programmed video mode
struct Geometry {
    width: usize,
    height: usize,
    /// Scanline stride in VRAM, in bytes
    stride: usize,
    /// Offset of the first visible pixel in VRAM, in bytes
    base: usize,
    /// Offset of the second line from the first, in bytes
    second_line: usize,
}

impl Geometry {
    /// Offset in VRAM of the first pixel of a line, in bytes
    fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            self.base
        } else {
            self.base + self.second_line + (line - 1) * self.stride
        }
    }
}

/// Apple DAFB video controller
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Dafb<TRenderer: Renderer> {
    #[serde(skip)]
    pub renderer: Option<TRenderer>,

    monitor: MacMonitor,

    kind: DafbKind,

    pub vram: Vec<u8>,

    /// Framebuffer base address, bits 20-9
    base_hi: Field32,
    /// Framebuffer base address, bits 8-5
    base_lo: Field32,
    /// Scanline stride, in 32-bit words
    stride: Field32,
    /// Timing control register
    timing_control: Field32,
    /// Configuration register (convolution/interlace)
    config: Field32,
    /// Block write control register
    block_control: Field32,
    /// Monitor sense lines driven by the DAFB (active low in the register)
    sense: Field32,
    /// Test register
    test: Field32,

    /// 'Turbo SCSI' config, part of the DAFB block
    turbo_scsi: Field32,
    /// DREQ of the 53C96, read back in the Turbo SCSI register
    #[serde(skip)]
    pub(crate) scsi_drq: bool,

    /// Swatch mode register
    swatch_mode: Field32,
    /// Swatch test register (unused by the hardware, scratch space for the driver)
    swatch_test: Field32,
    /// Interrupt enable register
    int_enable: Field32,
    /// Interrupt status register
    int_status: Field32,
    /// Scanline to generate the cursor interrupt on
    cursor_line: Field32,
    /// Scanline to generate the animation interrupt on
    anim_line: Field32,
    hparams: [Field32; 10],
    vparams: [Field32; 7],

    /// Palette, entries as 0x00BBGGRR
    pub palette: Vec<u32>,
    /// Palette entry currently being addressed
    pal_address: u8,
    /// Color component within the addressed palette entry (R, G, B)
    pal_idx: u8,
    /// AC842 RAMDAC pixel bus control register
    pbctrl: u8,
    /// Second pixel bus control register of the Antelope
    pcbr1: u8,

    vblank_ticks: Ticks,
}

impl<TRenderer> Dafb<TRenderer>
where
    TRenderer: Renderer,
{
    pub const VRAM_SIZE: usize = 0x20_0000;

    /// Value of the version field of the test register
    const fn version(&self) -> u32 {
        match self.kind {
            DafbKind::Dafb => 1,
            DafbKind::MemcJr => 3,
        }
    }

    /// The Turbo SCSI registers are in the discrete DAFB. On MEMCjr the block
    /// moved to PrimeTime, where the ROM uses other addresses, and what a
    /// real machine answers at these is not known.
    const fn has_turbo_scsi(&self) -> bool {
        matches!(self.kind, DafbKind::Dafb)
    }

    /// Version in PCBR1
    const ANTELOPE_VERSION: u8 = 0x02;

    /// Horizontal serration pulse location
    const HSERR: usize = 0;
    /// Horizontal active line (start of the active display area)
    const HAL: usize = 7;
    /// Horizontal front porch (end of the active display area)
    const HFP: usize = 8;
    /// Horizontal pixels (total pixel locations in a line minus 2)
    const HPIX: usize = 9;

    /// Vertical half-lines
    const VHLINE: usize = 0;
    /// Vertical active lines (end of the active display area)
    const VAL: usize = 4;
    /// Vertical front porch
    const VFP: usize = 5;
    /// Vertical front porch equalization
    const VFPEQ: usize = 6;

    /// Largest resolution accepted
    const MAX_WIDTH: usize = 1280;
    const MAX_HEIGHT: usize = 1024;

    const INT_VBL: u32 = 1 << 0;
    const INT_CURSOR: u32 = 1 << 2;

    /// Sense register after reset: no sense line driven
    const SENSE_UNDRIVEN: u32 = 0b111;

    pub fn new(renderer: TRenderer, monitor: MacMonitor, kind: DafbKind, vram_size: usize) -> Self {
        Self {
            renderer: Some(renderer),
            monitor,
            kind,
            vram: vec![0; vram_size],

            base_hi: Field32(0),
            base_lo: Field32(0),
            stride: Field32(0),
            timing_control: Field32(0),
            config: Field32(0),
            block_control: Field32(0),
            sense: Field32(Self::SENSE_UNDRIVEN),
            test: Field32(0),
            turbo_scsi: Field32(0),
            scsi_drq: false,

            swatch_mode: Field32(0),
            swatch_test: Field32(0),
            int_enable: Field32(0),
            int_status: Field32(0),
            cursor_line: Field32(0),
            anim_line: Field32(0),
            hparams: [Field32(0); 10],
            vparams: [Field32(0); 7],

            palette: vec![0; 256],
            pal_address: 0,
            pal_idx: 0,
            pbctrl: 0,
            pcbr1: 0,

            vblank_ticks: 0,
        }
    }

    pub fn reset(&mut self) {
        self.base_hi = Field32(0);
        self.base_lo = Field32(0);
        self.stride = Field32(0);
        self.timing_control = Field32(0);
        self.config = Field32(0);
        self.block_control = Field32(0);
        self.sense = Field32(Self::SENSE_UNDRIVEN);
        self.swatch_mode = Field32(0);
        self.int_enable = Field32(0);
        self.int_status = Field32(0);
        self.pbctrl = 0;
        self.pcbr1 = 0;
        self.pal_idx = 0;
        self.hparams = [Field32(0); 10];
        self.vparams = [Field32(0); 7];
    }

    /// Framebuffer base address in VRAM, in bytes
    fn base(&self) -> u32 {
        ((self.base_hi.0 & 0xFFF) << 9) | ((self.base_lo.0 & 0x0F) << 5)
    }

    /// Scanline stride, in bytes
    fn stride(&self) -> u32 {
        (self.stride.0 & 0xFFF) << 2
    }

    /// Monitor sense lines currently driven (active high)
    fn sense_drive(&self) -> u8 {
        ((self.sense.0 & 0b111) ^ 0b111) as u8
    }

    pub fn get_irq(&self) -> bool {
        self.int_status.0 != 0
    }

    /// Whether a pseudo-DMA access to the 53C96 waits for DREQ (Turbo SCSI
    /// bit 7 for reads, bit 8 for writes)
    pub fn scsi_drq_check(&self, write: bool) -> bool {
        self.turbo_scsi.0 & (1 << if write { 8 } else { 7 }) != 0
    }

    /// Currently configured color depth
    pub fn bpp(&self) -> Bpp {
        if self.kind == DafbKind::MemcJr && self.pcbr1 & 0xC0 == 0xC0 && self.pbctrl & 0x06 == 0x06
        {
            return Bpp::Sixteen;
        }
        match self.pbctrl & 0x1C {
            0x00 => Bpp::One,
            0x08 => Bpp::Two,
            0x10 => Bpp::Four,
            0x18 => Bpp::Eight,
            0x1C => Bpp::TwentyFour,
            _ => Bpp::One,
        }
    }

    fn read_sense(&self) -> u8 {
        let msense = self.monitor.sense();
        let drive = self.sense_drive();
        let mut sense = msense[0];
        if drive & 0b100 != 0 {
            sense &= msense[1] | 0b100;
        }
        if drive & 0b010 != 0 {
            sense &= msense[2] | 0b010;
        }
        if drive & 0b001 != 0 {
            sense &= msense[3] | 0b001;
        }

        // The register reads the inverse of the sense lines
        sense ^ 0b111
    }

    /// Calculates the geometry of the currently programmed video mode from the
    /// CRTC timing parameters.
    fn geometry(&self) -> Option<Geometry> {
        if self.hparams[Self::HPIX].0 == 0 || self.vparams[Self::VFPEQ].0 == 0 {
            // Timing not programmed (yet)
            return None;
        }

        let mut width = (self.hparams[Self::HFP]
            .0
            .checked_sub(self.hparams[Self::HAL].0)?) as usize;
        let mut height = ((self.vparams[Self::VFP].0 >> 1)
            .checked_sub(self.vparams[Self::VAL].0 >> 1)?) as usize;
        let base = self.base() as usize;

        let convolution = self.config.0 & (1 << 3) != 0;
        let clockdiv = 1usize << ((self.pbctrl & 0x60) >> 5);
        let stride = if convolution {
            1024
        } else {
            self.stride() as usize
        };
        if convolution {
            width /= clockdiv;

            // All modes with convolution enabled overstate the horizontal resolution by 23 pixels
            width = width.checked_sub(23)?;
        } else {
            width *= clockdiv;
        }

        // Interlaced modes
        if self.config.0 & (1 << 2) != 0 {
            height <<= 1;
        }

        if width == 0
            || height == 0
            || width > Self::MAX_WIDTH
            || height > Self::MAX_HEIGHT
            || stride == 0
        {
            return None;
        }

        // On the 12" monitor at 1 bpp, the video driver of the Quadra 700 ROM programs the base
        // 512 bytes before the screen and one more line at the top, which it fills with black,
        // probably to work around an issue of the chip. Show that line from the base, then the
        // screen.
        let second_line = if self.kind == DafbKind::Dafb && self.bpp() == Bpp::One && width == 512 {
            512
        } else {
            stride
        };

        Some(Geometry {
            width,
            height,
            stride,
            base,
            second_line,
        })
    }

    /// The RGBA pixel of each of the first N palette entries, in grayscale
    /// (the blue channel) on a monochrome monitor
    fn palette_lut<const N: usize>(&self) -> [[u8; 4]; N] {
        let color = self.monitor.has_color();
        std::array::from_fn(|i| {
            let c = self.palette[i];
            if color {
                [c as u8, (c >> 8) as u8, (c >> 16) as u8, 0xFF]
            } else {
                let b = (c >> 16) as u8;
                [b, b, b, 0xFF]
            }
        })
    }

    /// Renders the currently displayed frame to the target DisplayBuffer
    pub fn render_to(&self, buf: &mut DisplayBuffer) {
        // Display disable bit
        let geo = if self.swatch_mode.0 & 1 != 0 {
            None
        } else {
            self.geometry()
        };
        let Some(geo) = geo else {
            buf.set_size(self.monitor.width(), self.monitor.height());
            buf.fill(0);
            return;
        };

        buf.set_size(geo.width, geo.height);

        let bpp = self.bpp();

        // Amount of bytes of VRAM consumed by one visible scanline
        let rowbytes = match bpp {
            Bpp::One => geo.width.div_ceil(8),
            Bpp::Two => geo.width.div_ceil(4),
            Bpp::Four => geo.width.div_ceil(2),
            Bpp::Eight => geo.width,
            Bpp::Sixteen => geo.width * 2,
            Bpp::TwentyFour => geo.width * 4,
        };

        match bpp {
            Bpp::One => self.render_indexed::<2>(&geo, rowbytes, buf),
            Bpp::Two => self.render_indexed::<4>(&geo, rowbytes, buf),
            Bpp::Four => self.render_indexed::<16>(&geo, rowbytes, buf),
            Bpp::Eight => self.render_indexed::<256>(&geo, rowbytes, buf),
            Bpp::Sixteen => self.render_lines(&geo, rowbytes, buf, |fb, line| {
                let expand = |v: u16| ((v & 0x1F) as u8) << 3 | ((v & 0x1F) as u8) >> 2;
                for (px, src) in line.chunks_exact_mut(4).zip(fb.chunks_exact(2)) {
                    let c = u16::from_be_bytes([src[0], src[1]]);
                    px.copy_from_slice(&[expand(c >> 10), expand(c >> 5), expand(c), 0xFF]);
                }
            }),
            Bpp::TwentyFour => self.render_lines(&geo, rowbytes, buf, |fb, line| {
                for (px, src) in line.chunks_exact_mut(4).zip(fb.chunks_exact(4)) {
                    px.copy_from_slice(&[src[1], src[2], src[3], 0xFF]);
                }
            }),
        }
    }

    /// Renders the visible lines of pixels of log2(N) bits through the palette
    fn render_indexed<const N: usize>(&self, geo: &Geometry, rowbytes: usize, buf: &mut [u8]) {
        let lut = self.palette_lut::<N>();
        self.render_lines(geo, rowbytes, buf, |fb, line| {
            render_packed(fb, line, &lut);
        });
    }

    /// Renders the visible lines, each from the `rowbytes` of VRAM it starts at
    fn render_lines(
        &self,
        geo: &Geometry,
        rowbytes: usize,
        buf: &mut [u8],
        mut render_line: impl FnMut(&[u8], &mut [u8]),
    ) {
        if geo.width == 0 {
            return;
        }
        for (y, line) in buf.chunks_exact_mut(geo.width * 4).enumerate() {
            let row = geo.line_start(y);
            if row + rowbytes > self.vram.len() {
                // ??? Dunno if this happens
                break;
            }
            render_line(&self.vram[row..(row + rowbytes)], line);
        }
    }

    pub fn blank(&mut self) -> Result<()> {
        self.vram.fill(0);
        self.render()?;
        Ok(())
    }

    pub fn render(&mut self) -> Result<()> {
        // We have to move the renderer so we don't upset the borrow checker.
        let mut renderer = self.renderer.take().unwrap();
        self.render_to(renderer.buffer_mut());
        self.renderer = Some(renderer);
        self.renderer.as_mut().unwrap().update()?;
        Ok(())
    }

    /// Writes a byte of a palette entry. Written as R, G, B in sequence.
    fn palette_write(&mut self, val: u8) {
        let component = u32::from(val);
        let entry = &mut self.palette[usize::from(self.pal_address)];
        if self.monitor.has_color() {
            match self.pal_idx {
                0 => *entry = (*entry & 0xFF_FF00) | component,
                1 => *entry = (*entry & 0xFF_00FF) | (component << 8),
                _ => *entry = (*entry & 0x00_FFFF) | (component << 16),
            }
        } else if self.pal_idx == 2 {
            // Monochrome monitors only carry the blue channel
            *entry = component * 0x01_0101;
        }

        self.pal_idx += 1;
        if self.pal_idx == 3 {
            self.pal_idx = 0;
            self.pal_address = self.pal_address.wrapping_add(1);
        }
    }

    /// The Antelope has a second register at the address of the first, while
    /// the palette address is 1 and bits 1-2 of the first are set
    fn pcbr1_selected(&self) -> bool {
        self.kind == DafbKind::MemcJr && self.pal_address == 1 && self.pbctrl & 0x06 == 0x06
    }

    /// Reads the addressed component of the addressed palette entry.
    fn palette_read(&mut self) -> u8 {
        let entry = self.palette[usize::from(self.pal_address)];
        let component = match self.pal_idx {
            0 => entry & 0xFF,
            1 => (entry >> 8) & 0xFF,
            _ => (entry >> 16) & 0xFF,
        };
        self.pal_idx += 1;
        if self.pal_idx == 3 {
            self.pal_idx = 0;
            self.pal_address = self.pal_address.wrapping_add(1);
        }
        component as u8
    }
}

impl<TRenderer> BusMember<Address> for Dafb<TRenderer>
where
    TRenderer: Renderer,
{
    #[allow(clippy::match_overlapping_arm)]
    fn read(&mut self, addr: Address) -> Option<u8> {
        match addr {
            // VRAM
            0x00_0000..=0x1F_FFFF => Some(self.vram.get(addr as usize).copied().unwrap_or(0)),

            // Framebuffer base, bits 20-9
            0x80_0000 => Some(self.base_hi.be0()),
            0x80_0001 => Some(self.base_hi.be1()),
            0x80_0002 => Some(self.base_hi.be2()),
            0x80_0003 => Some(self.base_hi.be3()),
            // Framebuffer base, bits 8-5
            0x80_0004 => Some(self.base_lo.be0()),
            0x80_0005 => Some(self.base_lo.be1()),
            0x80_0006 => Some(self.base_lo.be2()),
            0x80_0007 => Some(self.base_lo.be3()),
            // Framebuffer stride, in 32-bit words
            0x80_0008 => Some(self.stride.be0()),
            0x80_0009 => Some(self.stride.be1()),
            0x80_000A => Some(self.stride.be2()),
            0x80_000B => Some(self.stride.be3()),
            // Timing control
            0x80_000C => Some(self.timing_control.be0()),
            0x80_000D => Some(self.timing_control.be1()),
            0x80_000E => Some(self.timing_control.be2()),
            0x80_000F => Some(self.timing_control.be3()),
            // Configuration
            0x80_0010 => Some(self.config.be0()),
            0x80_0011 => Some(self.config.be1()),
            0x80_0012 => Some(self.config.be2()),
            0x80_0013 => Some(self.config.be3()),
            // Block write control
            0x80_0014 => Some(self.block_control.be0()),
            0x80_0015 => Some(self.block_control.be1()),
            0x80_0016 => Some(self.block_control.be2()),
            0x80_0017 => Some(self.block_control.be3()),
            // Monitor sense
            0x80_001C..=0x80_001E => Some(0),
            0x80_001F => Some(self.read_sense()),
            // 'Turbo SCSI' config/handshake
            0x80_0024 if self.has_turbo_scsi() => Some(self.turbo_scsi.be0()),
            0x80_0025 if self.has_turbo_scsi() => Some(self.turbo_scsi.be1()),
            // Bit 9 is the 53C96 DREQ
            0x80_0026 if self.has_turbo_scsi() => {
                Some(Field32((self.turbo_scsi.0 & 0x1FF) | (u32::from(self.scsi_drq) << 9)).be2())
            }
            0x80_0027 if self.has_turbo_scsi() => Some(self.turbo_scsi.be3()),
            // Test register
            0x80_002C => Some(self.test.be0()),
            0x80_002D => Some(self.test.be1()),
            0x80_002E => Some(Field32((self.test.0 & 0x1FF) | (self.version() << 9)).be2()),
            0x80_002F => Some(self.test.be3()),

            // Interrupt status
            0x80_0108 => Some(self.int_status.be0()),
            0x80_0109 => Some(self.int_status.be1()),
            0x80_010A => Some(self.int_status.be2()),
            0x80_010B => Some(self.int_status.be3()),
            // Cursor scanline interrupt ack
            0x80_010C..=0x80_010E => Some(0),
            0x80_010F => {
                self.int_status.0 &= !Self::INT_CURSOR;
                Some(0)
            }
            // VBlank interrupt ack
            0x80_0114..=0x80_0116 => Some(0),
            0x80_0117 => {
                self.int_status.0 &= !Self::INT_VBL;
                Some(0)
            }
            // Cursor scanline
            0x80_0118 => Some(self.cursor_line.be0()),
            0x80_0119 => Some(self.cursor_line.be1()),
            0x80_011A => Some(self.cursor_line.be2()),
            0x80_011B => Some(self.cursor_line.be3()),
            // Animation scanline
            0x80_011C => Some(self.anim_line.be0()),
            0x80_011D => Some(self.anim_line.be1()),
            0x80_011E => Some(self.anim_line.be2()),
            0x80_011F => Some(self.anim_line.be3()),
            // Swatch test register
            0x80_0120 => Some(self.swatch_test.be0()),
            0x80_0121 => Some(self.swatch_test.be1()),
            0x80_0122 => Some(self.swatch_test.be2()),
            0x80_0123 => Some(self.swatch_test.be3()),
            // Timing stuff
            0x80_0124..=0x80_014B => {
                let idx = ((addr - 0x80_0124) / 4) as usize;
                Some(self.hparams[idx].be((addr & 3) as usize))
            }
            0x80_014C..=0x80_0167 => {
                let idx = ((addr - 0x80_014C) / 4) as usize;
                Some(self.vparams[idx].be((addr & 3) as usize))
            }

            // RAMDAC palette address
            0x80_0200..=0x80_0202 => Some(0),
            0x80_0203 => {
                self.pal_idx = 0;
                Some(self.pal_address)
            }
            // RAMDAC palette data
            0x80_0210..=0x80_0212 => Some(0),
            0x80_0213 => Some(self.palette_read()),
            // RAMDAC pixel bus control
            0x80_0220..=0x80_0222 => Some(0),
            0x80_0223 if self.pcbr1_selected() => Some(self.pcbr1),
            0x80_0223 => Some(self.pbctrl),

            0x80_0000..=0x80_03FF => Some(0),
            _ => None,
        }
    }

    #[allow(clippy::match_overlapping_arm)]
    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        match addr {
            // VRAM
            0x00_0000..=0x1F_FFFF => {
                if let Some(b) = self.vram.get_mut(addr as usize) {
                    *b = val;
                }
                Some(())
            }

            // Framebuffer base, bits 20-9
            0x80_0000 => Some(self.base_hi.set_be0(val)),
            0x80_0001 => Some(self.base_hi.set_be1(val)),
            0x80_0002 => Some(self.base_hi.set_be2(val)),
            0x80_0003 => Some(self.base_hi.set_be3(val)),
            // Framebuffer base, bits 8-5
            0x80_0004 => Some(self.base_lo.set_be0(val)),
            0x80_0005 => Some(self.base_lo.set_be1(val)),
            0x80_0006 => Some(self.base_lo.set_be2(val)),
            0x80_0007 => Some(self.base_lo.set_be3(val)),
            // Framebuffer stride, in 32-bit words
            0x80_0008 => Some(self.stride.set_be0(val)),
            0x80_0009 => Some(self.stride.set_be1(val)),
            0x80_000A => Some(self.stride.set_be2(val)),
            0x80_000B => Some(self.stride.set_be3(val)),
            // Timing control
            0x80_000C => Some(self.timing_control.set_be0(val)),
            0x80_000D => Some(self.timing_control.set_be1(val)),
            0x80_000E => Some(self.timing_control.set_be2(val)),
            0x80_000F => Some(self.timing_control.set_be3(val)),
            // Configuration
            0x80_0010 => Some(self.config.set_be0(val)),
            0x80_0011 => Some(self.config.set_be1(val)),
            0x80_0012 => Some(self.config.set_be2(val)),
            0x80_0013 => Some(self.config.set_be3(val)),
            // Block write control
            0x80_0014 => Some(self.block_control.set_be0(val)),
            0x80_0015 => Some(self.block_control.set_be1(val)),
            0x80_0016 => Some(self.block_control.set_be2(val)),
            0x80_0017 => Some(self.block_control.set_be3(val)),
            // Drive monitor sense lines (active low)
            0x80_001C => Some(self.sense.set_be0(val)),
            0x80_001D => Some(self.sense.set_be1(val)),
            0x80_001E => Some(self.sense.set_be2(val)),
            0x80_001F => Some(self.sense.set_be3(val)),
            // Turbo SCSI
            0x80_0024 if self.has_turbo_scsi() => Some(self.turbo_scsi.set_be0(val)),
            0x80_0025 if self.has_turbo_scsi() => Some(self.turbo_scsi.set_be1(val)),
            0x80_0026 if self.has_turbo_scsi() => Some(self.turbo_scsi.set_be2(val)),
            0x80_0027 if self.has_turbo_scsi() => Some(self.turbo_scsi.set_be3(val)),
            // Test register
            0x80_002C => Some(self.test.set_be0(val)),
            0x80_002D => Some(self.test.set_be1(val)),
            0x80_002E => Some(self.test.set_be2(val)),
            0x80_002F => Some(self.test.set_be3(val)),

            // Swatch mode
            0x80_0100 => Some(self.swatch_mode.set_be0(val)),
            0x80_0101 => Some(self.swatch_mode.set_be1(val)),
            0x80_0102 => Some(self.swatch_mode.set_be2(val)),
            0x80_0103 => Some(self.swatch_mode.set_be3(val)),
            // Interrupt enable
            0x80_0104 => Some(self.int_enable.set_be0(val)),
            0x80_0105 => Some(self.int_enable.set_be1(val)),
            0x80_0106 => Some(self.int_enable.set_be2(val)),
            0x80_0107 => {
                self.int_enable.set_be3(val);
                self.int_status.0 &= self.int_enable.0;
                Some(())
            }
            // Cursor scanline interrupt ack
            0x80_010C..=0x80_010E => Some(()),
            0x80_010F => {
                self.int_status.0 &= !Self::INT_CURSOR;
                Some(())
            }
            // VBlank interrupt ack
            0x80_0114..=0x80_0116 => Some(()),
            0x80_0117 => {
                self.int_status.0 &= !Self::INT_VBL;
                Some(())
            }
            // Cursor scanline
            0x80_0118 => Some(self.cursor_line.set_be0(val)),
            0x80_0119 => Some(self.cursor_line.set_be1(val)),
            0x80_011A => Some(self.cursor_line.set_be2(val)),
            0x80_011B => Some(self.cursor_line.set_be3(val)),
            // Animation scanline
            0x80_011C => Some(self.anim_line.set_be0(val)),
            0x80_011D => Some(self.anim_line.set_be1(val)),
            0x80_011E => Some(self.anim_line.set_be2(val)),
            0x80_011F => Some(self.anim_line.set_be3(val)),
            // Swatch test register
            0x80_0120 => Some(self.swatch_test.set_be0(val)),
            0x80_0121 => Some(self.swatch_test.set_be1(val)),
            0x80_0122 => Some(self.swatch_test.set_be2(val)),
            0x80_0123 => Some(self.swatch_test.set_be3(val)),
            // Timing parameters, addressed as arrays
            0x80_0124..=0x80_014B => {
                let idx = ((addr - 0x80_0124) / 4) as usize;
                Some(self.hparams[idx].set_be((addr & 3) as usize, val))
            }
            0x80_014C..=0x80_0167 => {
                let idx = ((addr - 0x80_014C) / 4) as usize;
                Some(self.vparams[idx].set_be((addr & 3) as usize, val))
            }

            // RAMDAC palette address
            0x80_0200..=0x80_0202 => Some(()),
            0x80_0203 => {
                self.pal_address = val;
                self.pal_idx = 0;
                Some(())
            }
            // RAMDAC palette data
            0x80_0210..=0x80_0212 => Some(()),
            0x80_0213 => {
                self.palette_write(val);
                Some(())
            }
            // RAMDAC pixel bus control
            0x80_0220..=0x80_0222 => Some(()),
            0x80_0223 if self.pcbr1_selected() => {
                self.pcbr1 = (val & 0xF0) | Self::ANTELOPE_VERSION;
                Some(())
            }
            0x80_0223 => {
                self.pbctrl = val;
                Some(())
            }

            0x80_0000..=0x80_03FF => Some(()),
            _ => None,
        }
    }
}

impl<TRenderer> Tickable<&dyn EmuContext> for Dafb<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if self.vblank_ticks >= ctx.bus_frequency() / 60 {
            self.vblank_ticks -= ctx.bus_frequency() / 60;

            if self.renderer.as_ref().unwrap().frame_wanted() {
                self.render()?;
            }

            if self.int_enable.0 & Self::INT_VBL != 0 {
                self.int_status.0 |= Self::INT_VBL;
            }
            // TODO simply firing this once per frame seems to work, but is
            // not accurate.
            if self.int_enable.0 & Self::INT_CURSOR != 0 {
                self.int_status.0 |= Self::INT_CURSOR;
            }
        }
        Ok(ticks)
    }
}

impl<TRenderer> Debuggable for Dafb<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_byte_bin, dbgprop_enum, dbgprop_group, dbgprop_long};
        use crate::{dbgprop_str, dbgprop_string, dbgprop_udec};

        let geo = self.geometry();

        vec![
            dbgprop_group!(
                "Registers",
                vec![
                    dbgprop_long!("Screen base", self.base()),
                    dbgprop_long!("Screen stride", self.stride()),
                    dbgprop_long!("Timing control", self.timing_control.0),
                    dbgprop_long!("Configuration", self.config.0),
                    dbgprop_byte_bin!("Monitor sense drive", self.sense_drive()),
                    dbgprop_long!("Swatch mode", self.swatch_mode.0),
                    dbgprop_long!("Interrupt enable", self.int_enable.0),
                    dbgprop_long!("Interrupt status", self.int_status.0),
                    dbgprop_byte_bin!("RAMDAC pixel bus control", self.pbctrl),
                    dbgprop_udec!("Palette write index", self.pal_address),
                ]
            ),
            dbgprop_group!(
                "Video timing",
                vec![
                    dbgprop_udec!("HAL", self.hparams[Self::HAL].0),
                    dbgprop_udec!("HFP", self.hparams[Self::HFP].0),
                    dbgprop_udec!("HPIX", self.hparams[Self::HPIX].0),
                    dbgprop_udec!("HSERR", self.hparams[Self::HSERR].0),
                    dbgprop_udec!("VAL", self.vparams[Self::VAL].0),
                    dbgprop_udec!("VFP", self.vparams[Self::VFP].0),
                    dbgprop_udec!("VFPEQ", self.vparams[Self::VFPEQ].0),
                    dbgprop_udec!("VHLINE", self.vparams[Self::VHLINE].0),
                ]
            ),
            dbgprop_enum!("Monitor", self.monitor),
            dbgprop_enum!("BPP", self.bpp()),
            if let Some(geo) = geo.as_ref() {
                dbgprop_string!("Resolution", format!("{}x{}", geo.width, geo.height))
            } else {
                dbgprop_str!("Resolution", "?")
            },
            dbgprop_bool!("VBlank IRQ enable", self.int_enable.0 & Self::INT_VBL != 0),
            dbgprop_bool!("VBlank IRQ", self.int_status.0 & Self::INT_VBL != 0),
        ]
    }
}

impl<TRenderer> Display for Dafb<TRenderer>
where
    TRenderer: Renderer,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DAFB video controller")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::NullRenderer;

    const PAL_ADDRESS: Address = 0x80_0203;
    const PAL_DATA: Address = 0x80_0213;
    const PBCTRL: Address = 0x80_0223;
    const SENSE: Address = 0x80_001F;

    fn dafb(kind: DafbKind) -> Dafb<NullRenderer> {
        let renderer = NullRenderer::new(0, 0).unwrap();
        Dafb::new(renderer, MacMonitor::RGB12, kind, 512 * 1024)
    }

    #[test]
    fn palette_read_moves_to_the_next_entry() {
        for kind in [DafbKind::Dafb, DafbKind::MemcJr] {
            let mut dafb = dafb(kind);
            dafb.write(PAL_ADDRESS, 0xFF).unwrap();
            for val in 1..=6 {
                dafb.write(PAL_DATA, val).unwrap();
            }
            assert_eq!(dafb.pal_address, 1);

            dafb.write(PAL_ADDRESS, 0xFF).unwrap();
            for val in 1..=6 {
                assert_eq!(dafb.read(PAL_DATA), Some(val));
            }
            assert_eq!(dafb.pal_address, 1);
            assert_eq!(dafb.pal_idx, 0);
        }
    }

    /// On the Antelope, reads through the palette reach the address that selects
    /// its second register, as writes do
    #[test]
    fn palette_read_selects_the_antelope_register() {
        let mut dafb = dafb(DafbKind::MemcJr);
        dafb.write(PAL_ADDRESS, 1).unwrap();
        dafb.write(PBCTRL, 0x06).unwrap();
        dafb.write(PBCTRL, 0xA0).unwrap();
        dafb.write(PAL_ADDRESS, 0).unwrap();
        assert_eq!(dafb.read(PBCTRL), Some(0x06));
        for _ in 0..3 {
            dafb.read(PAL_DATA).unwrap();
        }
        assert_eq!(
            dafb.read(PBCTRL),
            Some(0xA0 | Dafb::<NullRenderer>::ANTELOPE_VERSION)
        );
    }

    /// The ROM reads the sense lines before it writes the register. With none
    /// driven, a monitor with an extended sense code reads as code 7, which the
    /// register returns inverted.
    #[test]
    fn sense_lines_undriven_after_reset() {
        let renderer = NullRenderer::new(0, 0).unwrap();
        let mut dafb = Dafb::new(renderer, MacMonitor::RGB16, DafbKind::Dafb, 512 * 1024);
        assert_eq!(dafb.read(SENSE), Some(0));
        dafb.write(SENSE, 0).unwrap();
        dafb.reset();
        assert_eq!(dafb.read(SENSE), Some(0));
    }

    /// The 12" mode at 1 bpp, as the Quadra 700 ROM programs it: 385 lines, the
    /// first from the base, the others from the screen 512 bytes after it
    #[test]
    fn twelve_inch_1bpp_top_line() {
        type D = Dafb<NullRenderer>;
        for kind in [DafbKind::Dafb, DafbKind::MemcJr] {
            let mut dafb = dafb(kind);
            dafb.base_hi = Field32(0xE00 >> 9);
            dafb.stride = Field32(1024 >> 2);
            dafb.hparams[D::HAL] = Field32(0x68);
            dafb.hparams[D::HFP] = Field32(0x268);
            dafb.hparams[D::HPIX] = Field32(0x27E);
            dafb.vparams[D::VAL] = Field32(0x28);
            dafb.vparams[D::VFP] = Field32(0x32A);
            dafb.vparams[D::VFPEQ] = Field32(0x32B);
            dafb.pbctrl = 0x80;

            let geo = dafb.geometry().unwrap();
            assert_eq!((geo.width, geo.height), (512, 385));
            assert_eq!(geo.line_start(0), 0xE00);
            let line_1 = if kind == DafbKind::Dafb {
                0x1000
            } else {
                0x1200
            };
            assert_eq!(geo.line_start(1), line_1);
            assert_eq!(geo.line_start(2), line_1 + 1024);

            // 2 bpp
            dafb.pbctrl = 0x88;
            assert_eq!(dafb.geometry().unwrap().line_start(1), 0x1200);
        }
    }
}
