use crate::renderer::Renderer;
use crate::tickable::{Tickable, Ticks};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Video<T: Renderer> {
    #[serde(skip)]
    renderer: Option<T>,

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

        for idx in 0..Self::DISPLAY_SIZE {
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

        renderer.update()?;

        Ok(())
    }

    pub(crate) fn blank(&mut self) -> Result<()> {
        self.framebuffer.fill(0xFF);
        self.render()
    }
}

impl<T> Tickable for Video<T>
where
    T: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if self.vblank_ticks > 16_000_000 / 60 {
            self.render()?;

            self.vblank_ticks = 0;
        }
        Ok(ticks)
    }
}
