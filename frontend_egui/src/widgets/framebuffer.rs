//! Widget that receives frames from the emulator and draws them to a
//! GPU texture-backed image widget.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::Ordering;

use anyhow::{bail, Result};
use crossbeam_channel::Receiver;
use eframe::egui;
use eframe::egui::Vec2;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::renderer::DisplayBuffer;

pub struct FramebufferWidget {
    frame: Option<DisplayBuffer>,
    frame_recv: Option<Receiver<DisplayBuffer>>,
    viewport_texture: egui::TextureHandle,
    pub scale: f32,

    response: Option<egui::Response>,
}

impl FramebufferWidget {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            frame: None,
            frame_recv: None,
            viewport_texture: cc.egui_ctx.load_texture(
                "viewport",
                egui::ColorImage::new([SCREEN_WIDTH, SCREEN_HEIGHT], egui::Color32::BLACK),
                egui::TextureOptions::NEAREST,
            ),
            response: None,
            scale: 1.5,
        }
    }

    pub fn max_height(&self) -> f32 {
        SCREEN_HEIGHT as f32 * self.scale
    }

    #[inline(always)]
    fn convert_framebuffer(framebuffer: &DisplayBuffer) -> Vec<egui::Color32> {
        // TODO optimize this
        let mut out = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT);

        for c in framebuffer.chunks(4) {
            out.push(egui::Color32::from_rgb(
                c[0].load(Ordering::Relaxed),
                c[1].load(Ordering::Relaxed),
                c[2].load(Ordering::Relaxed),
            ));
        }

        out
    }

    pub fn connect_receiver(&mut self, recv: Receiver<DisplayBuffer>) {
        self.frame_recv = Some(recv);
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) -> egui::Response {
        if let Some(ref frame_recv) = self.frame_recv {
            if !frame_recv.is_empty() {
                self.frame = Some(frame_recv.recv().unwrap());
                self.viewport_texture.set(
                    egui::ColorImage {
                        size: [SCREEN_WIDTH, SCREEN_HEIGHT],
                        pixels: Self::convert_framebuffer(self.frame.as_ref().unwrap()),
                    },
                    egui::TextureOptions::NEAREST,
                );
            }
        }

        let size = self.viewport_texture.size_vec2();
        let sized_texture = egui::load::SizedTexture::new(&mut self.viewport_texture, size);
        let response = ui.add(
            egui::Image::new(sized_texture)
                .fit_to_fraction(Vec2::new(1.0, 1.0))
                .max_size(Vec2::new(
                    (SCREEN_WIDTH as f32) * self.scale,
                    (SCREEN_HEIGHT as f32) * self.scale,
                ))
                .maintain_aspect_ratio(true),
        );
        self.response = Some(response.clone());
        response
    }

    pub fn write_screenshot(&self, path: &Path) -> Result<()> {
        let Some(frame) = self.frame.as_ref() else {
            bail!("No framebuffer available");
        };
        let mut encoder = png::Encoder::new(
            File::create(path)?,
            SCREEN_WIDTH as u32,
            SCREEN_HEIGHT as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(
            &frame
                .iter()
                .map(|b| b.load(Ordering::Relaxed))
                .collect::<Vec<_>>(),
        )?;

        Ok(())
    }

    pub fn rect(&self) -> egui::Rect {
        self.response.as_ref().unwrap().rect
    }

    pub fn has_pointer(&self) -> bool {
        if let Some(resp) = self.response.as_ref() {
            resp.contains_pointer()
        } else {
            false
        }
    }
}
