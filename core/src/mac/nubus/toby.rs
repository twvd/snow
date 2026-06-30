use std::fmt::Display;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
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

    depth: u8,
    line_offset: usize,

    clut: Vec<u8>,
    clut_idx: usize,

    vblank_irq: bool,
    vblank_enable: bool,
    vblank_ticks: Ticks,

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
    /// Physical dots per scanline in VRAM (only the first H_VISIBLE are shown)
    const H_TOTAL: usize = 1024;
    /// Amount of VRAM present
    const VRAM_SIZE: usize = 0x8_0000;

    /// Ticks per frame at ~60 Hz
    const FRAME_TICKS: Ticks = 16_000_000 / 60;

    pub fn new(rom: &[u8], renderer: TRenderer) -> Self {
        let mut clut = vec![0u8; 256 * 3];
        clut[(128 * 3)..].fill(0xFF);

        Self {
            renderer: Some(renderer),
            rom: rom.to_owned(),
            vram: vec![0; Self::VRAM_SIZE],
            depth: 1,
            line_offset: 0,
            clut,
            clut_idx: 0,
            vblank_irq: false,
            vblank_enable: false,
            vblank_ticks: 0,
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
        self.depth
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

        for y in 0..Self::V_VISIBLE {
            for x in 0..Self::H_VISIBLE {
                // Position within the (padded) physical scanline
                let i = y * Self::H_TOTAL + x;
                let code = match self.depth {
                    1 => {
                        let byte = self.vram[self.line_offset + i / 8];
                        let bit = byte & (0x80 >> (i % 8)) != 0;
                        if bit { 0xFF } else { 0x7F }
                    }
                    2 => {
                        let byte = u16::from(self.vram[self.line_offset + i / 4]);
                        let shift = (i % 4) * 2;
                        ((byte << shift) & 0xC0) as u8 | 0x3F
                    }
                    4 => {
                        let byte = u16::from(self.vram[self.line_offset + i / 2]);
                        let shift = (i % 2) * 4;
                        ((byte << shift) & 0xF0) as u8 | 0x0F
                    }
                    _ => self.vram[self.line_offset + i],
                } as usize;

                let out = (y * Self::H_VISIBLE + x) * 4;
                buf[out] = self.clut[code * 3];
                buf[out + 1] = self.clut[code * 3 + 1];
                buf[out + 2] = self.clut[code * 3 + 2];
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
            // Framebuffer
            0x0_0000..=0x7_FFFF => Some(self.vram[a as usize]),

            // VBlank status: 0 during vertical blanking, 0xFF otherwise.
            // Only in the low byte of the Long
            0xD_0000..=0xD_FFFF if a & 3 == 3 => {
                let in_vblank = self.vblank_ticks >= Self::FRAME_TICKS * 480 / 525;
                Some(if in_vblank { 0x00 } else { 0xFF })
            }
            0xD_0000..=0xD_FFFF => Some(0),

            // Declaration ROM (single byte lane, at the top of the slot space)
            0xF_0000..=0xF_FFFF => Some(self.read_rom(a)),

            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        let a = addr & 0x0F_FFFF;
        match a {
            // Framebuffer
            0x0_0000..=0x7_FFFF => {
                self.vram[a as usize] = val;
                Some(())
            }

            // Color depth select
            0x8_0000 => {
                self.depth = match val {
                    0xDF => 1,
                    0xBF => 2,
                    0x7F => 4,
                    0xFF => 8,
                    _ => self.depth,
                };
                Some(())
            }

            // Framebuffer base/scroll offset
            0x8_000C => {
                self.line_offset = 4 * (usize::from(!val) & 0xFF);
                Some(())
            }

            // Other CRTC/timing registers.
            0x8_0001..=0x8_FFFF => Some(()),

            // CLUT data write. The hardware inverts the value; the write pointer
            // auto-decrements (B, G, R order within an entry).
            0x9_0018 => {
                if let Some(e) = self.clut.get_mut(self.clut_idx) {
                    *e = 255 - val;
                }
                self.clut_idx = if self.clut_idx == 0 {
                    self.clut.len() - 1
                } else {
                    self.clut_idx - 1
                };
                Some(())
            }

            // CLUT index: point at the B component of the selected entry
            0x9_001C => {
                self.clut_idx = usize::from(val) * 3 + 2;
                Some(())
            }

            // Other RAMDAC registers
            0x9_0000..=0x9_FFFF => Some(()),

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

impl<TRenderer> Tickable for Toby<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if self.vblank_ticks >= Self::FRAME_TICKS {
            self.render()?;
            self.vblank_ticks -= Self::FRAME_TICKS;
            if self.vblank_enable {
                self.vblank_irq = true;
            }
        }
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
                    dbgprop_byte!("Depth (bpp)", self.depth),
                    dbgprop_long!("Line offset", self.line_offset as u32),
                    dbgprop_long!("CLUT write index", self.clut_idx as u32),
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
