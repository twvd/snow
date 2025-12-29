//! Parser for shader parameter metadata from #pragma parameter directives

use super::ShaderParameter;

/// Parse shader source for #pragma parameter directives
///
/// Format: #pragma parameter NAME "Display Name" default min max step
/// Example: #pragma parameter SCANLINE_THINNESS "Scanline Intensity" 0.5 0.0 1.0 0.1
pub fn parse_shader_parameters(source: &str) -> Vec<ShaderParameter> {
    let mut params = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("#pragma parameter") {
            if let Some(param) = parse_pragma_line(stripped.trim()) {
                params.push(param);
            }
        }
    }

    params
}

fn parse_pragma_line(line: &str) -> Option<ShaderParameter> {
    // Split by whitespace, but preserve quoted strings
    let parts = parse_tokens(line);

    if parts.len() < 5 {
        log::warn!("Invalid #pragma parameter: not enough parts: {}", line);
        return None;
    }

    let name = parts[0].clone();
    let display_name = parts[1].trim_matches('"').to_string();
    let default = parts[2].parse::<f32>().ok()?;
    let min = parts[3].parse::<f32>().ok()?;
    let max = parts[4].parse::<f32>().ok()?;
    let step = if parts.len() > 5 {
        parts[5].parse::<f32>().ok()?
    } else {
        // Default step: 1% of range
        (max - min) / 100.0
    };

    Some(ShaderParameter {
        name,
        display_name,
        default,
        min,
        max,
        step,
    })
}

/// Parse tokens from a line, respecting quoted strings
fn parse_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in line.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ' ' | '\t' if !in_quotes => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_parameter() {
        let source = r#"#pragma parameter BEAM "Scanline Beam" 6.0 4.0 15.0 0.5"#;
        let params = parse_shader_parameters(source);

        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "BEAM");
        assert_eq!(params[0].display_name, "Scanline Beam");
        assert_eq!(params[0].default, 6.0);
        assert_eq!(params[0].min, 4.0);
        assert_eq!(params[0].max, 15.0);
        assert_eq!(params[0].step, 0.5);
    }

    #[test]
    fn test_parse_parameter_without_step() {
        let source = r#"#pragma parameter SCANLINE "Scanline Strength" 1.35 0.5 2.5"#;
        let params = parse_shader_parameters(source);

        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "SCANLINE");
        assert_eq!(params[0].step, (2.5 - 0.5) / 100.0); // Auto-calculated
    }

    #[test]
    fn test_parse_multiple_parameters() {
        let source = r#"
            #pragma parameter BEAM "Scanline Beam" 6.0 4.0 15.0 0.5
            #pragma parameter SCANLINE "Scanline Strength" 1.35 0.5 2.5 0.1
            #pragma parameter CRT_GAMMA "CRT Gamma" 2.1 0.0 5.0 0.1
        "#;
        let params = parse_shader_parameters(source);

        assert_eq!(params.len(), 3);
        assert_eq!(params[0].name, "BEAM");
        assert_eq!(params[1].name, "SCANLINE");
        assert_eq!(params[2].name, "CRT_GAMMA");
    }

    #[test]
    fn test_parse_with_code_context() {
        let source = r#"
            #ifdef FRAGMENT
            uniform sampler2D Texture;
            #pragma parameter MASK_INTENSITY "Mask Intensity" 0.2 0.0 1.0 0.05

            void main() {
                // blahblahblah
            }
            #endif
        "#;
        let params = parse_shader_parameters(source);

        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "MASK_INTENSITY");
    }
}
