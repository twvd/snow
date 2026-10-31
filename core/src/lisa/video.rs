//! The Lisa video circuit
//!
//! A counter addresses a 256-byte PROM, whose outputs give the sync signals,
//! the blanking, and when to restart the counter and the video address. It runs
//! at a state per 16 dots, 4 CPU clocks. Bit 7 of the PROM holds the serial number
//! of the machine, one bit per state.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::devices::{
    BusKind, DeviceConfig, DeviceModel, DeviceOps, DisplayConfig, DisplayFunction, DisplayInfo,
    DisplayStatus, FunctionConfig, FunctionInfo, FunctionMut, ModelInfo, RomInfo,
};
use crate::mac::MacModel;
use crate::renderer::FrameReceiver;
use crate::renderer::channel::ChannelRenderer;
use crate::tickable::Ticks;

/// Size of the video state PROM
const PROM_SIZE: usize = 256;

/// CPU clocks per state of the video PROM
const CPU_PER_STATE: Ticks = 4;

/// Size of the 16-bit words the video scans
const DOTS_PER_WORD: usize = 16;

/// A line counts as displayed if vertical sync is off this far into it
const FIRST_WORDS_OF_LINE: usize = 6;

/// Size of the area of RAM that the video latch can select
const PAGE_SHIFT: u32 = 15;

/// PROM outputs
const PROM_END_OF_LINE: u8 = 1 << 0;
const PROM_END_OF_FRAME: u8 = 1 << 3;
const PROM_NOT_BLANK: u8 = 1 << 2;
const PROM_NOT_VSYNC: u8 = 1 << 1;
const PROM_NOT_INTERRUPT: u8 = 1 << 5;
const PROM_CSYNC: u8 = 1 << 4;
const PROM_SERIAL: u8 = 1 << 7;

/// A state of the video PROM, in a frame
#[derive(Clone, Copy)]
struct State {
    /// The word the video reads in this state, from the start of its page
    address: u16,
    /// The PROM output
    outputs: u8,
}

#[derive(Serialize, Deserialize)]
pub struct LisaVideo {
    #[serde(skip)]
    renderer: ChannelRenderer,

    prom: Vec<u8>,
    /// The file the PROM came from
    prom_path: Option<PathBuf>,
    #[serde(skip)]
    states: Vec<State>,
    /// Display size from the PROM
    #[serde(skip)]
    width: usize,
    #[serde(skip)]
    height: usize,
    /// First state with the interrupt output active
    #[serde(skip)]
    interrupt_state: usize,

    /// CPU clocks into the frame
    clock: Ticks,

    /// Video address latch: A15-A20 of the page
    pub latch: u8,
    /// Vertical retrace interrupt pending
    interrupt: bool,
    /// Vertical retrace interrupt disabled
    disabled: bool,
}

impl LisaVideo {
    /// The video of a Lisa, with the PROM file that its device config names
    pub fn with_config(config: Option<&DeviceConfig>) -> Result<Self> {
        let path = config
            .and_then(|config| config.rom.as_ref())
            .with_context(|| format!("The {} needs its PROM file", INFO.name))?;
        let prom =
            std::fs::read(path).with_context(|| format!("Cannot read '{}'", path.display()))?;
        if prom.len() != PROM_SIZE {
            bail!("'{}' isn't a PROM of {} bytes", path.display(), PROM_SIZE);
        }
        let mut video = Self::new(&prom);
        video.prom_path = Some(path.clone());
        Ok(video)
    }

    pub fn new(prom: &[u8]) -> Self {
        let mut video = Self {
            renderer: ChannelRenderer::default(),
            prom: prom.to_vec(),
            prom_path: None,
            states: vec![],
            width: 0,
            height: 0,
            interrupt_state: 0,
            clock: 0,
            latch: 0x3F,
            interrupt: false,
            disabled: false,
        };
        video.run_prom();
        video
    }

    /// What the PROM gives isn't in a save state. The frame is in the machine's memory, so
    /// it shows at the next frame.
    pub fn after_deserialize(&mut self) {
        self.run_prom();
    }

    pub fn reset(&mut self) {
        self.latch = 0x3F;
        self.interrupt = false;
        self.disabled = false;
    }

