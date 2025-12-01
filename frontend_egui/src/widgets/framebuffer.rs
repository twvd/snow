//! Widget that receives frames from the emulator and draws them to a
//! GPU texture-backed image widget.

use std::{fs::File, path::Path};

use anyhow::{bail, Result};
use crossbeam_channel::Receiver;
use eframe::egui;
use eframe::egui::Vec2;
use eframe::egui_glow;
use egui::mutex::Mutex;
use serde::{Deserialize, Serialize};
use snow_core::renderer::DisplayBuffer;
use std::fmt::Display;
use std::sync::Arc;
use strum::EnumIter;

use super::crt_shader::{CrtShader, CrtShaderParams};

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

    // CRT shader
    pub crt_enabled: bool,
    pub crt_params: CrtShaderParams,
    crt_shader: Arc<Mutex<Option<CrtShader>>>,
    crt_output_texture: Option<egui::TextureHandle>,
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
            crt_enabled: false,
            crt_params: CrtShaderParams::default(),
            crt_shader: Arc::new(Mutex::new(None)),
            crt_output_texture: None,
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

        // Process with CRT shader if enabled (using invisible callback)
        if self.crt_enabled {
            self.process_crt_with_callback(ui);
        }

        // Choose which texture to display
        let display_texture = if self.crt_enabled && self.crt_output_texture.is_some() {
            self.crt_output_texture.as_mut().unwrap()
        } else {
            &mut self.viewport_texture
        };

        let size = display_texture.size_vec2();
        let sized_texture = egui::load::SizedTexture::new(display_texture, size);
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

    fn process_crt_with_callback(&mut self, ui: &egui::Ui) {
        let texture_id = self.viewport_texture.id();
        let shader = self.crt_shader.clone();
        let ctx = ui.ctx().clone();
        let texture_size = self.viewport_texture.size();
        let output_handle = Arc::new(Mutex::new(self.crt_output_texture.clone()));
        let params = self.crt_params;

        // Use a callback to get painter access (use full available rect to ensure it's not culled)
        let callback = egui::PaintCallback {
            rect: ui.available_rect_before_wrap(),
            callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
                let gl = painter.gl();

                // Initialize shader if needed
                let mut shader_lock = shader.lock();
                if shader_lock.is_none() {
                    match CrtShader::new(gl) {
                        Ok(s) => {
                            log::info!("CRT shader initialized");
                            *shader_lock = Some(s);
                        }
                        Err(e) => {
                            log::error!("CRT shader failed: {}", e);
                            return;
                        }
                    }
                }

                if let Some(crt_shader) = shader_lock.as_mut() {
                    if let Some(input_tex) = painter.texture(texture_id) {
                        // Process and read back pixels
                        if let Some(pixels) = crt_shader.process_texture_to_pixels(
                            gl,
                            input_tex,
                            [texture_size[0] as u32, texture_size[1] as u32],
                            &params,
                        ) {
                            // Update egui texture with processed pixels
                            let mut handle_lock = output_handle.lock();
                            if let Some(ref mut handle) = *handle_lock {
                                let image =
                                    egui::ColorImage::from_rgba_unmultiplied(texture_size, &pixels);
                                handle.set(image, egui::TextureOptions::LINEAR);
                            }
                        }
                    }
                }
            })),
        };

        ui.painter().add(callback);

        // Create output texture if needed
        if self.crt_output_texture.is_none() {
            self.crt_output_texture = Some(ctx.load_texture(
                "crt_output",
                egui::ColorImage::new(texture_size, egui::Color32::BLACK),
                egui::TextureOptions::LINEAR,
            ));
        }
    }

    /// Writes a screenshot as PNG
    pub fn write_screenshot<W: std::io::Write>(&self, writer: W) -> Result<()> {
        let Some(frame) = self.frame.as_ref() else {
            bail!("No framebuffer available");
        };
        let mut encoder = png::Encoder::new(
            writer,
            self.display_size[0].into(),
            self.display_size[1].into(),
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(frame)?;

        // png crate can't release the inner writer..
        Ok(())
    }

    /// Returns a screenshot as PNG
    pub fn screenshot(&self) -> Result<Vec<u8>> {
        let mut v = vec![];
        self.write_screenshot(&mut v)?;

        Ok(v)
    }

    /// Writes a screenshot as PNG to a file
    pub fn write_screenshot_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        self.write_screenshot(File::create(path.as_ref())?)
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
