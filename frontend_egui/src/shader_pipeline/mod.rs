//! Flexible shader pipeline for post-processing framebuffer textures

use anyhow::Result;
use eframe::glow;
use eframe::glow::HasContext;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod crt_lottes;
pub mod gdv_scanlines;
pub mod image_adjustment;
pub mod parser;

/// Identifies a specific shader in the pipeline
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::EnumIter)]
pub enum ShaderId {
    GdvScanlines,
    CrtLottes,
    ImageAdjustment,
}

impl ShaderId {
    /// Returns the display name for this shader
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::GdvScanlines => "GDV Mini Scanlines",
            Self::CrtLottes => "CRT Lottes",
            Self::ImageAdjustment => "Image Adjustment",
        }
    }

    /// Returns the parsed shader parameters (cached)
    pub fn parameters(&self) -> &'static [ShaderParameter] {
        use once_cell::sync::Lazy;

        static GDV_PARAMS: Lazy<Vec<ShaderParameter>> =
            Lazy::new(|| parser::parse_shader_parameters(gdv_scanlines::SHADER_SOURCE));

        static LOTTES_PARAMS: Lazy<Vec<ShaderParameter>> =
            Lazy::new(|| parser::parse_shader_parameters(crt_lottes::SHADER_SOURCE));

        static IMAGE_ADJUSTMENT_PARAMS: Lazy<Vec<ShaderParameter>> =
            Lazy::new(|| parser::parse_shader_parameters(image_adjustment::SHADER_SOURCE));

        match self {
            Self::GdvScanlines => &GDV_PARAMS,
            Self::CrtLottes => &LOTTES_PARAMS,
            Self::ImageAdjustment => &IMAGE_ADJUSTMENT_PARAMS,
        }
    }

    /// Create a new shader instance
    pub fn create_shader(&self, gl: &glow::Context) -> Result<Box<dyn ShaderPass>> {
        match self {
            Self::GdvScanlines => Ok(Box::new(gdv_scanlines::GdvScanlinesShader::new(gl)?)),
            Self::CrtLottes => Ok(Box::new(crt_lottes::CrtLottesShader::new(gl)?)),
            Self::ImageAdjustment => {
                Ok(Box::new(image_adjustment::ImageAdjustmentShader::new(gl)?))
            }
        }
    }
}

/// Metadata for a single shader parameter
#[derive(Clone, Debug)]
pub struct ShaderParameter {
    pub name: String,
    pub display_name: String,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub step: f32,
}

/// Configuration for a shader instance in the pipeline
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShaderConfig {
    pub id: ShaderId,
    pub enabled: bool,
    pub parameters: HashMap<String, f32>,
}

impl ShaderConfig {
    /// Create a builder for this shader
    pub fn builder(id: ShaderId) -> ShaderConfigBuilder {
        ShaderConfigBuilder::new(id)
    }
}

/// Builder for ShaderConfig
pub struct ShaderConfigBuilder {
    id: ShaderId,
    enabled: bool,
    parameters: HashMap<String, f32>,
}

#[allow(dead_code)]
impl ShaderConfigBuilder {
    /// Create a new builder with defaults from shader parameters from the glsl file
    pub fn new(id: ShaderId) -> Self {
        let parameters = id
            .parameters()
            .iter()
            .map(|p| (p.name.clone(), p.default))
            .collect();

        Self {
            id,
            enabled: true,
            parameters,
        }
    }

    /// Set whether this shader is enabled
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Override a parameter value
    pub fn param(mut self, name: &str, value: f32) -> Self {
        self.parameters.insert(name.to_string(), value);
        self
    }

    /// Build the final ShaderConfig
    pub fn build(self) -> ShaderConfig {
        ShaderConfig {
            id: self.id,
            enabled: self.enabled,
            parameters: self.parameters,
        }
    }
}

/// Trait for individual shader passes
pub trait ShaderPass: Send + Sync {
    /// Returns the compiled OpenGL program
    fn program(&self) -> glow::Program;

    /// Bind custom uniforms before rendering
    /// Standard uniforms (Texture, MVPMatrix, TextureSize, InputSize, OutputSize)
    /// are set by the pipeline automatically
    ///
    /// Default implementation binds all parameters from the HashMap as float uniforms
    unsafe fn bind_custom_uniforms(&self, gl: &glow::Context, params: &HashMap<String, f32>) {
        let program = self.program();
        for (name, &value) in params {
            ShaderPipeline::set_uniform_f32(gl, program, name, value);
        }
    }
}

