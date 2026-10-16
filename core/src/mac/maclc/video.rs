//! Built-in video of the Macintosh LC
//!
//! The V8 gate array shifts the pixels out of the video RAM, to the Ariel
//! RAMDAC which holds the colour table. The depth and the layout of the
//! video RAM are set in the registers of the V8.
//!
//! The frame is rendered at once at the start of the vertical blanking.
//! Video without video RAM (from main RAM) and the 560x384 mode of the
//! Apple IIe card are not implemented.

use super::bus::C16M_FREQUENCY;
use crate::debuggable::Debuggable;
use crate::mac::MacMonitor;
use crate::renderer::{DisplayBuffer, Renderer};
use crate::tickable::Ticks;
use crate::types::Byte;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Monitors which the V8 supports
pub const MONITORS: &[MacMonitor] = &[MacMonitor::RGB12, MacMonitor::HiRes14];

/// Sizes of the video RAM SIMM
pub const VRAM_SIZES: &[usize] = &[256 * 1024, 512 * 1024];

pub const VRAM_SIZE_DEFAULT: usize = 512 * 1024;

/// Depth of 16 bits per pixel in the monitor parameters register
const DEPTH_16BPP: u8 = 4;

const RAMDAC_ADDRESS: u32 = 0;
const RAMDAC_DATA: u32 = 1;
const RAMDAC_CONTROL: u32 = 2;
const RAMDAC_KEY: u32 = 3;

/// Ariel RAMDAC
#[derive(Serialize, Deserialize)]
struct Ramdac {
    /// Colour table (red, green, blue)
    palette: Vec<[u8; 3]>,

    /// Entry of the colour table which is read or written next
    address: u8,

    /// Component of the entry which is read or written next
    component: usize,

    /// Control register, of which the driver sets the depth in bits 0-2.
    /// The depth of the V8 is used for rendering.
    control: u8,

    /// Key colour, for the video overlay of the Apple IIe card
    key: u8,
}

impl Ramdac {
    fn new() -> Self {
        Self {
            palette: vec![[0; 3]; 256],
            address: 0,
            component: 0,
            control: 0,
            key: 0,
        }
    }

    fn next_component(&mut self) {
        self.component += 1;
        if self.component == 3 {
            self.component = 0;
            self.address = self.address.wrapping_add(1);
        }
    }

    fn read(&mut self, addr: u32) -> Byte {
        match addr & 3 {
            RAMDAC_ADDRESS => self.address,
            RAMDAC_DATA => {
                let val = self.palette[usize::from(self.address)][self.component];
                self.next_component();
                val
            }
            RAMDAC_CONTROL => self.control,
            RAMDAC_KEY => self.key,
            _ => unreachable!(),
        }
    }

