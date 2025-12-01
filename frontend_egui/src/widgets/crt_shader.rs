//! CRT shader for post-processing framebuffer textures

use eframe::glow;
use eframe::glow::HasContext;

/// crt-lottes-fast.glsl parameters
#[derive(Clone, Copy, Debug)]
pub struct CrtShaderParams {
    pub crt_gamma: f32,
    pub scanline_thinness: f32,
    pub scan_blur: f32,
    pub mask_intensity: f32,
    pub curvature: f32,
    pub corner: f32,

    /// 0.0 - no mask
    /// 1.0 - aperture grille
    /// 2.0 - aperture grille light
    /// 3.0 - shadow mask
    pub mask: f32,
    pub trinitron_curve: f32,
}

impl Default for CrtShaderParams {
    fn default() -> Self {
        Self {
            crt_gamma: 2.1,
            scanline_thinness: 0.5,
            scan_blur: 2.5,
            mask_intensity: 0.20,
            curvature: 0.00,
            corner: 0.0,
            mask: 2.0,
            trinitron_curve: 0.0,
        }
    }
}

const VERTEX_SHADER: &str = r#"
#version 140
in vec2 position;
in vec2 tex_coord;
out vec4 TEX0;

void main() {
    gl_Position = vec4(position, 0.0, 1.0);
    TEX0 = vec4(tex_coord, 0.0, 0.0);
}
"#;

const FRAGMENT_SHADER_SOURCE: &str = include_str!("../../shaders/crt-lottes-fast.glsl");

pub struct CrtShader {
    program: glow::Program,
    vao: glow::VertexArray,
    fbo: glow::Framebuffer,
    output_texture: Option<glow::Texture>,
    output_size: [u32; 2],
}

impl CrtShader {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        unsafe {
            // Compile shaders
            let fragment_source = format!(
                "#version 140\n#define FRAGMENT\n#define PARAMETER_UNIFORM\n{}",
                FRAGMENT_SHADER_SOURCE
            );

            let vertex_shader = gl.create_shader(glow::VERTEX_SHADER)?;
            gl.shader_source(vertex_shader, VERTEX_SHADER);
            gl.compile_shader(vertex_shader);
            if !gl.get_shader_compile_status(vertex_shader) {
                let log = gl.get_shader_info_log(vertex_shader);
                log::error!("Vertex shader failed: {}", log);
                return Err(log);
            }

            let fragment_shader = gl.create_shader(glow::FRAGMENT_SHADER)?;
            gl.shader_source(fragment_shader, &fragment_source);
            gl.compile_shader(fragment_shader);
            if !gl.get_shader_compile_status(fragment_shader) {
                let log = gl.get_shader_info_log(fragment_shader);
                log::error!("Fragment shader failed: {}", log);
                gl.delete_shader(vertex_shader);
                return Err(log);
            }

            let program = gl.create_program()?;
            gl.attach_shader(program, vertex_shader);
            gl.attach_shader(program, fragment_shader);
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                log::error!("Shader linking failed: {}", log);
                return Err(log);
            }

            gl.delete_shader(vertex_shader);
            gl.delete_shader(fragment_shader);

            // Create fullscreen quad VAO
            let vao = gl.create_vertex_array()?;
            gl.bind_vertex_array(Some(vao));

            let vertices: [f32; 16] = [
                -1.0, -1.0, 0.0, 0.0, 1.0, -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 0.0, 1.0,
            ];

            let vbo = gl.create_buffer()?;
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
            let ebo = gl.create_buffer()?;
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
            gl.buffer_data_u8_slice(
                glow::ELEMENT_ARRAY_BUFFER,
                std::slice::from_raw_parts(
                    indices.as_ptr() as *const u8,
                    indices.len() * std::mem::size_of::<u32>(),
                ),
                glow::STATIC_DRAW,
            );

            let pos_loc = gl
                .get_attrib_location(program, "position")
                .ok_or_else(|| "Could not find 'position' attribute".to_string())?;
            gl.enable_vertex_attrib_array(pos_loc);
            gl.vertex_attrib_pointer_f32(pos_loc, 2, glow::FLOAT, false, 16, 0);

            let tex_loc = gl
                .get_attrib_location(program, "tex_coord")
                .ok_or_else(|| "Could not find 'tex_coord' attribute".to_string())?;
            gl.enable_vertex_attrib_array(tex_loc);
            gl.vertex_attrib_pointer_f32(tex_loc, 2, glow::FLOAT, false, 16, 8);

            gl.bind_vertex_array(None);

            // Create FBO
            let fbo = gl.create_framebuffer()?;

            Ok(Self {
                program,
                vao,
                fbo,
                output_texture: None,
                output_size: [0, 0],
            })
        }
    }

    pub fn process_texture_to_pixels(
        &mut self,
        gl: &glow::Context,
        input_texture: glow::Texture,
        texture_size: [u32; 2],
        params: &CrtShaderParams,
    ) -> Option<Vec<u8>> {
        unsafe {
            // Recreate output texture if size changed
            if self.output_size != texture_size {
                if let Some(old_tex) = self.output_texture {
                    gl.delete_texture(old_tex);
                }

                let tex = gl.create_texture().ok()?;
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));

                // Allocate texture storage (no initial data)
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

                self.output_texture = Some(tex);
                self.output_size = texture_size;
            }

            let output_tex = self.output_texture?;

            // Render to FBO
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(output_tex),
                0,
            );

            gl.viewport(0, 0, texture_size[0] as i32, texture_size[1] as i32);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);

            // Render with shader
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(input_texture));

            // Set uniforms
            if let Some(loc) = gl.get_uniform_location(self.program, "Texture") {
                gl.uniform_1_i32(Some(&loc), 0);
            }
            self.set_uniform(
                gl,
                "TextureSize",
                texture_size[0] as f32,
                texture_size[1] as f32,
            );
            self.set_uniform(
                gl,
                "InputSize",
                texture_size[0] as f32,
                texture_size[1] as f32,
            );
            self.set_uniform(
                gl,
                "OutputSize",
                texture_size[0] as f32,
                texture_size[1] as f32,
            );

            // Shader parameters
            self.set_param(gl, "CRT_GAMMA", params.crt_gamma);
            self.set_param(gl, "SCANLINE_THINNESS", params.scanline_thinness);
            self.set_param(gl, "SCAN_BLUR", -params.scan_blur);
            self.set_param(gl, "MASK_INTENSITY", params.mask_intensity);
            self.set_param(gl, "CURVATURE", params.curvature);
            self.set_param(gl, "CORNER", params.corner);
            self.set_param(gl, "MASK", params.mask);
            self.set_param(gl, "TRINITRON_CURVE", params.trinitron_curve);

            gl.bind_vertex_array(Some(self.vao));
            gl.draw_elements(glow::TRIANGLES, 6, glow::UNSIGNED_INT, 0);

            // Read pixels back from FBO
            let mut pixels = vec![0u8; (texture_size[0] * texture_size[1] * 4) as usize];
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

            Some(pixels)
        }
    }

    unsafe fn set_uniform(&self, gl: &glow::Context, name: &str, x: f32, y: f32) {
        if let Some(loc) = gl.get_uniform_location(self.program, name) {
            gl.uniform_2_f32(Some(&loc), x, y);
        }
    }

    unsafe fn set_param(&self, gl: &glow::Context, name: &str, value: f32) {
        if let Some(loc) = gl.get_uniform_location(self.program, name) {
            gl.uniform_1_f32(Some(&loc), value);
        }
    }
}