/// A configured shader pass (shader implementation + configuration)
struct ConfiguredPass {
    shader: Box<dyn ShaderPass>,
    config: ShaderConfig,
}

/// Shader pipeline manager
pub struct ShaderPipeline {
    /// Ordered list of configured shader passes
    passes: Vec<ConfiguredPass>,

    /// Shared fullscreen quad VAO
    vao: glow::VertexArray,

    /// Intermediate framebuffers (one per pass except the last)
    intermediate_fbos: Vec<glow::Framebuffer>,
    intermediate_textures: Vec<Option<glow::Texture>>,

    /// Final output FBO and texture
    output_fbo: glow::Framebuffer,
    output_texture: Option<glow::Texture>,

    /// Output size for texture allocation
    output_size: [u32; 2],
}

impl ShaderPipeline {
    /// Create an empty pipeline
    pub fn new(gl: &glow::Context) -> Result<Self> {
        unsafe {
            // Create fullscreen quad VAO
            let vao = gl.create_vertex_array().map_err(anyhow::Error::msg)?;
            gl.bind_vertex_array(Some(vao));

            // Vertices for rendering a full framebuffer quad
            let vertices: [f32; 32] = [
                -1.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, // Bottom-left
                1.0, -1.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, // Bottom-right
                1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, // Top-right
                -1.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, // Top-left
            ];

            let vbo = gl.create_buffer().map_err(anyhow::Error::msg)?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                std::slice::from_raw_parts(
                    vertices.as_ptr() as *const u8,
                    vertices.len() * std::mem::size_of::<f32>(),
                ),
                glow::STATIC_DRAW,
            );

            let indices: [u32; 6] = [0, 1, 2, 2, 3, 0];
            let ebo = gl.create_buffer().map_err(anyhow::Error::msg)?;
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
            gl.buffer_data_u8_slice(
                glow::ELEMENT_ARRAY_BUFFER,
                std::slice::from_raw_parts(
                    indices.as_ptr() as *const u8,
                    indices.len() * std::mem::size_of::<u32>(),
                ),
                glow::STATIC_DRAW,
            );

            // Set up vertex attributes (standard for all (?) shaders)
            // VertexCoord at location 0 (vec4)
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 4, glow::FLOAT, false, 32, 0);

