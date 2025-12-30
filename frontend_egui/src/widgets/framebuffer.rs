//! Widget that receives frames from the emulator and draws them to a
//! GPU texture-backed image widget.

use std::{fs::File, path::Path};

use crate::shader_pipeline::{ShaderConfig, ShaderId, ShaderPipeline};
use anyhow::{bail, Result};
use crossbeam_channel::Receiver;
use eframe::egui;
use eframe::egui::Vec2;
use eframe::egui_glow;
use egui::mutex::Mutex;
use serde::{Deserialize, Serialize};
use snow_core::mac::MacModel;
use snow_core::renderer::DisplayBuffer;
use std::fmt::Display;
use std::sync::Arc;
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

    // Shader pipeline
    pub shader_enabled: bool,
    shader_pipeline: Arc<Mutex<Option<ShaderPipeline>>>,
    shader_configs: Vec<ShaderConfig>,
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
            scale: 2.0,
            scaling_algorithm: ScalingAlgorithm::Linear,
            display_size: [0, 0],
            shader_enabled: false,
            shader_pipeline: Arc::new(Mutex::new(None)),
            shader_configs: Self::default_shader_configs(MacModel::Plus),
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

        // Run the framebuffer through the CRT shader if enabled
        if self.shader_enabled {
            self.trigger_crt_shader(ui);
        }

        // Choose which texture to display
        // Only use shader output if at least one shader pass is enabled
        let any_shader_enabled = self.shader_configs.iter().any(|c| c.enabled);
        let display_texture =
            if self.shader_enabled && any_shader_enabled && self.crt_output_texture.is_some() {
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

    fn trigger_crt_shader(&mut self, ui: &egui::Ui) {
        // Copy references to move into the callback
        let texture_id = self.viewport_texture.id();
        let pipeline = self.shader_pipeline.clone();
        let ctx = ui.ctx().clone();
        let texture_size = self.viewport_texture.size();
        let output_handle = Arc::new(Mutex::new(self.crt_output_texture.clone()));
        let configs = self.shader_configs.clone();
        let scaling_algorithm = self.scaling_algorithm;

        // Use a callback to get painter access (use full available rect to ensure it's not culled)
        let callback = egui::PaintCallback {
            // Use a simple 1x1 rect to ensure the callback always fires
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1.0, 1.0)),
            callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
                let gl = painter.gl();

                // Initialize shader pipeline if needed
                let mut pipeline_lock = pipeline.lock();
                if pipeline_lock.is_none() {
                    match ShaderPipeline::new(gl) {
                        Ok(mut p) => {
                            // Add shaders based on configs
                            for config in &configs {
                                match config.id.create_shader(gl) {
                                    Ok(shader) => {
                                        p.add_pass(shader, config.clone());
                                    }
                                    Err(e) => {
                                        log::error!(
                                            "Failed to create {} shader: {}",
                                            config.id.display_name(),
                                            e
                                        );
                                    }
                                }
                            }

                            log::info!("Shader pipeline initialized");
                            *pipeline_lock = Some(p);
                        }
                        Err(e) => {
                            log::error!("Shader pipeline failed: {}", e);
                            return;
                        }
                    }
                }

                if let Some(pipeline) = pipeline_lock.as_mut() {
                    pipeline.update_configs(&configs);

                    if let Some(input_tex) = painter.texture(texture_id) {
                        // Process through shader pipeline
                        if let Some(pixels) = pipeline.process_texture_to_pixels(
                            gl,
                            input_tex,
                            [texture_size[0] as u32, texture_size[1] as u32],
                        ) {
                            // Update egui texture with processed pixels
                            // TODO skip the unneccesary copies from VRAM and present the output
                            // texture directly?
                            let mut handle_lock = output_handle.lock();
                            if let Some(ref mut handle) = *handle_lock {
                                let image =
                                    egui::ColorImage::from_rgba_unmultiplied(texture_size, &pixels);
                                handle.set(image, scaling_algorithm.texture_options());
                            }
                        }
                    }
                }
            })),
        };

        // Using the debug painter guarantees callback will be called
        ui.ctx().debug_painter().add(callback);

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

    /// Creates default shader configs for a given Mac model
    fn default_shader_configs(model: MacModel) -> Vec<ShaderConfig> {
        match model {
            MacModel::Early128K
            | MacModel::Early512K
            | MacModel::Early512Ke
            | MacModel::Plus
            | MacModel::SE
            | MacModel::SeFdhd
            | MacModel::Classic
            | MacModel::SE30 => {
                vec![
                    ShaderConfig::builder(ShaderId::ImageAdjustment)
                        .enabled(false)
                        .build(),
                    ShaderConfig::builder(ShaderId::GdvScanlines)
                        .param("BEAM", 5.0)
                        .param("SCANLINE", 1.00)
                        .build(),
                    ShaderConfig::builder(ShaderId::CrtLottes)
                        .param("CRT_GAMMA", 2.1)
                        .param("SCANLINE_THINNESS", 0.70)
                        .param("SCAN_BLUR", 1.6)
                        .param("MASK_INTENSITY", 0.00)
                        .param("CURVATURE", 0.00)
                        .param("CORNER", 2.0)
                        .param("MASK", 0.0)
                        .param("TRINITRON_CURVE", 0.0)
                        .build(),
                ]
            }
            MacModel::MacII | MacModel::MacIIFDHD | MacModel::MacIIx | MacModel::MacIIcx => {
                vec![
                    ShaderConfig::builder(ShaderId::ImageAdjustment)
                        .enabled(false)
                        .build(),
                    ShaderConfig::builder(ShaderId::GdvScanlines)
                        .param("BEAM", 5.0)
                        .param("SCANLINE", 0.85)
                        .build(),
                    ShaderConfig::builder(ShaderId::CrtLottes)
                        .param("CRT_GAMMA", 2.1)
                        .param("SCANLINE_THINNESS", 0.5)
                        .param("SCAN_BLUR", 2.5)
                        .param("MASK_INTENSITY", 0.10)
                        .param("CURVATURE", 0.00)
                        .param("CORNER", 1.0)
                        .param("MASK", 2.0)
                        .param("TRINITRON_CURVE", 0.0)
                        .build(),
                ]
            }
        }
    }

    /// Loads CRT shader defaults for particular Mac models
    pub fn load_shader_defaults(&mut self, model: MacModel) {
        self.shader_configs = Self::default_shader_configs(model);
        // Clear the pipeline to force reinitialization with new configs
        self.reset_pipeline();
    }

    /// Clears the shader pipeline, forcing reinitialization on next render
    pub fn reset_pipeline(&self) {
        *self.shader_pipeline.lock() = None;
    }

    /// Exports the current shader configuration
    pub fn export_config(&self) -> Vec<ShaderConfig> {
        self.shader_configs.clone()
    }

    /// Imports shader configuration and resets the pipeline
    pub fn import_config(&mut self, configs: Vec<ShaderConfig>) {
        self.shader_configs = configs;
        self.reset_pipeline();
    }

    /// Returns a mutable reference to shader configs
    pub fn shader_configs_mut(&mut self) -> &mut Vec<ShaderConfig> {
        &mut self.shader_configs
    }

    /// Returns the number of shader configs
    pub fn shader_config_count(&self) -> usize {
        self.shader_configs.len()
    }

    /// Moves a shader config up in the pipeline (towards index 0)
    pub fn move_shader_up(&mut self, index: usize) -> bool {
        if index > 0 && index < self.shader_configs.len() {
            self.shader_configs.swap(index, index - 1);
            self.reset_pipeline();
            true
        } else {
            false
        }
    }

    /// Moves a shader config down in the pipeline (towards end)
    pub fn move_shader_down(&mut self, index: usize) -> bool {
        if index < self.shader_configs.len().saturating_sub(1) {
            self.shader_configs.swap(index, index + 1);
            self.reset_pipeline();
            true
        } else {
            false
        }
    }

    /// Removes a shader from the pipeline at the given index
    pub fn remove_shader(&mut self, index: usize) -> bool {
        if index < self.shader_configs.len() {
            self.shader_configs.remove(index);
            self.reset_pipeline();
            true
        } else {
            false
        }
    }

    /// Adds a new shader to the end of the pipeline with default settings
    pub fn add_shader(&mut self, id: ShaderId) {
        let config = ShaderConfig::builder(id).build();
        self.shader_configs.push(config);
        self.reset_pipeline();
    }

    /// Returns a list of shader IDs that are not currently in the pipeline
    pub fn available_shaders(&self) -> Vec<ShaderId> {
        use strum::IntoEnumIterator;
        ShaderId::iter()
            .filter(|id| !self.shader_configs.iter().any(|c| c.id == *id))
            .collect()
    }
}
