use eframe::egui;
use snow_core::cpu_m68k::regs::RegisterFile;
use snow_core::types::Long;

/// egui widget to display Motorola 68000 register state
pub struct RegistersWidget {
    regs: RegisterFile,
    lastregs: RegisterFile,
}

impl RegistersWidget {
    const COLOR_VALUE: egui::Color32 = egui::Color32::WHITE;
    const COLOR_CHANGED: egui::Color32 = egui::Color32::YELLOW;

    pub fn new() -> Self {
        Self {
            regs: RegisterFile::new(),
            lastregs: RegisterFile::new(),
        }
    }

    /// Updates the current view with a new register state.
    /// Registers that have been changed since then will appear yellow.
    pub fn update_regs(&mut self, regs: RegisterFile) {
        self.lastregs = std::mem::replace(&mut self.regs, regs);
    }

    pub fn draw(&self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        let available_height = ui.available_height();

        TableBuilder::new(ui)
            .max_scroll_height(available_height)
            .column(Column::exact(40.0))
            .column(Column::remainder().at_least(50.0))
            .column(Column::remainder().at_least(60.0))
            .striped(true)
            // TODO this gets messed up?
            //.header(20.0, |mut header| {
            //    header.col(|ui| {
            //        ui.heading("Register");
            //    });
            //    header.col(|ui| {
            //        ui.heading("Hexadecimal");
            //    });
            //    header.col(|ui| {
            //        ui.heading("Decimal");
            //    });
            //})
            .body(|mut body| {
                let mut reg = |name, v: &dyn Fn(&RegisterFile) -> Long| {
                    let color = if v(&self.regs) != v(&self.lastregs) {
                        Self::COLOR_CHANGED
                    } else {
                        Self::COLOR_VALUE
                    };
                    body.row(20.0, |mut row| {
                        row.col(|ui| {
                            ui.label(egui::RichText::new(name));
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:08X}", v(&self.regs)))
                                    .family(egui::FontFamily::Monospace)
                                    .color(color),
                            );
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{}", v(&self.regs)))
                                    .family(egui::FontFamily::Monospace)
                                    .color(color),
                            );
                        });
                    });
                };
                reg("D0", &|r: &RegisterFile| r.read_d::<Long>(0));
                reg("D1", &|r: &RegisterFile| r.read_d::<Long>(1));
                reg("D2", &|r: &RegisterFile| r.read_d::<Long>(2));
                reg("D3", &|r: &RegisterFile| r.read_d::<Long>(3));
                reg("D4", &|r: &RegisterFile| r.read_d::<Long>(4));
                reg("D5", &|r: &RegisterFile| r.read_d::<Long>(5));
                reg("D6", &|r: &RegisterFile| r.read_d::<Long>(6));
                reg("D7", &|r: &RegisterFile| r.read_d::<Long>(7));
                reg("A0", &|r: &RegisterFile| r.read_a::<Long>(0));
                reg("A1", &|r: &RegisterFile| r.read_a::<Long>(1));
                reg("A2", &|r: &RegisterFile| r.read_a::<Long>(2));
                reg("A3", &|r: &RegisterFile| r.read_a::<Long>(3));
                reg("A4", &|r: &RegisterFile| r.read_a::<Long>(4));
                reg("A5", &|r: &RegisterFile| r.read_a::<Long>(5));
                reg("A6", &|r: &RegisterFile| r.read_a::<Long>(6));
                reg("A7", &|r: &RegisterFile| r.read_a::<Long>(7));
                reg("PC", &|r: &RegisterFile| r.pc);
                reg("SSP", &|r: &RegisterFile| r.ssp);
                reg("USP", &|r: &RegisterFile| r.usp);
                body.row(20.0, |mut row| {
                    row.col(|ui| {
                        ui.label(egui::RichText::new("SR"));
                    });
                    row.col(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{:04X}", self.regs.sr.sr()))
                                .family(egui::FontFamily::Monospace)
                                .color(if self.regs.sr == self.lastregs.sr {
                                    Self::COLOR_VALUE
                                } else {
                                    Self::COLOR_CHANGED
                                }),
                        );
                    });
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            let mut flag = |n, v| {
                                ui.label(format!(
                                    "{} {}",
                                    if v {
                                        egui_material_icons::icons::ICON_CHECK_BOX
                                    } else {
                                        egui_material_icons::icons::ICON_CHECK_BOX_OUTLINE_BLANK
                                    },
                                    n
                                ))
                            };
                            flag("C", self.regs.sr.c());
                            flag("V", self.regs.sr.v());
                            flag("Z", self.regs.sr.z());
                            flag("N", self.regs.sr.n());
                            flag("X", self.regs.sr.x());
                            flag("Supervisor", self.regs.sr.supervisor());
                            flag("Trace", self.regs.sr.trace());
                            ui.label(format!("Int mask: {}", self.regs.sr.int_prio_mask()));
                        });
                    });
                });
            });
    }
}
