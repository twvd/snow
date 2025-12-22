use std::fmt::Display;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
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

    pub fn reset(&mut self) {
        self.vblank_irq = false;
        self.vblank_enable = false;
        self.fb_select = false;
    }

    pub fn get_irq(&self) -> bool {
        self.vblank_irq
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

impl<TRenderer> BusMember<Address> for SE30Video<TRenderer>
where
    TRenderer: Renderer,
{
    fn read(&mut self, addr: Address) -> Option<u8> {
        // Assume normal slot, not super slot
        match addr & 0xFF_FFFF {
            0x00_0000..=0x00_FFFF | 0xE0_0000..=0xE0_FFFF => {
                Some(self.vram[(addr & 0xFFFF) as usize])
            }
            // ROM
            0xFE_0000..=0xFF_FFFF => Some(self.rom[(addr - 0xFE_0000) as usize % self.rom.len()]),
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        // Assume normal slot, not super slot
        match addr & 0xFF_FFFF {
            0x00_0000..=0x00_FFFF | 0xE0_0000..=0xE0_FFFF => {
                self.vram[(addr as usize) & 0xFFFF] = val;
                Some(())
            }
            _ => None,
        }
    }
}

impl<TRenderer> Tickable for SE30Video<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if !self.vblank_enable {
            self.vblank_irq = false;
        }
        if self.vblank_ticks > 16_000_000 / 60 {
            self.render()?;

            self.vblank_ticks = 0;
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
