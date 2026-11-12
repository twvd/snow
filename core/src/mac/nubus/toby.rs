use std::fmt::Display;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{NubusSlot, bus_long, lanes, standard_offset};
use crate::bus::Address;
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::renderer::{DisplayBuffer, Renderer, render_packed};
use crate::tickable::{Tickable, Ticks};
use crate::types::LatchingEvent;

/// Size of the Toby card declaration ROM (342-0008-a.bin)
pub const TOBY_ROM_SIZE: usize = 4096;

/// Macintosh II Video Card ("Toby" Frame Buffer, Apple 630-0153, ROM 342-0008)
///
/// A fixed-frequency 640x480 NuBus video card supporting 1/2/4/8 bpp with a
/// 256-entry CLUT.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Toby<TRenderer: Renderer> {
    #[serde(skip)]
    pub renderer: Option<TRenderer>,

    rom: Vec<u8>,
    pub vram: Vec<u8>,

    /// Color depth: 0 = 1bpp, 1 = 2bpp, 2 = 4bpp, 3 = 8bpp
    mode: u8,

    /// Byte offset of the visible framebuffer within VRAM
    fb_base: usize,

    /// CLUT: 256 entries of R, G, B
    clut: Vec<u8>,
    clut_addr: usize,
    clut_comp: usize,

    vblank_irq: bool,
    vblank_enable: bool,
    vblank_ticks: Ticks,
    in_vblank: bool,

    pub render: LatchingEvent,
}

impl<TRenderer> Toby<TRenderer>
where
    TRenderer: Renderer,
{
    /// Visible dots in one scanline
    const H_VISIBLE: usize = 640;
    /// Visible lines in one frame
    const V_VISIBLE: usize = 480;
    /// Amount of VRAM present
    const VRAM_SIZE: usize = 0x8_0000;
    /// Default framebuffer base offset
    const FB_BASE: usize = 0x20;

    pub fn new(rom: &[u8], renderer: TRenderer) -> Self {
        Self {
            renderer: Some(renderer),
            rom: rom.to_owned(),
            vram: vec![0; Self::VRAM_SIZE],
            mode: 0,
            fb_base: Self::FB_BASE,
            clut: vec![0; 256 * 3],
            clut_addr: 0,
            clut_comp: 0,
            vblank_irq: false,
            vblank_enable: false,
            vblank_ticks: 0,
            in_vblank: false,
            render: LatchingEvent::default(),
        }
    }

    pub fn bpp(&self) -> u8 {
        1 << self.mode
    }

    fn read_rom(&self, a: u32) -> u8 {
        if a & 3 != 0 {
            // Declaration ROM is on a single byte lane (addr % 4 == 0)
            return 0;
        }

        // ROM is bit-inverted and byte-reversed
        let k = ((0x0F_FFFC - a) / 4) as usize;
        self.rom.get(k).map(|b| !b).unwrap_or(0)
    }

    /// Renders the current frame to the target DisplayBuffer
    pub fn render_to(&self, buf: &mut DisplayBuffer) {
        buf.set_size(Self::H_VISIBLE, Self::V_VISIBLE);

        let fb = &self.vram[self.fb_base..];
        match self.mode {
            0 => self.render_lines(fb, buf, &self.clut_lut::<2>()),
            1 => self.render_lines(fb, buf, &self.clut_lut::<4>()),
            2 => self.render_lines(fb, buf, &self.clut_lut::<16>()),
            3 => self.render_lines(fb, buf, &self.clut_lut::<256>()),
            _ => unreachable!(),
        }
    }

    /// The RGBA pixel of each of the N pixel values. The framebuffer is packed
    /// with the leftmost pixel in the most significant bits, and the pixel
    /// value left-aligned forms the CLUT index (so for 2bpp the indices are
    /// 0x00/0x40/0x80/0xC0, for 4bpp 0x00..0xF0).
    fn clut_lut<const N: usize>(&self) -> [[u8; 4]; N] {
        let shift = 8 - N.trailing_zeros();
        std::array::from_fn(|v| {
            let c = &self.clut[(v << shift) * 3..][..3];
            [c[0], c[1], c[2], 0xFF]
        })
    }

    /// Renders the visible lines of pixels of log2(N) bits
    fn render_lines<const N: usize>(&self, fb: &[u8], buf: &mut [u8], lut: &[[u8; 4]; N]) {
        // Bytes per scanline: 128 (1bpp), 256 (2bpp), 512 (4bpp), 1024 (8bpp)
        let stride = 128usize << self.mode;
        let row_bytes = Self::H_VISIBLE * N.trailing_zeros() as usize / 8;
        for (y, line) in buf
            .chunks_exact_mut(Self::H_VISIBLE * 4)
            .take(Self::V_VISIBLE)
            .enumerate()
        {
            render_packed(&fb[y * stride..][..row_bytes], line, lut);
        }
    }
}

