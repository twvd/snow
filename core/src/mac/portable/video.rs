use crate::emulator::EmuContext;
use crate::renderer::{MONOCHROME_LUT, Renderer, render_packed};
use crate::tickable::{Tickable, Ticks};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Video<T: Renderer> {
    #[serde(skip)]
    pub(super) renderer: Option<T>,

    pub framebuffer: Vec<u8>,
    vblank_ticks: Ticks,
}

impl<T> Video<T>
where
    T: Renderer,
{
    /// Display width in pixels
    const DISPLAY_WIDTH: usize = 640;
    /// Display height in pixels
    const DISPLAY_HEIGHT: usize = 400;

    const DISPLAY_SIZE: usize = Self::DISPLAY_WIDTH * Self::DISPLAY_HEIGHT;

    const FRAMEBUFFER_SIZE: usize = ((Self::DISPLAY_WIDTH * Self::DISPLAY_HEIGHT) / 8) + 768;

    pub fn new(renderer: T) -> Self {
        Self {
            renderer: Some(renderer),
            framebuffer: vec![0xFF; Self::FRAMEBUFFER_SIZE],
            vblank_ticks: 0,
        }
    }

    fn render(&mut self) -> Result<()> {
        let fb = &self.framebuffer;

        let renderer = self.renderer.as_mut().unwrap();
        let buf = renderer.buffer_mut();
        buf.set_size(Self::DISPLAY_WIDTH, Self::DISPLAY_HEIGHT);

        render_packed(fb, &mut buf[..Self::DISPLAY_SIZE * 4], &MONOCHROME_LUT);

        renderer.update()?;

        Ok(())
    }

    pub(crate) fn blank(&mut self) -> Result<()> {
        self.framebuffer.fill(0xFF);
        self.render()
    }
}

impl<T> Tickable<&dyn EmuContext> for Video<T>
where
    T: Renderer,
{
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        let frametime: Ticks = ctx.bus_frequency() / 60;

        self.vblank_ticks += ticks;
        while self.vblank_ticks > frametime {
            if self.renderer.as_ref().unwrap().frame_wanted() {
                self.render()?;
            }

            self.vblank_ticks -= frametime;
        }
        Ok(ticks)
    }
}
