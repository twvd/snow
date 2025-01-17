use crate::emulator::DisassemblyListing;
use eframe::egui;
use snow_core::bus::Address;

pub struct Disassembly<'a> {
    code: &'a DisassemblyListing,
    pc: Option<Address>,
}

impl<'a> Disassembly<'a> {
    pub fn new(code: &'a DisassemblyListing, pc: Option<Address>) -> Self {
        Self { code, pc }
    }

    pub fn draw(&self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        let available_height = ui.available_height();

        TableBuilder::new(ui)
            .max_scroll_height(available_height)
            .column(Column::exact(20.0))
            .column(Column::exact(70.0))
            .column(Column::exact(100.0))
            .column(Column::initial(100.0))
            .striped(true)
            .body(|mut body| {
                for c in self.code {
                    body.row(18.0, |mut row| {
                        row.col(|ui| {
                            if self.pc == Some(c.addr) {
                                ui.label(
                                    egui::RichText::new(
                                        egui_material_icons::icons::ICON_PLAY_ARROW,
                                    )
                                    .color(egui::Color32::LIGHT_GREEN),
                                );
                            }
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(format!(":{:06X}", c.addr))
                                    .family(egui::FontFamily::Monospace),
                            );
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:<16}", c.raw_as_string()))
                                    .family(egui::FontFamily::Monospace)
                                    .color(egui::Color32::DARK_GRAY),
                            );
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(c.str.to_owned())
                                    .family(egui::FontFamily::Monospace),
                            );
                        });
                    });
                }
            });
    }
}
