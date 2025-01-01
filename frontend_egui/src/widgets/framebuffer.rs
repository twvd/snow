//! Widget that receives frames from the emulator and draws them to a
//! GPU texture-backed image widget.

use std::sync::atomic::Ordering;

use crossbeam_channel::Receiver;
use eframe::egui;
use eframe::egui::Vec2;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::renderer::DisplayBuffer;

pub struct FramebufferWidget {
    frame_recv: Option<Receiver<DisplayBuffer>>,
    viewport_texture: egui::TextureHandle,

    /// Resulting position of the widget
    rect: egui::Rect,
}

impl FramebufferWidget {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            frame_recv: None,
            viewport_texture: cc.egui_ctx.load_texture(
                "viewport",
                egui::ColorImage::example(),
                egui::TextureOptions::NEAREST,
            ),
            rect: egui::Rect::from([egui::Pos2::new(0.0, 0.0), egui::Pos2::new(0.0, 0.0)]),
        }
    }

    #[inline(always)]
    fn convert_framebuffer(framebuffer: &DisplayBuffer) -> Vec<egui::Color32> {
        // TODO optimize this
        let mut out = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT);

        for c in framebuffer.chunks(4) {
            out.push(egui::Color32::from_rgb(
                c[0].load(Ordering::Relaxed),
                c[1].load(Ordering::Relaxed),
                c[2].load(Ordering::Relaxed),
            ));
        }

        out
    }

    pub fn connect_receiver(&mut self, recv: Receiver<DisplayBuffer>) {
        self.frame_recv = Some(recv);
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) {
        if let Some(ref frame_recv) = self.frame_recv {
            if !frame_recv.is_empty() {
                let frame = frame_recv.recv().unwrap();

                self.viewport_texture.set(
                    egui::ColorImage {
                        size: [SCREEN_WIDTH, SCREEN_HEIGHT],
                        pixels: Self::convert_framebuffer(&frame),
                    },
                    egui::TextureOptions::NEAREST,
                );
            }
        }

        let size = self.viewport_texture.size_vec2();
        let sized_texture = egui::load::SizedTexture::new(&mut self.viewport_texture, size);
        let response = ui.add(egui::Image::new(sized_texture).fit_to_fraction(Vec2::new(1.0, 1.0)));
        self.rect = response.rect;
    }

    pub fn rect(&self) -> egui::Rect {
        self.rect
    }
}