    fn write(&mut self, addr: u32, val: Byte) {
        match addr & 3 {
            RAMDAC_ADDRESS => {
                self.address = val;
                self.component = 0;
            }
            RAMDAC_DATA => {
                self.palette[usize::from(self.address)][self.component] = val;
                self.next_component();
            }
            RAMDAC_CONTROL => self.control = val,
            RAMDAC_KEY => self.key = val,
            _ => unreachable!(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Video<TRenderer: Renderer> {
    #[serde(skip)]
    pub renderer: Option<TRenderer>,

    monitor: MacMonitor,
    vram: Vec<u8>,
    ramdac: Ramdac,

    /// Position in the frame, in cycles of C16M scaled by the bus frequency
    frame_clock: Ticks,
}

impl<TRenderer> Video<TRenderer>
where
    TRenderer: Renderer,
{
    pub fn new(renderer: TRenderer, monitor: Option<MacMonitor>, vram_size: Option<usize>) -> Self {
        let monitor = monitor.filter(|m| MONITORS.contains(m)).unwrap_or_default();
        let vram_size = vram_size
            .filter(|s| VRAM_SIZES.contains(s))
            .unwrap_or(VRAM_SIZE_DEFAULT);

        Self {
            renderer: Some(renderer),
            monitor,
            vram: vec![0; vram_size],
            ramdac: Ramdac::new(),
            frame_clock: 0,
        }
    }

    /// Sense lines of the monitor
    pub fn sense(&self) -> u8 {
        self.monitor.sense()[0]
    }

    /// Length of a frame in cycles of C16M
    fn frame_period(&self) -> Ticks {
        match self.monitor {
            // 640 x 407 dots at 15.6672 MHz (60.15 Hz)
            MacMonitor::RGB12 => 640 * 407,
            // 864 x 525 dots at 30.24 MHz (66.67 Hz)
            _ => 864 * 525 * 15_667_200 / 30_240_000,
        }
    }

    /// Advances by a number of bus cycles. Returns true at the start of the
    /// vertical blanking.
    pub fn tick(&mut self, ticks: Ticks, bus_frequency: Ticks) -> bool {
        let period = self.frame_period() * bus_frequency;

        self.frame_clock += ticks * C16M_FREQUENCY;
        if self.frame_clock >= period {
            self.frame_clock -= period;
            true
        } else {
            false
        }
    }

    /// The video RAM is mirrored in its 512 KB window when it is smaller
    pub fn read_vram(&self, offset: usize) -> Byte {
        self.vram[offset & (self.vram.len() - 1)]
    }

    pub fn write_vram(&mut self, offset: usize, val: Byte) {
        let mask = self.vram.len() - 1;
        self.vram[offset & mask] = val;
    }

    pub fn read_ramdac(&mut self, addr: u32) -> Byte {
        self.ramdac.read(addr)
    }

    pub fn write_ramdac(&mut self, addr: u32, val: Byte) {
        self.ramdac.write(addr, val);
    }

    /// Renders the frame to the target DisplayBuffer.
    ///
    /// `depth` is the depth in the monitor parameters register of the V8,
    /// `row_bytes` the length of a line in video RAM.
    fn render_to(&self, buf: &mut DisplayBuffer, depth: u8, row_bytes: usize) {
        let (width, height) = (self.monitor.width(), self.monitor.height());
        let palette = &self.ramdac.palette;
        let mask = self.vram.len() - 1;

        buf.set_size(width, height);
        for (y, line) in buf.chunks_exact_mut(width * 4).enumerate() {
            let row = y * row_bytes;
            for (x, pixel) in line.chunks_exact_mut(4).enumerate() {
                let rgb = if depth < DEPTH_16BPP {
                    // The pixel is the upper bits of the address in the colour
                    // table, the lower bits are set.
                    let bpp = 1 << depth;
                    let byte = self.vram[(row + x * bpp / 8) & mask];
                    let index = usize::from(byte << ((x * bpp) % 8)) | (0xFF >> bpp);
                    palette[index]
                } else {
                    // Each component goes through the first 32 entries of the
                    // colour table. Depths above 16 bpp are not known to exist.
                    let word = u16::from_be_bytes([
                        self.vram[(row + x * 2) & mask],
                        self.vram[(row + x * 2 + 1) & mask],
                    ]);
                    [
                        palette[usize::from(word >> 10) & 0x1F][0],
                        palette[usize::from(word >> 5) & 0x1F][1],
                        palette[usize::from(word) & 0x1F][2],
                    ]
                };
                pixel[..3].copy_from_slice(&rgb);
                pixel[3] = 0xFF;
            }
        }
    }

    pub fn render(&mut self, depth: u8, row_bytes: usize) -> Result<()> {
        // We have to move the renderer so we don't upset the borrow checker.
        let mut renderer = self.renderer.take().unwrap();
        self.render_to(renderer.buffer_mut(), depth, row_bytes);
        self.renderer = Some(renderer);
        self.renderer.as_mut().unwrap().update()?;
        Ok(())
    }

    /// Shows a black screen
    pub fn blank(&mut self) -> Result<()> {
        let renderer = self.renderer.as_mut().unwrap();
        let buf = renderer.buffer_mut();
        buf.set_size(self.monitor.width(), self.monitor.height());
        for pixel in buf.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[0, 0, 0, 0xFF]);
        }
        renderer.update()?;
        Ok(())
    }
}

impl<TRenderer> Debuggable for Video<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_byte, dbgprop_string, dbgprop_udec};

