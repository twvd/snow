use std::sync::atomic::Ordering;

use crate::{
    bus::Address,
    renderer::Renderer,
    tickable::{Tickable, Ticks},
    types::LatchingEvent,
};

use anyhow::Result;

pub const SCREEN_HEIGHT: usize = 342;
pub const SCREEN_WIDTH: usize = 512;

/// Video logic
pub struct Video<T: Renderer> {
    renderer: T,

    /// Absolute beam position
    dots: Ticks,

    /// Latch for entered VBlank
    event_vblank: LatchingEvent,

    /// Latch for entered HBlank
    event_hblank: LatchingEvent,

    /// Primary and alternate framebuffer
    pub framebuffers: [Vec<u8>; 2],

    /// Video page to be used by video circuitry
    /// (true = main, false = alternate)
    /// (lives in VIA, copied here)
    pub framebuffer_select: bool,
}

impl<T> Video<T>
where
    T: Renderer,
{
    /// Visible dots in one scanline
    const H_VISIBLE_DOTS: usize = 512;

    /// Length of HBlank, in dots.
    const HBLANK_DOTS: usize = 192;

    /// Total scanline length, including HBlank, in dots.
    const H_DOTS: usize = Self::H_VISIBLE_DOTS + Self::HBLANK_DOTS;

    /// Visible lines in one frame
    const V_VISIBLE_LINES: usize = 342;

    /// Length of VBlank, in lines.
    const VBLANK_LINES: usize = 28;

    /// Total scanlines, including VBlank.
    const V_LINES: usize = Self::V_VISIBLE_LINES + Self::VBLANK_LINES;

    /// Total dots in one frame, including blanking periods.
    const FRAME_DOTS: usize = Self::H_DOTS * Self::V_LINES;

    /// Total visible dots in one frame.
    const FRAME_VISIBLE_DOTS: usize = Self::H_VISIBLE_DOTS * Self::V_VISIBLE_LINES;

    /// Size (in bytes) of a single framebuffer.
    pub const FRAMEBUFFER_SIZE: usize = Self::FRAME_DOTS / 8;

    /// Offset of main framebuffer (from END of RAM)
    pub const FRAMEBUFFER_MAIN_OFFSET: Address = 0xD900;

    /// Offset of alternate framebuffer (from END of RAM)
    pub const FRAMEBUFFER_ALT_OFFSET: Address = 0x5900;

    /// Tests if currently in any blanking period.
    pub fn in_blanking_period(&self) -> bool {
        self.in_hblank() || self.in_vblank()
    }

    /// Tests if currently in VBlank period.
    pub fn in_vblank(&self) -> bool {
        self.dots >= Self::V_VISIBLE_LINES * Self::H_DOTS
    }

    /// Tests if currently in HBlank period.
    pub fn in_hblank(&self) -> bool {
        self.dots % Self::H_DOTS >= Self::H_VISIBLE_DOTS
    }

    /// Gets the current active scanline
    pub fn get_scanline(&self) -> usize {
        self.dots / Self::H_DOTS
    }

    pub fn new(renderer: T) -> Self {
        Self {
            renderer,
            dots: 0,
            event_vblank: LatchingEvent::default(),
            event_hblank: LatchingEvent::default(),
            framebuffers: [
                vec![0; Self::FRAMEBUFFER_SIZE],
                vec![0; Self::FRAMEBUFFER_SIZE],
            ],
            framebuffer_select: false,
        }
    }

    /// Reads and clears 'entered vblank' latch
    pub fn get_clr_vblank(&mut self) -> bool {
        self.event_vblank.get_clear()
    }

    /// Reads and clears 'entered hblank' latch
    pub fn get_clr_hblank(&mut self) -> bool {
        self.event_hblank.get_clear()
    }

    /// Prepares the image and sends it to the frontend renderer
    fn render(&mut self) -> Result<()> {
        let fb = if !self.framebuffer_select {
            &self.framebuffers[0]
        } else {
            &self.framebuffers[1]
        };

        let buf = self.renderer.get_buffer();
        for idx in 0..Self::FRAME_VISIBLE_DOTS {
            let byte = idx / 8;
            let bit = idx % 8;
            if fb[byte] & (1 << (7 - bit)) == 0 {
                buf[idx * 4].store(0xEE, Ordering::Release);
                buf[idx * 4 + 1].store(0xEE, Ordering::Release);
                buf[idx * 4 + 2].store(0xEE, Ordering::Release);
            } else {
                buf[idx * 4].store(0x22, Ordering::Release);
                buf[idx * 4 + 1].store(0x22, Ordering::Release);
                buf[idx * 4 + 2].store(0x22, Ordering::Release);
            }
        }
        self.renderer.update()?;

        Ok(())
    }
}

impl<T> Tickable for Video<T>
where
    T: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        let before_vblank = self.in_vblank();
        let before_hblank = self.in_hblank();

        // Update beam position
        self.dots = (self.dots + ticks) % Self::FRAME_DOTS;

        if !before_vblank && self.in_vblank() {
            // Just entered VBlank
            self.event_vblank.set();
        }

        if !before_hblank && self.in_hblank() {
            // Just entered HBlank
            self.event_hblank.set();
        }

        if before_vblank && !self.in_vblank() {
            // Just left VBlank
            self.render()?;
        }

        Ok(ticks)
    }
}