impl<TRenderer> NubusSlot<TRenderer> for Toby<TRenderer>
where
    TRenderer: Renderer,
{
    // The card decodes 20 address bits and mirrors the rest. The framebuffer is
    // memory and answers every size; it's stored inverted, like everything on
    // the card. A register decodes only its address and has its byte lane, as
    // MAME models the card (nubus_m2video.cpp).
    fn read_sized(&mut self, addr: Address, size: u32) -> Option<u32> {
        let offset = standard_offset(addr)?;
        let a = offset & 0x0F_FFFF;
        if let Some(bytes) = self.vram.get(a as usize..(a + size) as usize) {
            return Some(bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(!b)));
        }
        let long = match a & !3 {
            // VBlank status, on lane 3: 0 during vertical blanking
            0xD_0000..=0xD_FFFC => {
                if self.in_vblank {
                    0
                } else {
                    0xFF
                }
            }
            // Declaration ROM, on lane 0, at the top of the slot space
            r @ 0xF_0000..=0xF_FFFC => u32::from(self.read_rom(r)) << 24,
            r => {
                log::warn!("{}: read from unimplemented register {:05X}", self, r);
                0
            }
        };
        Some(lanes(long, offset, size))
    }

    fn write_sized(&mut self, addr: Address, size: u32, val: u32) -> Option<()> {
        let offset = standard_offset(addr)?;
        let a = offset & 0x0F_FFFF;
        if let Some(bytes) = self.vram.get_mut(a as usize..(a + size) as usize) {
            for (b, v) in bytes
                .iter_mut()
                .zip(&val.to_be_bytes()[4 - size as usize..])
            {
                *b = !v;
            }
            return Some(());
        }
        let data = !bus_long(val, size, offset);
        match a {
            // TFB registers, one to a long word, 8 bits on lane 3
            0x8_0000..=0x8_FFFF => match (a >> 2) & 0xF {
                // Framebuffer base/scroll offset
                0x3 => self.fb_base = 4 * (data as usize & 0xFF),
                0xF => self.mode = ((data >> 4) & 3) as u8,
                _ => {}
            },
            // Bt453 RAMDAC, one register to a long word, 8 bits on lane 0
            0x9_0000..=0x9_FFFF => {
                let byte = (data >> 24) as u8;
                match (a >> 2) & 3 {
                    1 | 3 => {
                        self.clut_addr = usize::from(byte);
                        self.clut_comp = 0;
                    }
                    2 => {
                        let idx = self.clut_addr * 3 + self.clut_comp;
                        if let Some(e) = self.clut.get_mut(idx) {
                            *e = byte;
                        }
                        self.clut_comp += 1;
                        if self.clut_comp == 3 {
                            self.clut_comp = 0;
                            self.clut_addr = (self.clut_addr + 1) & 0xFF;
                        }
                    }
                    _ => {}
                }
            }
            // VBlank interrupt control
            0xA_0000..=0xA_FFFF => {
                if a & 4 != 0 {
                    self.vblank_enable = false;
                } else {
                    self.vblank_enable = true;
                    self.vblank_irq = false;
                }
            }
            r => log::warn!(
                "{}: write to unimplemented register {:05X} {:08X}",
                self,
                r,
                val
            ),
        }
        Some(())
    }

    fn reset(&mut self) {
        self.vblank_irq = false;
        self.vblank_enable = false;
    }

    fn get_irq(&self) -> bool {
        self.vblank_irq
    }

    fn blank(&mut self) -> Result<()> {
        self.vram.fill(0xFF);
        self.render()?;
        Ok(())
    }

    fn render(&mut self) -> Result<()> {
        // We have to move the renderer so we don't upset the borrow checker.
        let mut renderer = self.renderer.take().unwrap();
        self.render_to(renderer.buffer_mut());
        self.renderer = Some(renderer);
        self.renderer.as_mut().unwrap().update()?;
        Ok(())
    }

    fn set_renderer(&mut self, renderer: TRenderer) {
        self.renderer = Some(renderer);
    }
}

impl<TRenderer> Tickable<&dyn EmuContext> for Toby<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if self.vblank_ticks >= ctx.bus_frequency() / 60 {
            if self.renderer.as_ref().unwrap().frame_wanted() {
                self.render()?;
            }
            self.vblank_ticks -= ctx.bus_frequency() / 60;
            if self.vblank_enable {
                self.vblank_irq = true;
            }
        }
        self.in_vblank = self.vblank_ticks >= ctx.bus_frequency() / 60 * 480 / 525;
        Ok(ticks)
    }
}

impl<TRenderer> Debuggable for Toby<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_byte, dbgprop_group, dbgprop_long};

        vec![
            dbgprop_group!(
                "Registers",
                vec![
                    dbgprop_byte!("Depth (bpp)", self.bpp()),
                    dbgprop_long!("CLUT write address", self.clut_addr as u32),
                ]
            ),
            dbgprop_bool!("VBlank enable", self.vblank_enable),
            dbgprop_bool!("VBlank IRQ", self.vblank_irq),
        ]
    }
}

impl<TRenderer> Display for Toby<TRenderer>
where
    TRenderer: Renderer,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Macintosh II Video Card (Toby)")
    }
}
