//! Widget that receives frames from the emulator and draws them to a
//! GPU texture-backed image widget.

use std::fs::File;
use std::path::Path;

use anyhow::{bail, Result};
use crossbeam_channel::Receiver;
use eframe::egui;
use eframe::egui::Vec2;
use snow_core::renderer::DisplayBuffer;

pub struct FramebufferWidget {
    frame: Option<DisplayBuffer>,
    frame_recv: Option<Receiver<DisplayBuffer>>,
    viewport_texture: egui::TextureHandle,
    pub scale: f32,
    display_size: [u16; 2],

    response: Option<egui::Response>,
}

impl FramebufferWidget {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            frame: None,
            frame_recv: None,
            viewport_texture: cc.egui_ctx.load_texture(
                "viewport",
                egui::ColorImage::new([0, 0], egui::Color32::BLACK),
                egui::TextureOptions::NEAREST,
            ),
            response: None,
            scale: 1.5,
            display_size: [0, 0],
        }
    }

    pub fn set_display_size(&mut self, width: u16, height: u16) {
        self.display_size = [width, height];
    }

    pub fn display_size<T>(&self) -> [T; 2]
    where
        T: From<u16>,
    {
        core::array::from_fn(|i| self.display_size[i].into())
    }

    pub fn display_size_max_scaled(&self) -> egui::Vec2 {
        egui::Vec2::from(core::array::from_fn(|i| {
            f32::from(self.display_size[i]) * self.scale
        }))
    }

    pub fn scaling_factors_actual(&self) -> egui::Vec2 {
        egui::Vec2::from(self.display_size()) / self.rect().size()
    }

    pub fn max_height(&self) -> f32 {
        f32::from(self.display_size[1]) * self.scale
    }

    pub fn connect_receiver(&mut self, recv: Receiver<DisplayBuffer>, w: u16, h: u16) {
        self.set_display_size(w, h);
        self.frame_recv = Some(recv);
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) -> egui::Response {
        if let Some(ref frame_recv) = self.frame_recv {
            if !frame_recv.is_empty() {
                let frame = frame_recv.recv().unwrap();
                assert_eq!(self.display_size[0], frame.width());
                assert_eq!(self.display_size[1], frame.height());

                self.viewport_texture.set(
                    egui::ColorImage {
                        size: self.display_size.map(|i| i.into()),
                        pixels: Vec::from_iter(
                            frame
                                .chunks_exact(4)
                                .map(|c| egui::Color32::from_rgb(c[0], c[1], c[2])),
                        ),
                    },
                    egui::TextureOptions::NEAREST,
                );
                self.frame = Some(frame);
            }
        }

        let size = self.viewport_texture.size_vec2();
        let sized_texture = egui::load::SizedTexture::new(&mut self.viewport_texture, size);
        let response = ui.add(
            egui::Image::new(sized_texture)
                .fit_to_fraction(Vec2::new(1.0, 1.0))
                .max_size(self.display_size_max_scaled())
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
            self.display_size[0].into(),
            self.display_size[1].into(),
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(frame)?;

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
