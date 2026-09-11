use crate::util::image::load_png_from_bytes_as_texture;
use eframe::egui;
use eframe::egui::Ui;
use itertools::Itertools;
use rand::Rng;
use rand::seq::SliceRandom;

macro_rules! asset_path {
    ($file:literal) => {
        concat!("../../../assets/snowy/", $file)
    };
}
const CAST_LIGHT_MASK_00: &[u8] = include_bytes!(asset_path!("cast_light_mask_00.png"));
const CAST_LIGHT_MASK_SCARF_00: &[u8] = include_bytes!(asset_path!("cast_light_mask_scarf_00.png"));
const MAC_BASE_01: &[u8] = include_bytes!(asset_path!("mac_base_01.png"));
const MAC_BASE_02: &[u8] = include_bytes!(asset_path!("mac_base_02.png"));
const MAC_FACE_LIT_00: &[u8] = include_bytes!(asset_path!("mac_face_lit_00.png"));
const MAC_FACE_UNLIT_00: &[u8] = include_bytes!(asset_path!("mac_face_unlit_00.png"));
const SANTA_HAT_OVERLAY: &[u8] = include_bytes!(asset_path!("santa_hat_overlay.png"));
const SCARF_OVERLAY: &[u8] = include_bytes!(asset_path!("scarf_overlay.png"));
const SKY_DAY_CLOUDS_00: &[u8] = include_bytes!(asset_path!("sky_day_clouds_00.png"));
const SKY_DAY_PLAIN_00: &[u8] = include_bytes!(asset_path!("sky_day_plain_00.png"));
const SKY_PLAIN_00: &[u8] = include_bytes!(asset_path!("sky_plain_00.png"));
const SKY_AURORA_00: &[u8] = include_bytes!(asset_path!("sky_aurora_00.png"));
const SNOW_HAT_OVERLAY_00: &[u8] = include_bytes!(asset_path!("snow_hat_overlay_00.png"));