            // TexCoord at location 1 (vec4)
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 4, glow::FLOAT, false, 32, 16);

            gl.bind_vertex_array(None);

            // Create output FBO
            let output_fbo = gl.create_framebuffer().map_err(anyhow::Error::msg)?;

            Ok(Self {
                passes: vec![],
                vao,
                intermediate_fbos: vec![],
                intermediate_textures: vec![],
                output_fbo,
                output_texture: None,
                output_size: [0, 0],
            })
        }
    }

    /// Add a shader pass to the pipeline
    pub fn add_pass(&mut self, shader: Box<dyn ShaderPass>, config: ShaderConfig) {
        self.passes.push(ConfiguredPass { shader, config });
    }

    /// Update the pipeline's configs
    pub fn update_configs(&mut self, new_configs: &[ShaderConfig]) {
        for (i, new_config) in new_configs.iter().enumerate() {
            assert_eq!(self.passes[i].config.id, new_config.id);
            self.passes[i].config = new_config.clone();
        }
    }

    /// Ensure FBOs and textures are allocated for the current size and pass count
    unsafe fn ensure_resources(
        &mut self,
        gl: &glow::Context,
        texture_size: [u32; 2],
    ) -> Option<()> {
        // Check if we need to reallocate
        let enabled_count = self.passes.iter().filter(|p| p.config.enabled).count();
        let needs_realloc = self.output_size != texture_size
            || self.intermediate_fbos.len() != enabled_count.saturating_sub(1);

        if !needs_realloc {
            return Some(());
        }

        // Clean up old intermediate textures and FBOs
        for tex in self.intermediate_textures.drain(..).flatten() {
            gl.delete_texture(tex);
        }
        for fbo in self.intermediate_fbos.drain(..) {
            gl.delete_framebuffer(fbo);
        }

        // Clean up old output texture
        if let Some(old_tex) = self.output_texture {
            gl.delete_texture(old_tex);
        }

        // Create intermediate FBOs and textures (one for each pass except the last)
        let intermediate_count = enabled_count.saturating_sub(1);
        for _ in 0..intermediate_count {
            let fbo = gl.create_framebuffer().ok()?;
            let tex = gl.create_texture().ok()?;

            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            let size = (texture_size[0] * texture_size[1] * 4) as usize;
            let data = vec![0u8; size];
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                texture_size[0] as i32,
                texture_size[1] as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&data)),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );

            self.intermediate_fbos.push(fbo);
            self.intermediate_textures.push(Some(tex));
        }

        // Create output texture
        let output_tex = gl.create_texture().ok()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(output_tex));
        let size = (texture_size[0] * texture_size[1] * 4) as usize;
        let data = vec![0u8; size];
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            texture_size[0] as i32,
            texture_size[1] as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&data)),
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        self.output_texture = Some(output_tex);

        self.output_size = texture_size;
        Some(())
    }

    /// Process texture through the shader pipeline
    pub fn process_texture_to_pixels(
        &mut self,
        gl: &glow::Context,
        input_texture: glow::Texture,
        texture_size: [u32; 2],
    ) -> Option<Vec<u8>> {
        let enabled_count = self.passes.iter().filter(|p| p.config.enabled).count();

        if enabled_count == 0 {
            return None;
        }

        unsafe {
            self.ensure_resources(gl, texture_size)?;
        }

        let mut current_input = input_texture;

        // Run enabled passes
        for (i, configured_pass) in self.passes.iter().filter(|p| p.config.enabled).enumerate() {
            let pass = &configured_pass.shader;
            let config = &configured_pass.config;
            let program = pass.program();

            let is_last = i == enabled_count - 1;

            let (fbo, output_tex) = if is_last {
                // Last pass: render to final output
                (self.output_fbo, self.output_texture?)
            } else {
                // Intermediate pass
                (self.intermediate_fbos[i], self.intermediate_textures[i]?)
            };

            unsafe {
                // Bind FBO
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(output_tex),
                    0,
                );

                // Check FBO status
                let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
                if status != glow::FRAMEBUFFER_COMPLETE {
                    log::error!("Framebuffer incomplete: 0x{:x}", status);
                    return None;
                }

                gl.viewport(0, 0, texture_size[0] as i32, texture_size[1] as i32);
                gl.clear_color(0.0, 0.0, 0.0, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);

                // Use shader program
                gl.use_program(Some(program));

                // Bind input texture
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(current_input));

                // Set standard uniforms
                if let Some(loc) = gl.get_uniform_location(program, "Texture") {
                    gl.uniform_1_i32(Some(&loc), 0);
                }
                if let Some(loc) = gl.get_uniform_location(program, "MVPMatrix") {
                    let identity: [f32; 16] = [
                        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
                        1.0,
                    ];
                    gl.uniform_matrix_4_f32_slice(Some(&loc), false, &identity);
                }

                Self::set_uniform_vec2(
                    gl,
                    program,
                    "TextureSize",
                    texture_size[0] as f32,
                    texture_size[1] as f32,
                );
                Self::set_uniform_vec2(
                    gl,
                    program,
                    "InputSize",
                    texture_size[0] as f32,
                    texture_size[1] as f32,
                );
                Self::set_uniform_vec2(
                    gl,
                    program,
                    "OutputSize",
                    texture_size[0] as f32,
                    texture_size[1] as f32,
                );

                // Bind shader-specific parameters
                pass.bind_custom_uniforms(gl, &config.parameters);

                // Draw fullscreen quad
                gl.bind_vertex_array(Some(self.vao));
                gl.draw_elements(glow::TRIANGLES, 6, glow::UNSIGNED_INT, 0);
                gl.bind_vertex_array(None);
            }

            // Output of this pass becomes input to next
            current_input = output_tex;
        }

        // Read back final result
        let mut pixels = vec![0u8; (texture_size[0] * texture_size[1] * 4) as usize];

        unsafe {
            gl.read_pixels(
                0,
                0,
                texture_size[0] as i32,
                texture_size[1] as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixels[..])),
            );

            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_vertex_array(None);
        }

        Some(pixels)
    }

    /// Helper to set a vec2 uniform
    unsafe fn set_uniform_vec2(
        gl: &glow::Context,
        program: glow::Program,
        name: &str,
        x: f32,
        y: f32,
    ) {
        let Some(loc) = gl.get_uniform_location(program, name) else {
            return;
        };
        gl.uniform_2_f32(Some(&loc), x, y);
    }

    /// Helper to set a float uniform
    pub unsafe fn set_uniform_f32(
        gl: &glow::Context,
        program: glow::Program,
        name: &str,
        value: f32,
    ) {
        let Some(loc) = gl.get_uniform_location(program, name) else {
            return;
        };
        gl.uniform_1_f32(Some(&loc), value);
    }
}
