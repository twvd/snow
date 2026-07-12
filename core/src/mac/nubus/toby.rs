use std::fmt::Display;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::renderer::{DisplayBuffer, Renderer};
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

    pub fn reset(&mut self) {
        self.vblank_irq = false;
        self.vblank_enable = false;
    }

    pub fn get_irq(&self) -> bool {
        self.vblank_irq
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
        // Bytes per scanline: 128 (1bpp), 256 (2bpp), 512 (4bpp), 1024 (8bpp)
        let stride = 128usize << self.mode;

        // The framebuffer is packed with the leftmost pixel in the most significant
        // bits and the pixel value left-aligned to form the CLUT index (so for
        // 2bpp the indices are 0x00/0x40/0x80/0xC0, for 4bpp 0x00..0xF0).
        for y in 0..Self::V_VISIBLE {
            for x in 0..Self::H_VISIBLE {
                let index = match self.mode {
                    0 => {
                        // 1 bpp
                        let byte = fb[y * stride + x / 8];
                        usize::from((byte << (x % 8)) & 0x80)
                    }
                    1 => {
                        // 2 bpp
                        let byte = fb[y * stride + x / 4];
                        usize::from((byte << ((x % 4) * 2)) & 0xC0)
                    }
                    2 => {
                        // 4 bpp
                        let byte = fb[y * stride + x / 2];
                        if x % 2 == 0 {
                            usize::from(byte & 0xF0)
                        } else {
                            usize::from((byte & 0x0F) << 4)
                        }
                    }
                    3 => {
                        // 8 bpp
                        usize::from(fb[y * stride + x])
                    }
                    _ => unreachable!(),
                };

                let out = (y * Self::H_VISIBLE + x) * 4;
                buf[out] = self.clut[index * 3];
                buf[out + 1] = self.clut[index * 3 + 1];
                buf[out + 2] = self.clut[index * 3 + 2];
                buf[out + 3] = 0xFF;
            }
        }
    }

    pub fn blank(&mut self) -> Result<()> {
        self.vram.fill(0xFF);
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
}

impl<TRenderer> BusMember<Address> for Toby<TRenderer>
where
    TRenderer: Renderer,
{
    fn read(&mut self, addr: Address) -> Option<u8> {
        // The card decodes 20 address bits and mirrors the rest.
        let a = addr & 0x0F_FFFF;
        match a {
            // Framebuffer (stored inverted)
            0x0_0000..=0x7_FFFF => Some(!self.vram[a as usize]),

            // VBlank status: 0 during vertical blanking, 0xFF otherwise.
            0xD_0000..=0xD_FFFF if a & 3 == 3 => Some(if self.in_vblank { 0x00 } else { 0xFF }),
            0xD_0000..=0xD_FFFF => Some(0),

            // Declaration ROM (single byte lane, at the top of the slot space)
            0xF_0000..=0xF_FFFF => Some(self.read_rom(a)),

            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        let a = addr & 0x0F_FFFF;
        match a {
            // Framebuffer (stored inverted)
            0x0_0000..=0x7_FFFF => {
                self.vram[a as usize] = !val;
                Some(())
            }

            // Framebuffer base/scroll offset register
            0x8_000C => {
                self.fb_base = 4 * (usize::from(!val) & 0xFF);
                Some(())
            }

            // CRTC/timing registers. Written 32-bits, inverted
            0x8_0000..=0x8_FFFF => {
                if a & 3 == 3 {
                    let reg = ((a >> 2) & 0xF) as usize;
                    if reg == 0xF {
                        self.mode = (!val >> 4) & 3;
                    }
                }
                Some(())
            }

            // CLUT/RAMDAC
            0x9_0000..=0x9_FFFF => {
                if a & 3 == 0 {
                    match (a >> 2) & 3 {
                        1 | 3 => {
                            self.clut_addr = usize::from(!val);
                            self.clut_comp = 0;
                        }
                        2 => {
                            let idx = self.clut_addr * 3 + self.clut_comp;
                            if let Some(e) = self.clut.get_mut(idx) {
                                *e = !val;
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
                Some(())
            }

            // VBlank interrupt control
            0xA_0000..=0xA_FFFF => {
                if a & 4 != 0 {
                    self.vblank_enable = false;
                } else {
                    self.vblank_enable = true;
                    self.vblank_irq = false;
                }
                Some(())
            }

            _ => None,
        }
    }
}

impl<TRenderer> Tickable<&dyn EmuContext> for Toby<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if self.vblank_ticks >= ctx.bus_frequency() / 60 {
            self.render()?;
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