/// About dialog showing application information
#[allow(dead_code)]
pub struct AboutDialog {
    open: bool,
    img_cast_light_mask_00: egui::TextureHandle,
    img_cast_light_mask_scarf_00: egui::TextureHandle,
    img_mac_base_01: egui::TextureHandle,
    img_mac_base_02: egui::TextureHandle,
    img_mac_face_lit_00: egui::TextureHandle,
    img_mac_face_unlit_00: egui::TextureHandle,
    img_santa_hat_overlay: egui::TextureHandle,
    img_scarf_overlay: egui::TextureHandle,
    img_sky_day_clouds_00: egui::TextureHandle,
    img_sky_day_plain_00: egui::TextureHandle,
    img_sky_plain_00: egui::TextureHandle,
    img_sky_aurora_00: egui::TextureHandle,
    img_snow_hat_overlay_00: egui::TextureHandle,
    snowy_daytime: bool,
    snowy_clouds: bool,
    snowy_scarf: bool,
    snowy_hat: bool,
    snowy_xmas: bool,

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
        "Nolan Check",
    ];

    pub fn new(ctx: &egui::Context) -> Self {
        let mut rng = rand::rng();
        let mut shuffled_thanks = Self::THANKS.to_vec();
        shuffled_thanks.shuffle(&mut rng);
        Self {
            img_cast_light_mask_00: load_png_from_bytes_as_texture(
                ctx,
                CAST_LIGHT_MASK_00,
                "CAST_LIGHT_MASK_00",
            )
            .unwrap(),
            img_cast_light_mask_scarf_00: load_png_from_bytes_as_texture(
                ctx,
                CAST_LIGHT_MASK_SCARF_00,
                "CAST_LIGHT_MASK_SCARF_00",
            )
            .unwrap(),
            img_mac_base_01: load_png_from_bytes_as_texture(ctx, MAC_BASE_01, "MAC_BASE_01")
                .unwrap(),
            img_mac_base_02: load_png_from_bytes_as_texture(ctx, MAC_BASE_02, "MAC_BASE_02")
                .unwrap(),
            img_mac_face_lit_00: load_png_from_bytes_as_texture(
                ctx,
                MAC_FACE_LIT_00,
                "MAC_FACE_LIT_00",
            )
            .unwrap(),
            img_mac_face_unlit_00: load_png_from_bytes_as_texture(
                ctx,
                MAC_FACE_UNLIT_00,
                "MAC_FACE_UNLIT_00",
            )
            .unwrap(),
            img_santa_hat_overlay: load_png_from_bytes_as_texture(
                ctx,
                SANTA_HAT_OVERLAY,
                "SANTA_HAT_OVERLAY",
            )
            .unwrap(),
            img_scarf_overlay: load_png_from_bytes_as_texture(ctx, SCARF_OVERLAY, "SCARF_OVERLAY")
                .unwrap(),
            img_sky_day_clouds_00: load_png_from_bytes_as_texture(
                ctx,
                SKY_DAY_CLOUDS_00,
                "SKY_DAY_CLOUDS_00",
            )
            .unwrap(),
            img_sky_day_plain_00: load_png_from_bytes_as_texture(
                ctx,
                SKY_DAY_PLAIN_00,
                "SKY_DAY_PLAIN_00",
            )
            .unwrap(),
            img_sky_plain_00: load_png_from_bytes_as_texture(ctx, SKY_PLAIN_00, "SKY_PLAIN_00")
                .unwrap(),
            img_sky_aurora_00: load_png_from_bytes_as_texture(ctx, SKY_AURORA_00, "SKY_AURORA_00")
                .unwrap(),
            img_snow_hat_overlay_00: load_png_from_bytes_as_texture(
                ctx,
                SNOW_HAT_OVERLAY_00,
                "SNOW_HAT_OVERLAY_00",
            )
            .unwrap(),
            open: false,
            snowy_hat: false,
            snowy_daytime: false,
            snowy_scarf: false,
            snowy_clouds: false,
            snowy_xmas: false,
            shuffled_thanks,
        }
    }
    pub fn update(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }

        egui::Modal::new(egui::Id::new("About Snow")).show(ctx, |ui| {
            ui.set_width(525.0);
            ui.set_height(270.0);

            ui.horizontal(|ui| {
                ui.add_space(20.0);
                // Left column - Image
                ui.vertical(|ui| {
                    ui.add_space(20.0);

                    self.draw_snowy(ui);
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
                        snow_core::build_version(),
                        snow_core::built_info::CFG_TARGET_ARCH,
                        snow_core::built_info::PROFILE
                    ));
                    ui.label(format!(
                        "Built on {}",
                        snow_core::built_info::BUILT_TIME_UTC
                    ));

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
                                .chunks(3)
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

    fn draw_snowy(&self, ui: &mut Ui) {
        let size = self.img_sky_plain_00.size_vec2();
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());

        ui.put(
            rect,
            egui::Image::new(match (self.snowy_daytime, self.snowy_clouds) {
                (false, false) => &self.img_sky_plain_00,
                (false, true) => &self.img_sky_aurora_00,
                (true, false) => &self.img_sky_day_plain_00,
                (true, true) => &self.img_sky_day_clouds_00,
            }),
        );
        ui.put(rect, egui::Image::new(&self.img_mac_base_01));
        if self.snowy_scarf {
            ui.put(rect, egui::Image::new(&self.img_scarf_overlay));
        }
        if self.snowy_hat {
            if self.snowy_xmas {
                ui.put(rect, egui::Image::new(&self.img_santa_hat_overlay));
            } else {
                ui.put(rect, egui::Image::new(&self.img_snow_hat_overlay_00));
            }
        }
    }

    pub fn open(&mut self) {
        use chrono::{Datelike, Local, Timelike};

        self.open = true;

        let now = Local::now();
        let (month, day) = (now.date_naive().month(), now.date_naive().day());
        let hour = now.time().hour();
        let xmas = (month == 12 && day >= 14) || (month == 1 && day <= 14);

        self.snowy_daytime = (8..20).contains(&hour);
        self.snowy_scarf = rand::rng().random();
        self.snowy_clouds = rand::rng().random();
        self.snowy_hat = xmas || rand::rng().random();
        self.snowy_xmas = xmas;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
}