        vec![
            dbgprop_string!("Monitor", self.monitor.to_string()),
            dbgprop_udec!("Video RAM (KB)", self.vram.len() / 1024),
            dbgprop_byte!("RAMDAC address", self.ramdac.address),
            dbgprop_udec!("RAMDAC component", self.ramdac.component),
            dbgprop_byte!("RAMDAC control", self.ramdac.control),
            dbgprop_byte!("RAMDAC key colour", self.ramdac.key),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::NullRenderer;

    fn video(monitor: MacMonitor, vram_size: usize) -> Video<NullRenderer> {
        Video::new(
            NullRenderer::new(0, 0).unwrap(),
            Some(monitor),
            Some(vram_size),
        )
    }

    fn set_colour(video: &mut Video<NullRenderer>, address: u8, rgb: [u8; 3]) {
        video.write_ramdac(RAMDAC_ADDRESS, address);
        for c in rgb {
            video.write_ramdac(RAMDAC_DATA, c);
        }
    }

    fn pixel(buf: &DisplayBuffer, x: usize, y: usize) -> [u8; 3] {
        let idx = (y * usize::from(buf.width()) + x) * 4;
        buf[idx..idx + 3].try_into().unwrap()
    }

    #[test]
    fn ramdac_auto_increment() {
        let mut video = video(MacMonitor::HiRes14, 256 * 1024);

        video.write_ramdac(RAMDAC_ADDRESS, 0xFF);
        for c in 1..=6 {
            video.write_ramdac(RAMDAC_DATA, c);
        }
        assert_eq!(video.read_ramdac(RAMDAC_ADDRESS), 0x01);

        video.write_ramdac(RAMDAC_ADDRESS, 0xFF);
        let read: Vec<_> = (0..6).map(|_| video.read_ramdac(RAMDAC_DATA)).collect();
        assert_eq!(read, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn vram_mirror() {
        let mut video = video(MacMonitor::HiRes14, 256 * 1024);
        video.write_vram(0, 0x12);
        assert_eq!(video.read_vram(256 * 1024), 0x12);

        let mut video = self::video(MacMonitor::HiRes14, 512 * 1024);
        video.write_vram(0, 0x12);
        assert_eq!(video.read_vram(256 * 1024), 0x00);
    }

    #[test]
    fn render_indexed() {
        let mut video = video(MacMonitor::HiRes14, 256 * 1024);
        let mut buf = DisplayBuffer::new(0usize, 0usize);
        set_colour(&mut video, 0x7F, [1, 2, 3]);
        set_colour(&mut video, 0xFF, [4, 5, 6]);
        set_colour(&mut video, 0x2F, [7, 8, 9]);

        // 1 bpp: the second line starts 512 bytes after the first
        video.write_vram(0, 0b0100_0000);
        video.write_vram(512, 0b1000_0000);
        video.render_to(&mut buf, 0, 512);
        assert_eq!((buf.width(), buf.height()), (640, 480));
        assert_eq!(pixel(&buf, 0, 0), [1, 2, 3]);
        assert_eq!(pixel(&buf, 1, 0), [4, 5, 6]);
        assert_eq!(pixel(&buf, 0, 1), [4, 5, 6]);
        assert_eq!(pixel(&buf, 1, 1), [1, 2, 3]);

        // 4 bpp
        video.write_vram(0, 0x2F);
        video.render_to(&mut buf, 2, 512);
        assert_eq!(pixel(&buf, 0, 0), [7, 8, 9]);
        assert_eq!(pixel(&buf, 1, 0), [4, 5, 6]);

        // 8 bpp
        video.render_to(&mut buf, 3, 512);
        assert_eq!(pixel(&buf, 0, 0), [7, 8, 9]);
    }

    #[test]
    fn render_direct() {
        let mut video = video(MacMonitor::RGB12, 512 * 1024);
        let mut buf = DisplayBuffer::new(0usize, 0usize);
        set_colour(&mut video, 0x1F, [0xF1, 0xF2, 0xF3]);
        set_colour(&mut video, 0x01, [0x11, 0x12, 0x13]);

        // Red 31, green 1, blue 0 in the second pixel of the second line
        video.write_vram(1024 + 2, 0x7C);
        video.write_vram(1024 + 3, 0x20);
        video.render_to(&mut buf, 4, 1024);
        assert_eq!((buf.width(), buf.height()), (512, 384));
        assert_eq!(pixel(&buf, 1, 1), [0xF1, 0x12, 0x00]);
    }

    #[test]
    fn frame_rate() {
        let frames = |monitor| {
            let mut video = video(monitor, 256 * 1024);
            (0..16_000_000 / 100)
                .filter(|_| video.tick(100, 16_000_000))
                .count()
        };
        assert_eq!(frames(MacMonitor::RGB12), 60);
        assert_eq!(frames(MacMonitor::HiRes14), 66);
    }
}
