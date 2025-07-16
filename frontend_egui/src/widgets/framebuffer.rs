//! Widget that receives frames from the emulator and draws them to a
//! GPU texture-backed image widget.

use std::fs::File;
use std::path::Path;

use anyhow::{bail, Result};
use crossbeam_channel::Receiver;
use eframe::egui;
use eframe::egui::Vec2;
use serde::{Deserialize, Serialize};
use snow_core::renderer::DisplayBuffer;
use std::fmt::Display;
use strum::EnumIter;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, EnumIter, Eq, PartialEq)]
pub enum ScalingAlgorithm {
    Linear,
    NearestNeighbor,
}

impl ScalingAlgorithm {
    fn texture_options(&self) -> egui::TextureOptions {
        match self {
            Self::Linear => egui::TextureOptions::LINEAR,
            Self::NearestNeighbor => egui::TextureOptions::NEAREST,
        }
    }
}

impl Display for ScalingAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Linear => write!(f, "Linear"),
            Self::NearestNeighbor => write!(f, "Nearest-Neighbor"),
        }
    }
}

pub struct FramebufferWidget {
    frame: Option<DisplayBuffer>,
    frame_recv: Option<Receiver<DisplayBuffer>>,
    viewport_texture: egui::TextureHandle,
    pub scale: f32,
    pub scaling_algorithm: ScalingAlgorithm,
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
            scaling_algorithm: ScalingAlgorithm::Linear,
            display_size: [0, 0],
        }
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

    pub fn connect_receiver(&mut self, recv: Receiver<DisplayBuffer>) {
        self.frame_recv = Some(recv);
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, fullscreen: bool) -> egui::Response {
        if let Some(ref frame_recv) = self.frame_recv {
            if !frame_recv.is_empty() {
                let frame = frame_recv.recv().unwrap();

                self.display_size = [frame.width(), frame.height()];
                self.viewport_texture.set(
                    egui::ColorImage {
                        size: self.display_size.map(|i| i.into()),
                        pixels: Vec::from_iter(
                            frame
                                .chunks_exact(4)
                                .map(|c| egui::Color32::from_rgb(c[0], c[1], c[2])),
                        ),
                    },
                    self.scaling_algorithm.texture_options(),
                );
                self.frame = Some(frame);
            }
        }

        let size = self.viewport_texture.size_vec2();
        let sized_texture = egui::load::SizedTexture::new(&mut self.viewport_texture, size);
        let size = if fullscreen {
            ui.available_size()
        } else {
            self.display_size_max_scaled()
        };
        let response = ui.add(
            egui::Image::new(sized_texture)
                .fit_to_fraction(Vec2::new(1.0, 1.0))
                .max_size(size)
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
