use crate::devices::{
    BusKind, DeviceModel, DeviceOps, DisplayConfig, DisplayFunction, DisplayInfo, DisplayStatus,
    FunctionConfig, FunctionInfo, FunctionMut, ModelInfo,
};
use crate::emulator::EmuContext;
use crate::mac::MacModel;
use crate::renderer::channel::ChannelRenderer;
use crate::renderer::{FrameReceiver, MONOCHROME_LUT, render_packed};
use crate::tickable::{Tickable, Ticks};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Video {
    #[serde(skip)]
    renderer: ChannelRenderer,

    pub framebuffer: Vec<u8>,
    vblank_ticks: Ticks,
}

impl Video {
    /// Display width in pixels
    const DISPLAY_WIDTH: usize = 640;
    /// Display height in pixels
    const DISPLAY_HEIGHT: usize = 400;

    const DISPLAY_SIZE: usize = Self::DISPLAY_WIDTH * Self::DISPLAY_HEIGHT;

    const FRAMEBUFFER_SIZE: usize = ((Self::DISPLAY_WIDTH * Self::DISPLAY_HEIGHT) / 8) + 768;

    fn render(&mut self) -> Result<()> {
        let fb = &self.framebuffer;

        let renderer = &mut self.renderer;
        let buf = renderer.buffer_mut();
        buf.set_size(Self::DISPLAY_WIDTH, Self::DISPLAY_HEIGHT);

        render_packed(fb, &mut buf[..Self::DISPLAY_SIZE * 4], &MONOCHROME_LUT);

        renderer.update()?;

        Ok(())
    }
}

impl Default for Video {
    fn default() -> Self {
        Self {
            renderer: ChannelRenderer::default(),
            framebuffer: vec![0xFF; Self::FRAMEBUFFER_SIZE],
            vblank_ticks: 0,
        }
    }
}

fn fits(mac: MacModel) -> bool {
    mac.has_built_in(DeviceModel::PortableVideo)
}

pub const INFO: ModelInfo = ModelInfo {
    fits,
    fixed: true,
    ..ModelInfo::base(
        DeviceModel::PortableVideo,
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

impl DeviceOps for Video {
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

impl DisplayFunction for Video {
    fn status(&self) -> DisplayStatus {
        DisplayStatus { connected: true }
    }

    fn blank(&mut self) -> Result<()> {
        self.framebuffer.fill(0xFF);
        self.render()
    }

    fn frames(&self) -> FrameReceiver {
        self.renderer.get_receiver()
    }
}

impl Tickable<&dyn EmuContext> for Video {
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        let frametime: Ticks = ctx.bus_frequency() / 60;

        self.vblank_ticks += ticks;
        while self.vblank_ticks > frametime {
            if self.renderer.frame_wanted() {
                self.render()?;
            }

            self.vblank_ticks -= frametime;
        }
        Ok(ticks)
    }
}
