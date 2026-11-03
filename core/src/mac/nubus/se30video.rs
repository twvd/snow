use std::fmt::Display;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{NubusSlot, VideoCard};
use crate::bus::Address;
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::renderer::{DisplayBuffer, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::LatchingEvent;

/// SE/30 video controller
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct SE30Video<TRenderer: Renderer> {
    #[serde(skip)]
    pub renderer: Option<TRenderer>,

    rom: Vec<u8>,
    vblank_irq: bool,
    pub vblank_enable: bool,
    pub fb_select: bool,
    vblank_ticks: Ticks,
    pub vram: Vec<u8>,
    pub render: LatchingEvent,
}

impl<TRenderer> SE30Video<TRenderer>
where
    TRenderer: Renderer,
{
    /// Visible dots in one scanline
    const H_VISIBLE_DOTS: usize = 512;

    /// Visible lines in one frame
    const V_VISIBLE_LINES: usize = 342;

    /// Total visible dots in one frame.
    const FRAME_VISIBLE_DOTS: usize = Self::H_VISIBLE_DOTS * Self::V_VISIBLE_LINES;

    /// Amount of VRAM present
    const VRAM_SIZE: usize = 0x10000;

    pub fn new(rom: &[u8], renderer: TRenderer) -> Self {
        Self {
            renderer: Some(renderer),
            rom: rom.to_owned(),
            vblank_irq: false,
            vblank_enable: false,
            fb_select: false,
            vram: vec![0; Self::VRAM_SIZE],
            vblank_ticks: 0,
            render: LatchingEvent::default(),
        }
    }

    pub fn framebuffer(&self) -> &[u8] {
        let base_offset = if !self.fb_select { 0 } else { 0x8000 };

        // Skip top scanline
        &self.vram[(base_offset + (Self::H_VISIBLE_DOTS / 8))..]
    }

    /// Renders current dislayed frame to target DisplayBuffer
    pub fn render_to(&self, buf: &mut DisplayBuffer) {
        let fb = self.framebuffer();
        buf.set_size(Self::H_VISIBLE_DOTS, Self::V_VISIBLE_LINES);

        for idx in 0..Self::FRAME_VISIBLE_DOTS {
            let byte = idx / 8;
            let bit = idx % 8;
            if fb[byte] & (1 << (7 - bit)) == 0 {
                buf[idx * 4] = 0xEE;
                buf[idx * 4 + 1] = 0xEE;
                buf[idx * 4 + 2] = 0xEE;
            } else {
                buf[idx * 4] = 0x22;
                buf[idx * 4 + 1] = 0x22;
                buf[idx * 4 + 2] = 0x22;
            }
            buf[idx * 4 + 3] = 0xFF;
        }
    }
}

impl<TRenderer> SE30Video<TRenderer>
where
    TRenderer: Renderer,
{
    /// The VRAM bytes at `offset`, if they're all VRAM
    fn vram_bytes(&mut self, offset: Address, size: u32) -> Option<&mut [u8]> {
        let range = offset & 0xFF_0000;
        if range != 0 && range != 0xE0_0000 {
            return None;
        }
        let a = (offset & 0xFFFF) as usize;
        self.vram.get_mut(a..a + size as usize)
    }
}

impl<TRenderer> NubusSlot for SE30Video<TRenderer>
where
    TRenderer: Renderer,
{
    // VRAM, at $00_0000 and again at $E0_0000, and the ROM at the top are
    // memory: they answer any access within a word at once
    fn read_sized(&mut self, offset: Address, size: u32) -> Option<u32> {
        if let Some(bytes) = self.vram_bytes(offset, size) {
            return Some(bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b)));
        }
        if offset >= 0xFE_0000 {
            return Some((offset..offset + size).fold(0, |v, a| {
                (v << 8) | u32::from(self.rom[(a - 0xFE_0000) as usize % self.rom.len()])
            }));
        }
        log::warn!("{}: read from unimplemented offset {:06X}", self, offset);
        Some(0)
    }

    fn write_sized(&mut self, offset: Address, size: u32, val: u32) -> Option<()> {
        match self.vram_bytes(offset, size) {
            Some(bytes) => bytes.copy_from_slice(&val.to_be_bytes()[4 - size as usize..]),
            None => log::warn!("{}: write to unimplemented offset {:06X}", self, offset),
        }
        Some(())
    }

    fn reset(&mut self) {
        self.vblank_irq = false;
        self.vblank_enable = false;
        self.fb_select = false;
    }

    fn get_irq(&self) -> bool {
        self.vblank_irq
    }
}

impl<TRenderer> VideoCard<TRenderer> for SE30Video<TRenderer>
where
    TRenderer: Renderer,
{
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

impl<TRenderer> Tickable<&dyn EmuContext> for SE30Video<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if !self.vblank_enable {
            self.vblank_irq = false;
        }
        if self.vblank_ticks >= ctx.bus_frequency() / 60 {
            if self.renderer.as_ref().unwrap().frame_wanted() {
                self.render()?;
            }

            self.vblank_ticks -= ctx.bus_frequency() / 60;
            if self.vblank_enable {
                self.vblank_irq = true;
            }
        }
        Ok(ticks)
    }
}

impl<TRenderer> Debuggable for SE30Video<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_bool;
        use crate::debuggable::*;

        vec![
            dbgprop_bool!("VBlank enable", self.vblank_enable),
            dbgprop_bool!("VBlank IRQ", self.vblank_irq),
            dbgprop_bool!("Framebuffer select", self.vblank_enable),
        ]
    }
}

impl<TRenderer> Display for SE30Video<TRenderer>
where
    TRenderer: Renderer,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SE/30 video controller")
    }
}
