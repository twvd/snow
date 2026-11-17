use std::fmt::Display;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::Address;
use crate::debuggable::Debuggable;
use crate::devices::{
    BusKind, DeviceModel, DeviceOps, DisplayConfig, DisplayFunction, DisplayInfo, DisplayStatus,
    FunctionConfig, FunctionInfo, FunctionMut, ModelInfo, RomInfo,
};
use crate::emulator::EmuContext;
use crate::mac::MacModel;
use crate::renderer::channel::ChannelRenderer;
use crate::renderer::{DisplayBuffer, FrameReceiver, MONOCHROME_LUT, render_packed};
use crate::tickable::{Tickable, Ticks};
use crate::types::LatchingEvent;

/// SE/30 video controller
#[derive(Serialize, Deserialize)]
pub struct SE30Video {
    #[serde(skip)]
    renderer: ChannelRenderer,

    rom: Vec<u8>,
    vblank_irq: bool,
    pub vblank_enable: bool,
    pub fb_select: bool,
    vblank_ticks: Ticks,
    pub vram: Vec<u8>,
    pub render: LatchingEvent,
}

impl SE30Video {
    /// Visible dots in one scanline
    const H_VISIBLE_DOTS: usize = 512;

    /// Visible lines in one frame
    const V_VISIBLE_LINES: usize = 342;

    /// Total visible dots in one frame.
    const FRAME_VISIBLE_DOTS: usize = Self::H_VISIBLE_DOTS * Self::V_VISIBLE_LINES;

    /// Amount of VRAM present
    const VRAM_SIZE: usize = 0x10000;

    pub fn new(rom: &[u8]) -> Self {
        Self {
            renderer: ChannelRenderer::default(),
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

        render_packed(
            fb,
            &mut buf[..Self::FRAME_VISIBLE_DOTS * 4],
            &MONOCHROME_LUT,
        );
    }
}

impl SE30Video {
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

/// The video answers in the space of NuBus slot $E, $FExx_xxxx, though it isn't on a NuBus:
/// the CPU reaches it directly. `offset` is in that space. VRAM, at $00_0000 and again at
/// $E0_0000, and the ROM at the top are memory: they answer any access within a word at once.
impl SE30Video {
    /// A read of `size` bytes at `offset`, right-justified
    pub fn read(&mut self, offset: Address, size: u32) -> u32 {
        if let Some(bytes) = self.vram_bytes(offset, size) {
            return bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b));
        }
        if offset >= 0xFE_0000 {
            return (offset..offset + size).fold(0, |v, a| {
                (v << 8) | u32::from(self.rom[(a - 0xFE_0000) as usize % self.rom.len()])
            });
        }
        log::warn!("{}: read from unimplemented offset {:06X}", self, offset);
        0
    }

    /// A write of the `size` low bytes of `val` at `offset`
    pub fn write(&mut self, offset: Address, size: u32, val: u32) {
        match self.vram_bytes(offset, size) {
            Some(bytes) => bytes.copy_from_slice(&val.to_be_bytes()[4 - size as usize..]),
            None => log::warn!("{}: write to unimplemented offset {:06X}", self, offset),
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
}

impl SE30Video {
    /// Prepares the image and sends it to the frontend
    pub fn render(&mut self) -> Result<()> {
        // We have to move the renderer so we don't upset the borrow checker.
        let mut renderer = std::mem::take(&mut self.renderer);
        self.render_to(renderer.buffer_mut());
        self.renderer = renderer;
        self.renderer.update()?;
        Ok(())
    }
}

fn fits(mac: MacModel) -> bool {
    mac.has_built_in(DeviceModel::SE30Video)
}

pub const INFO: ModelInfo = ModelInfo {
    fits,
    fixed: true,
    rom: Some(RomInfo {
        label: "SE/30 video ROM",
        digests: &["8af892fd7fff89c2151bb3027f3dc61e531f24e4adb0e3face95c90daece4409"],
        optional: false,
    }),
    ..ModelInfo::base(
        DeviceModel::SE30Video,
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

impl DeviceOps for SE30Video {
    fn info(&self) -> &'static ModelInfo {
        &INFO
    }

    fn config(&self) -> FunctionConfig {
        FunctionConfig::Display(DisplayConfig::default())
    }

    fn function(&mut self) -> FunctionMut<'_> {
        FunctionMut::Display(self)
    }
}

impl DisplayFunction for SE30Video {
    fn status(&self) -> DisplayStatus {
        DisplayStatus { connected: true }
    }

    fn blank(&mut self) -> Result<()> {
        self.vram.fill(0xFF);
        self.render()?;
        Ok(())
    }

    fn frames(&self) -> FrameReceiver {
        self.renderer.get_receiver()
    }
}

impl Tickable<&dyn EmuContext> for SE30Video {
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if !self.vblank_enable {
            self.vblank_irq = false;
        }
        if self.vblank_ticks >= ctx.bus_frequency() / 60 {
            if self.renderer.frame_wanted() {
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

impl Debuggable for SE30Video {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_bool;
        use crate::debuggable::*;

        vec![
            dbgprop_bool!("VBlank enable", self.vblank_enable),
            dbgprop_bool!("VBlank IRQ", self.vblank_irq),
            dbgprop_bool!("Framebuffer select", self.fb_select),
        ]
    }
}

impl Display for SE30Video {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SE/30 video controller")
    }
}
