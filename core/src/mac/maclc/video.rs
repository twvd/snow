//! Built-in video of the Macintosh LC
//!
//! The gate array shifts the pixels out of the video RAM, to the Ariel
//! RAMDAC which holds the colour table. The mode, which is the size of the
//! picture, the depth and the layout of the video RAM, comes from the
//! registers of the gate array.
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

pub use super::v8::{MONITORS, VRAM_SIZE_DEFAULT, VRAM_SIZES};

/// Depth of 16 bits per pixel
const DEPTH_16BPP: u8 = 4;

/// Video mode set in the gate array
pub struct Mode {
    pub width: usize,
    pub height: usize,

    /// Depth as in the registers of the gate array: the bits per pixel are
    /// 2 to the power of this
    pub depth: u8,

    /// Length of a line in video RAM
    pub row_bytes: usize,
}

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
    /// The depth of the gate array is used for rendering.
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
    pub fn new(renderer: TRenderer, monitor: MacMonitor, vram_size: usize) -> Self {
        Self {
            renderer: Some(renderer),
            monitor,
            vram: vec![0; vram_size],
            ramdac: Ramdac::new(),
            frame_clock: 0,
        }
    }

    pub fn monitor(&self) -> MacMonitor {
        self.monitor
    }

    /// Sense lines of the monitor
    pub fn sense(&self) -> u8 {
        self.monitor.sense()[0]
    }

    pub fn vram_size(&self) -> usize {
        self.vram.len()
    }

    /// Advances by a number of bus cycles. Returns true at the start of the
    /// vertical blanking. `frame_period` is the length of a frame in cycles
    /// of C16M.
    pub fn tick(&mut self, ticks: Ticks, bus_frequency: Ticks, frame_period: Ticks) -> bool {
        let period = frame_period * bus_frequency;

        self.frame_clock += ticks * C16M_FREQUENCY;
        if self.frame_clock >= period {
            self.frame_clock -= period;
            true
        } else {
            false
        }
    }

    /// Reads the video RAM at an index, which the gate array works out
    pub fn read_vram(&self, idx: usize) -> Byte {
        self.vram[idx]
    }

    pub fn write_vram(&mut self, idx: usize, val: Byte) {
        self.vram[idx] = val;
    }

    pub fn read_ramdac(&mut self, addr: u32) -> Byte {
        self.ramdac.read(addr)
    }

    pub fn write_ramdac(&mut self, addr: u32, val: Byte) {
        self.ramdac.write(addr, val);
    }

    /// Renders the frame to the target DisplayBuffer
    fn render_to(&self, buf: &mut DisplayBuffer, mode: &Mode) {
        let &Mode {
            width,
            height,
            depth,
            row_bytes,
        } = mode;
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

    pub fn render(&mut self, mode: &Mode) -> Result<()> {
        // We have to move the renderer so we don't upset the borrow checker.
        let mut renderer = self.renderer.take().unwrap();
        self.render_to(renderer.buffer_mut(), mode);
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
        Video::new(NullRenderer::new(0, 0).unwrap(), monitor, vram_size)
    }

    fn mode(video: &Video<NullRenderer>, depth: u8, row_bytes: usize) -> Mode {
        Mode {
            width: video.monitor().width(),
            height: video.monitor().height(),
            depth,
            row_bytes,
        }
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
    fn render_indexed() {
        let mut video = video(MacMonitor::HiRes14, 256 * 1024);
        let mut buf = DisplayBuffer::new(0usize, 0usize);
        set_colour(&mut video, 0x7F, [1, 2, 3]);
        set_colour(&mut video, 0xFF, [4, 5, 6]);
        set_colour(&mut video, 0x2F, [7, 8, 9]);

        // 1 bpp: the second line starts 512 bytes after the first
        video.write_vram(0, 0b0100_0000);
        video.write_vram(512, 0b1000_0000);
        video.render_to(&mut buf, &mode(&video, 0, 512));
        assert_eq!((buf.width(), buf.height()), (640, 480));
        assert_eq!(pixel(&buf, 0, 0), [1, 2, 3]);
        assert_eq!(pixel(&buf, 1, 0), [4, 5, 6]);
        assert_eq!(pixel(&buf, 0, 1), [4, 5, 6]);
        assert_eq!(pixel(&buf, 1, 1), [1, 2, 3]);

        // 4 bpp
        video.write_vram(0, 0x2F);
        video.render_to(&mut buf, &mode(&video, 2, 512));
        assert_eq!(pixel(&buf, 0, 0), [7, 8, 9]);
        assert_eq!(pixel(&buf, 1, 0), [4, 5, 6]);

        // 8 bpp
        video.render_to(&mut buf, &mode(&video, 3, 512));
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
        video.render_to(&mut buf, &mode(&video, 4, 1024));
        assert_eq!((buf.width(), buf.height()), (512, 384));
        assert_eq!(pixel(&buf, 1, 1), [0xF1, 0x12, 0x00]);
    }

    #[test]
    fn frame_rate() {
        // Frames in a second, at two frequencies of the bus
        let frames = |bus_frequency, frame_period| {
            let mut video = video(MacMonitor::RGB12, 256 * 1024);
            (0..bus_frequency / 100)
                .filter(|_| video.tick(100, bus_frequency, frame_period))
                .count()
        };
        assert_eq!(frames(16_000_000, 640 * 407), 60);
        assert_eq!(frames(25_000_000, 640 * 407), 60);
        assert_eq!(frames(16_000_000, 864 * 525 * 15_667_200 / 30_240_000), 66);
    }
}
