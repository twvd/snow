use eframe::egui;
use itertools::Itertools;
use rand::seq::SliceRandom;

/// About dialog showing application information
pub struct AboutDialog {
    open: bool,
    image: egui::TextureHandle,
    shuffled_thanks: Vec<&'static str>,
}

impl AboutDialog {
    const THANKS: &[&'static str] = &[
        "chip-64bit",
        "gloriouscow",
        "hop",
        "originaldave_",
        "Rubix",
        "Eric Helgeson",
        "Reza Fouladian",
        "KenDesigns",
    ];

    pub fn new(ctx: &egui::Context) -> Self {
        let mut rng = rand::rng();
        let mut shuffled_thanks = Self::THANKS.to_vec();
        shuffled_thanks.shuffle(&mut rng);
        Self {
            image: crate::util::image::load_png_from_bytes_as_texture(
                ctx,
                include_bytes!("../../../docs/src/images/snowmac_small.png"),
                "snowmac_small",
            )
            .unwrap(),
            open: false,
            shuffled_thanks,
        }
    }
    pub fn update(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }

        egui::Modal::new(egui::Id::new("About Snow")).show(ctx, |ui| {
            ui.set_width(500.0);
            ui.set_height(270.0);

            ui.horizontal(|ui| {
                ui.add_space(20.0);
                // Left column - Image
                ui.vertical(|ui| {
                    ui.add_space(20.0);
                    ui.add(egui::Image::new(&self.image));
                });

                ui.add_space(20.0);

                // Right column - Information
                ui.vertical(|ui| {
                    ui.add_space(20.0);

                    // Title
                    ui.label(egui::RichText::new("Snow").size(24.0).strong());

                    ui.add_space(8.0);

                    // Subtitle
                    ui.label(
                        egui::RichText::new("Classic Macintosh emulator")
                            .size(14.0)
                            .color(egui::Color32::GRAY),
                    );
                    ui.label(format!(
                        "Version {} ({} {})",
                        crate::version_string(),
                        crate::built_info::CFG_TARGET_ARCH,
                        crate::built_info::PROFILE
                    ));
                    ui.label(format!("Built on {}", crate::built_info::BUILT_TIME_UTC));

                    ui.add_space(16.0);

                    // License and copyright
                    ui.label("Copyright (c) Thomas W. - thomas@thomasw.dev");
                    ui.label("Licensed under the MIT License");

                    ui.add_space(16.0);

                    // Credits
                    ui.separator();
                    ui.add_space(10.0);
                    ui.label("Thanks and greetings to:");
                    ui.label(
                        egui::RichText::new(
                            self.shuffled_thanks
                                .chunks(4)
                                .map(|names| names.join(", "))
                                .join("\n"),
                        )
                        .italics(),
                    );
                });
            });

            ui.add_space(20.0);
            ui.separator();

            // Close button
            egui::Sides::new().show(
                ui,
                |_ui| {},
                |ui| {
                    if ui.button("Close").clicked() {
                        self.open = false;
                    }
                },
            );
        });
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
}
