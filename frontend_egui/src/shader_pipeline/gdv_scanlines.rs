//! GDV mini scanlines (gdv-mini-scanlines.glsl) shader pass

use super::ShaderPass;
use anyhow::{anyhow, bail, Result};
use eframe::glow;
use eframe::glow::HasContext;

pub const SHADER_SOURCE: &str = include_str!("../../shaders/gdv-mini-scanlines.glsl");

pub struct GdvScanlinesShader {
    program: glow::Program,
}

impl GdvScanlinesShader {
    pub fn new(gl: &glow::Context) -> Result<Self> {
        unsafe {
            // Compile vertex shader
            let vertex_source = format!(
                "#version 140\n#define VERTEX\n#define PARAMETER_UNIFORM\n{}",
                SHADER_SOURCE
            );

            let vertex_shader = gl
                .create_shader(glow::VERTEX_SHADER)
                .map_err(|e| anyhow!("Failed to create vertex shader: {}", e))?;
            gl.shader_source(vertex_shader, &vertex_source);
            gl.compile_shader(vertex_shader);
            if !gl.get_shader_compile_status(vertex_shader) {
                let log = gl.get_shader_info_log(vertex_shader);
                bail!("Vertex shader failed to compile: {}", log);
            }

            // Compile fragment shader
            let fragment_source = format!(
                // texture2D is deprecated in GLSL 140
                "#version 140\n#define FRAGMENT\n#define PARAMETER_UNIFORM\n#define texture2D texture\n{}",
                SHADER_SOURCE
            );

            let fragment_shader = gl
                .create_shader(glow::FRAGMENT_SHADER)
                .map_err(|e| anyhow!("Failed to create fragment shader: {}", e))?;
            gl.shader_source(fragment_shader, &fragment_source);
            gl.compile_shader(fragment_shader);
            if !gl.get_shader_compile_status(fragment_shader) {
                let log = gl.get_shader_info_log(fragment_shader);
                gl.delete_shader(vertex_shader);
                bail!("Fragment shader compile failed: {}", log);
            }

            // Link program
            let program = gl
                .create_program()
                .map_err(|e| anyhow!("Failed to create program: {}", e))?;
            gl.attach_shader(program, vertex_shader);
            gl.attach_shader(program, fragment_shader);

            // Bind attribute locations before linking
            gl.bind_attrib_location(program, 0, "VertexCoord");
            gl.bind_attrib_location(program, 1, "TexCoord");

            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);

                gl.delete_shader(vertex_shader);
                gl.delete_shader(fragment_shader);
                bail!("Linking failed: {}", log);
            }

            gl.delete_shader(vertex_shader);
            gl.delete_shader(fragment_shader);

            Ok(Self { program })
        }
    }
}

impl ShaderPass for GdvScanlinesShader {
    fn program(&self) -> glow::Program {
        self.program
    }
}
