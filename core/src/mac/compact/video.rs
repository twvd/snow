//! CRT video handling module
//!
//! Code here deals with the timings of the CRTs and video circuitry of the original
//! compact Macs (128K, 512K, SE, Classic).
//!
//! ## Timing
//! Video runs off a 15.667 MHz pixel clock, which is a multiple of 2 from the CPU
//! clock.
//! Many clocks and timings are branched off (with divisors) from the pixel clock
//! or synced to the blanking periods. For example:
//!  - A sound sample is loaded on HBlank.
//!  - Disk drive spindle motor speed PWM is loaded on HBlank.
//!
//! The effective screen refresh rate is 60.14 Hz.
//!
//! ## Shared RAM access for frame buffer
//! The video framebuffer lives in main system RAM. This means access to the RAM is
//! shared between the audio/video circuitry and the main CPU. While the video
//! circuitry is accessing the RAM, the memory controller de-asserts DTACK if the
//! CPU accesses RAM, making the CPU insert wait states.
//!
//! ## Frame buffer layout
//! The framebuffer has a simple 1-bit-per-pixel layout. The MSbit is the left-most
//! pixel. A zero indicates a white pixel, a one indicates a black pixel.
//!
//! ## Frame Layout
//!  - VBlank: 28 lines (0 to 27)
//!  - Visible Area: 342 lines (28 to 369)
//!  - Visible dots: 512 dots per scanline (only in the visible area)
//!  - HBlank: 192 dots per scanline (at the end of each scanline)
//!
//! +------------------------------- CRT Frame -------------------------------+
//! |                              VBlank (28 lines)                          |
//! |   +------------------------------- Scanline 0 ------------------------+ |
//! |   |        No Visible Dots (VBlank)        | HBlank (192 dots)        | |
//! |   +------------------------------- Scanline 1 ------------------------+ |
//! |   |        No Visible Dots (VBlank)        | HBlank (192 dots)        | |
//! |   +------------------------------- Scanline 2 ------------------------+ |
//! |   |        No Visible Dots (VBlank)        | HBlank (192 dots)        | |
//! |   ... (28 total lines of vertical blanking)                             |
//! |   +------------------------------- Scanline 27 -----------------------+ |
//! |   |        No Visible Dots (VBlank)        | HBlank (192 dots)        | |
//! |                             Visible Area (342 lines)                    |
//! |   +------------------------------- Scanline 28 -----------------------+ |
//! |   |      Visible (512 dots)             | HBlank (192 dots)           | |
//! |   +------------------------------- Scanline 29 -----------------------+ |
//! |   |      Visible (512 dots)             | HBlank (192 dots)           | |
//! |   +------------------------------- Scanline 30 -----------------------+ |
//! |   |      Visible (512 dots)             | HBlank (192 dots)           | |
//! |                                                                         |
//! |   ... (repeat for 342 visible lines)                                    |
//! |                                                                         |
//! |   +------------------------------- Scanline 369 ----------------------+ |
//! |   |      Visible (512 dots)             | HBlank (192 dots)           | |
//! +-------------------------------------------------------------------------+

use crate::bus::Address;
use crate::debuggable::Debuggable;
use crate::renderer::Renderer;
use crate::tickable::{Tickable, Ticks};
use crate::types::LatchingEvent;

use anyhow::Result;

/// CRT/video circuitry state
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

    /// Offset in dots of where the visible area begins.
    const FRAME_VISIBLE_OFFSET: usize = Self::VBLANK_LINES * Self::H_DOTS;

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
        self.dots < Self::FRAME_VISIBLE_OFFSET
    }

    /// Tests if currently in HBlank period (also during VBlank)
    pub fn in_hblank(&self) -> bool {
        self.dots % Self::H_DOTS >= Self::H_VISIBLE_DOTS
    }

    /// Gets the current active scanline
    pub fn get_scanline(&self) -> usize {
        self.dots / Self::H_DOTS
    }

    /// Gets the current scanline, offset from the top of the visible frame.
    pub fn get_visible_scanline(&self) -> Option<usize> {
        if self.in_vblank() {
            None
        } else {
            Some(self.get_scanline() - Self::VBLANK_LINES)
        }
    }

    pub fn new(renderer: T) -> Self {
        Self {
            renderer,
            dots: 0,
            event_vblank: LatchingEvent::default(),
            event_hblank: LatchingEvent::default(),
            framebuffers: [
                vec![0xFF; Self::FRAMEBUFFER_SIZE],
                vec![0xFF; Self::FRAMEBUFFER_SIZE],
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

        let buf = self.renderer.buffer_mut();
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
        self.renderer.update()?;

        Ok(())
    }

    /// Blanks the display and sends an update to the renderer.
    pub fn blank(&mut self) -> Result<()> {
        self.framebuffers.iter_mut().for_each(|b| b.fill(0xFF));
        self.render()
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

impl<T> Debuggable for Video<T>
where
    T: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_header, dbgprop_str, dbgprop_udec};

        vec![
            dbgprop_str!(
                "Active framebuffer",
                if self.framebuffer_select {
                    "Primary"
                } else {
                    "Alternate"
                }
            ),
            dbgprop_bool!("In VBlank", self.in_vblank()),
            dbgprop_bool!("In HBlank", self.in_hblank()),
            dbgprop_header!("Beam position"),
            dbgprop_udec!("Horizontal", self.dots % Self::H_DOTS),
            dbgprop_udec!("Vertical", self.get_scanline()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use crate::{mac::MacModel, renderer::NullRenderer};

    use super::*;

    fn video() -> Video<NullRenderer> {
        Video::new(NullRenderer::new(0, 0).unwrap())
    }

    #[test]
    fn vblank_period() {
        let mut v = video();

        // Scanline 0 - 27 is VBlank
        assert_eq!(v.get_scanline(), 0);
        assert!(v.in_vblank());

        // Last dot in VBlank
        v.tick((28 * (512 + 192)) - 1).unwrap();
        assert!(v.in_vblank());
        assert_eq!(v.get_scanline(), 27);

        // Exit VBlank
        v.tick(1).unwrap();
        assert_eq!(v.get_scanline(), 28);
        assert!(!v.in_vblank());
        assert!(!v.get_clr_vblank());

        // Last dot in visible area before next VBlank
        v.tick((342 * (512 + 192)) - 1).unwrap();
        assert_eq!(v.get_scanline(), 369);
        assert!(!v.in_vblank());
        assert!(!v.get_clr_vblank());

        // Enter VBlank
        v.tick(1).unwrap();
        assert_eq!(v.get_scanline(), 0);
        assert!(v.in_vblank());
        assert!(v.get_clr_vblank());
        assert_eq!(v.dots, 0);
    }

    #[test]
    fn hblank_period() {
        let mut v = video();

        for _ in 0..370 {
            assert!(!v.in_hblank());
            v.tick(512).unwrap();
            assert!(v.in_hblank());
            v.tick(192).unwrap();
        }
    }
}
