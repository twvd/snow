use anyhow::Result;
use crossbeam_channel::Receiver;
use eframe::egui;
use eframe::egui::Vec2;
use snow_core::emulator::comm::{EmulatorCommand, EmulatorSpeed};
use snow_core::emulator::Emulator;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::mac::MacModel;
use snow_core::renderer::DisplayBuffer;
use snow_core::tickable::Tickable;
use std::sync::atomic::Ordering;
use std::thread;
use std::thread::JoinHandle;

pub struct SnowGui {
    viewport_texture: egui::TextureHandle,
    frame_recv: Option<Receiver<DisplayBuffer>>,
    emuthread: Option<JoinHandle<()>>,
}

impl SnowGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            viewport_texture: cc.egui_ctx.load_texture(
                "viewport",
                egui::ColorImage::example(),
                egui::TextureOptions::NEAREST,
            ),
            frame_recv: None,
            emuthread: None,
        }
    }

    pub fn init_emulator(&mut self) -> Result<()> {
        let rom = include_bytes!("../../plus3.rom");

        // Initialize emulator
        let (mut emulator, frame_recv) = Emulator::new(rom, MacModel::Plus)?;
        let cmd = emulator.create_cmd_sender();
        // TODO audio
        cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Video));
        cmd.send(EmulatorCommand::Run)?;

        self.frame_recv = Some(frame_recv);

        // Spin up emulator thread
        let emuthread = thread::spawn(move || loop {
            match emulator.tick(1) {
                Ok(0) => break,
                Ok(_) => (),
                Err(e) => panic!("Emulator error: {}", e),
            }
        });

        self.emuthread = Some(emuthread);

        Ok(())
    }

    #[inline(always)]
    fn convert_framebuffer(framebuffer: DisplayBuffer) -> Vec<egui::Color32> {
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
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if ui.add(egui::Button::new("Start")).clicked() {
                self.init_emulator();
            }

            if let Some(ref frame_recv) = self.frame_recv {
                if !frame_recv.is_empty() {
                    let frame = frame_recv.recv().unwrap();

                    self.viewport_texture.set(
                        egui::ColorImage {
                            size: [SCREEN_WIDTH, SCREEN_HEIGHT],
                            pixels: Self::convert_framebuffer(frame),
                        },
                        egui::TextureOptions::NEAREST,
                    );
                }
            }

            let size = self.viewport_texture.size_vec2();
            let sized_texture = egui::load::SizedTexture::new(&mut self.viewport_texture, size);
            ui.add(egui::Image::new(sized_texture).fit_to_fraction(Vec2::new(1.0, 1.0)));
        });
    }
}
