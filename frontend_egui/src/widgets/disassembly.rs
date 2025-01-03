use crate::emulator::DisassemblyListing;
use eframe::egui;

pub struct Disassembly<'a> {
    code: &'a DisassemblyListing,
}

impl<'a> Disassembly<'a> {
    pub fn new(code: &'a DisassemblyListing) -> Disassembly<'a> {
        Self { code }
    }

    pub fn draw(&self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        TableBuilder::new(ui)
            .column(Column::auto())
            .column(Column::auto())
            .body(|mut body| {
                for c in self.code {
                    body.row(20.0, |mut row| {
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:06X}", c.addr))
                                    .family(egui::FontFamily::Monospace),
                            );
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