    /// Runs a frame through the PROM: the counter is 6 bits, and the PROM has
    /// A8 and A14 of the video address on its address lines A6 and A7.
    fn run_prom(&mut self) {
        let mut states = vec![];
        let (mut counter, mut address) = (0_usize, 0_u16);
        let (mut x, mut y) = (0_usize, 0_usize);
        let (mut width, mut height) = (0, 0);
        loop {
            let index = counter
                | (usize::from(address & (1 << 8) != 0) << 6)
                | (usize::from(address & (1 << 14) != 0) << 7);
            let outputs = self.prom[index];
            states.push(State { address, outputs });

            if outputs & PROM_NOT_BLANK != 0 {
                // Vertical sync starts some words into its first line, which the
                // PROM doesn't blank
                if outputs & PROM_NOT_VSYNC != 0 {
                    width = width.max(x + DOTS_PER_WORD);
                    if x == FIRST_WORDS_OF_LINE * DOTS_PER_WORD {
                        height = height.max(y + 1);
                    }
                }
                address = address.wrapping_add(1);
            }
            if outputs & PROM_END_OF_FRAME != 0 {
                break;
            }
            if outputs & PROM_END_OF_LINE != 0 {
                counter = 0;
                x = 0;
                y += 1;
            } else {
                counter = (counter + 1) & 0x3F;
                x += DOTS_PER_WORD;
            }
        }
        self.interrupt_state = states
            .iter()
            .position(|s| s.outputs & PROM_NOT_INTERRUPT == 0)
            .unwrap_or(0);
        self.states = states;
        self.width = width;
        self.height = height;
    }

    /// The state the PROM is in, `ahead` CPU clocks from the last tick
    fn state(&self, ahead: Ticks) -> State {
        let index = ((self.clock + ahead) / CPU_PER_STATE) as usize;
        self.states[index % self.states.len()]
    }

    /// The Screen Kit's PROM, by the display size it gives
    pub fn is_screen_kit(&self) -> bool {
        self.width == 608
    }

    /// Physical address of the video page
    fn page(&self) -> usize {
        usize::from(self.latch & 0x3F) << PAGE_SHIFT
    }

    /// The serial number bit, in the state the video is in. The boot ROM reads it
    /// with bus cycles one state apart, so the state has to be exact: `ahead` is
    /// the CPU clocks since the last tick.
    pub fn serial_bit(&self, ahead: Ticks) -> bool {
        self.state(ahead).outputs & PROM_SERIAL != 0
    }

    /// The bits of the status register that come from the video: vertical
    /// retrace (active low), video output, composite sync
    pub fn status(&self, ram: &[u8], ahead: Ticks) -> u16 {
        let state = self.state(ahead);
        let word = self.page() + usize::from(state.address) * 2;
        let video_bit = ram.get(word).is_some_and(|&b| b & 1 != 0);
        (if self.interrupt { 0 } else { 1 << 2 })
            | (if video_bit { 1 << 4 } else { 0 })
            | (if state.outputs & PROM_CSYNC != 0 {
                1 << 5
            } else {
                0
            })
    }

    /// VTMSK: disabling also clears a pending interrupt
    pub fn set_disabled(&mut self, val: bool) {
        self.disabled = val;
        if val {
            self.interrupt = false;
        }
    }

    pub fn irq(&self) -> bool {
        self.interrupt
    }

    /// CPU clocks per frame
    pub fn frame_period(&self) -> Ticks {
        self.states.len() as Ticks * CPU_PER_STATE
    }

    /// Advances by CPU clocks, true if a frame ended
    pub fn tick(&mut self, ticks: Ticks, ram: &[u8]) -> Result<bool> {
        let frame = self.frame_period();
        let interrupt_clock = self.interrupt_state as Ticks * CPU_PER_STATE;

        let before = self.clock;
        self.clock += ticks;
        if before < interrupt_clock && self.clock >= interrupt_clock && !self.disabled {
            self.interrupt = true;
        }
        if self.clock >= frame {
            self.clock %= frame;
            self.render(ram)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn render(&mut self, ram: &[u8]) -> Result<()> {
        let (width, height) = (self.width, self.height);
        let stride = width.div_ceil(8);
        let page = self.page();
        let renderer = &mut self.renderer;
        if !renderer.frame_wanted() {
            return Ok(());
        }
        let buf = renderer.buffer_mut();
        buf.set_size(width, height);
        for y in 0..height {
            for x in 0..width {
                let byte = ram.get(page + y * stride + x / 8).copied().unwrap_or(0);
                let level = if byte & (0x80 >> (x % 8)) != 0 {
                    0x22
                } else {
                    0xEE
                };
                let idx = (y * width + x) * 4;
                buf[idx] = level;
                buf[idx + 1] = level;
                buf[idx + 2] = level;
                buf[idx + 3] = 0xFF;
            }
        }
        renderer.update()
    }

    fn blank(&mut self) -> Result<()> {
        let renderer = &mut self.renderer;
        let (width, height) = (self.width, self.height);
        let buf = renderer.buffer_mut();
        buf.set_size(width, height);
        for idx in 0..width * height {
            buf[idx * 4] = 0xEE;
            buf[idx * 4 + 1] = 0xEE;
            buf[idx * 4 + 2] = 0xEE;
            buf[idx * 4 + 3] = 0xFF;
        }
        renderer.update()
    }
}

/// The video of the Lisa, with its PROM: the Lisa's own monitor (341-0229-A) or the Screen
/// Kit's (341-0348-A)
pub const INFO: ModelInfo = ModelInfo {
    fits: MacModel::is_lisa,
    fixed: true,
    rom: Some(RomInfo {
        label: "Lisa video state PROM",
        digests: &[
            "31347919b61cfc77e7f54cfdde517018e8d7f36e99206b796204e13fc205c42f",
            "7548b5f60661bb833dae5f0de6b946a787478c5ee68d3af88ab59c8ad7130d2d",
        ],
        optional: false,
        varying_bits: PROM_SERIAL,
    }),
    ..ModelInfo::base(
        DeviceModel::LisaVideo,
        "Built-in video",
        BusKind::BuiltIn,
        FunctionInfo::Display(DisplayInfo {
            // The screen is the machine's own
            monitors: &[],
            default_monitor: 0,
            vram_sizes: &[],
            default_vram_size: 0,
        }),
    )
};

impl DeviceOps for LisaVideo {
    fn info(&self) -> &'static ModelInfo {
        &INFO
    }

    fn rom(&self) -> Option<&Path> {
        self.prom_path.as_deref()
    }

    fn config(&self) -> FunctionConfig {
        FunctionConfig::Display(DisplayConfig::default())
    }

    fn function(&mut self) -> FunctionMut<'_> {
        FunctionMut::Display(self)
    }
}

