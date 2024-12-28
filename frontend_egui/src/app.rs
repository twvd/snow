use eframe::egui;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};

pub struct SnowGui {
    viewport_texture: egui::TextureHandle,
}

impl SnowGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            viewport_texture: cc.egui_ctx.load_texture(
                "viewport",
                egui::ColorImage::example(),
                egui::TextureOptions::NEAREST,
            ),
        }
    }
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Snow");

            let pixels: Vec<egui::Color32> =
                vec![egui::Color32::from_rgb(255, 0, 255); SCREEN_WIDTH * SCREEN_HEIGHT];

            self.viewport_texture.set(
                egui::ColorImage {
                    size: [SCREEN_WIDTH, SCREEN_HEIGHT],
                    pixels,
                },
                egui::TextureOptions::NEAREST,
            );

            let size = self.viewport_texture.size_vec2();
            let sized_texture = egui::load::SizedTexture::new(&mut self.viewport_texture, size);
            ui.add(egui::Image::new(sized_texture).fit_to_exact_size(size));
        });
    }
}
