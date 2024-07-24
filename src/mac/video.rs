use crate::tickable::{Tickable, Ticks};

use anyhow::Result;

/// Video logic
pub struct Video {
    /// Absolute beam position
    dots: Ticks,

    /// Latch for entered VBlank
    entered_vblank: bool,
}

impl Video {
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

    /// Tests if currently in VBlank period.
    pub fn in_vblank(&self) -> bool {
        self.dots >= Self::V_VISIBLE_LINES * Self::H_DOTS
    }

    /// Tests if currently in HBlank period.
    pub fn in_hblank(&self) -> bool {
        self.dots % Self::H_DOTS >= Self::H_VISIBLE_DOTS
    }

    pub fn new() -> Self {
        Self {
            dots: 0,
            entered_vblank: false,
        }
    }

    /// Reads and clears 'entered vblank' latch
    pub fn get_clr_vblank(&mut self) -> bool {
        let v = self.entered_vblank;
        self.entered_vblank = false;
        v
    }
}

impl Tickable for Video {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        let before_vblank = self.in_vblank();

        // Update beam position
        self.dots = (self.dots + ticks) % Self::FRAME_DOTS;

        if !before_vblank && self.in_vblank() {
            // Just entered VBlank
            self.entered_vblank = true;
        }

        Ok(ticks)
    }
}