impl DisplayFunction for LisaVideo {
    fn status(&self) -> DisplayStatus {
        DisplayStatus { connected: true }
    }

    fn frames(&self) -> FrameReceiver {
        self.renderer.get_receiver()
    }

    fn blank(&mut self) -> Result<()> {
        Self::blank(self)
    }
}

/// A PROM with a frame of one line of 8 words, for the tests that make a Lisa
#[cfg(test)]
pub(crate) fn test_prom() -> Vec<u8> {
    let mut prom = vec![PROM_NOT_BLANK | PROM_NOT_VSYNC | PROM_NOT_INTERRUPT; PROM_SIZE];
    prom[7] |= PROM_END_OF_LINE | PROM_END_OF_FRAME;
    prom
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small PROM: 4 words a line, a line of blanking, 2 lines of video
    fn prom() -> Vec<u8> {
        let visible = PROM_NOT_BLANK | PROM_NOT_VSYNC | PROM_NOT_INTERRUPT;
        let mut prom = vec![visible; 256];
        for bank in 0..4 {
            prom[bank * 64 + 3] = visible | PROM_END_OF_LINE;
        }
        // Address lines A6 and A7 aren't reached in this small frame
        prom[3] = visible | PROM_END_OF_LINE;
        prom
    }

    #[test]
    fn the_prom_of_the_tests_has_a_frame() {
        let video = LisaVideo::new(&test_prom());
        assert_eq!((video.width, video.height), (8 * DOTS_PER_WORD, 1));
    }

    #[test]
    fn serial_from_prom_position() {
        let mut p = prom();
        p[0] |= PROM_SERIAL;
        p[3] &= !PROM_END_OF_LINE;
        p[7] |= PROM_END_OF_FRAME;
        let video = LisaVideo::new(&p);
        assert!(video.serial_bit(0));
        assert_eq!(video.width, 8 * DOTS_PER_WORD);
        assert_eq!(video.height, 1);
    }

    #[test]
    fn interrupt_until_disabled() {
        let mut p = prom();
        p[1] &= !PROM_NOT_INTERRUPT;
        p[3] |= PROM_END_OF_FRAME;
        let mut video = LisaVideo::new(&p);
        let ram = vec![0; 1 << 21];
        assert!(!video.irq());
        video.tick(CPU_PER_STATE, &ram).unwrap();
        assert!(video.irq());
        assert_eq!(video.status(&ram, 0) & (1 << 2), 0);
        video.set_disabled(true);
        assert!(!video.irq());
        assert_eq!(video.status(&ram, 0) & (1 << 2), 1 << 2);
    }

    #[test]
    fn tick_reports_end_of_frame() {
        let mut p = prom();
        p[3] |= PROM_END_OF_FRAME;
        let mut video = LisaVideo::new(&p);
        let ram = vec![0; 1 << 21];
        let period = video.frame_period();
        assert!(!video.tick(period - 1, &ram).unwrap());
        assert!(video.tick(1, &ram).unwrap());
        assert!(!video.tick(period - 1, &ram).unwrap());
        assert!(video.tick(period, &ram).unwrap());
    }
}
