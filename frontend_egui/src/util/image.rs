use eframe::egui;

/// Load PNG from byte slice
pub fn load_png_from_bytes_as_texture(
    ctx: &egui::Context,
    png_bytes: &[u8],
    texture_name: &str,
) -> Result<egui::TextureHandle, Box<dyn std::error::Error>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));

    let mut transformations = png::Transformations::normalize_to_color8();
    transformations.insert(png::Transformations::ALPHA);
    decoder.set_transformations(transformations);

    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;

    let pixels: Vec<egui::Color32> = buf
        .chunks_exact(4)
        .map(|rgba| egui::Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]))
        .collect();

    let color_image = egui::ColorImage {
        size: [info.width as usize, info.height as usize],
        pixels,
    };

    Ok(ctx.load_texture(texture_name, color_image, egui::TextureOptions::default()))
}
