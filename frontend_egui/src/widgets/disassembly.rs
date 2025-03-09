use crate::emulator::EmulatorState;
use eframe::egui;
use snow_core::cpu_m68k::cpu::Breakpoint;

pub struct Disassembly {}

impl Disassembly {
    pub fn new() -> Self {
        Self {}
    }

    pub fn draw(&self, ui: &mut egui::Ui, state: &EmulatorState) {
        use egui_extras::{Column, TableBuilder};

        let code = state.get_disassembly();
        let pc = state.get_pc();

        let available_height = ui.available_height();

        TableBuilder::new(ui)
            .max_scroll_height(available_height)
            .auto_shrink(false)
            .column(Column::exact(40.0))
            .column(Column::exact(70.0))
            .column(Column::exact(100.0))
            .column(Column::initial(120.0))
            .striped(true)
            .body(|mut body| {
                for c in code {
                    body.row(12.0, |mut row| {
                        row.col(|ui| {
                            if ui
                                .add(
                                    egui::Label::new(egui::RichText::new(
                                        if state.get_breakpoints().contains(&Breakpoint::Execution(c.addr)) {
                                            egui_material_icons::icons::ICON_RADIO_BUTTON_UNCHECKED
                                        } else {
                                            egui_material_icons::icons::ICON_RADIO_BUTTON_CHECKED
                                        },
                                    ).size(8.0).color(egui::Color32::DARK_RED))
                                    .sense(egui::Sense::click()),
                                )
                                .clicked()
                            {
                                state.toggle_breakpoint(Breakpoint::Execution(c.addr));
                            }
                            if pc == Some(c.addr) {
                                ui.label(
                                    egui::RichText::new(
                                        egui_material_icons::icons::ICON_PLAY_ARROW,
                                    )
                                    .color(egui::Color32::LIGHT_GREEN)
                                    .size(8.0),
                                );
                            }
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(format!(":{:06X}", c.addr))
                                    .family(egui::FontFamily::Monospace)
                                    .size(10.0),
                            );
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:<16}", c.raw_as_string()))
                                    .family(egui::FontFamily::Monospace)
                                    .size(10.0)
                                    .color(egui::Color32::DARK_GRAY),
                            );
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(c.str.to_owned())
                                    .family(egui::FontFamily::Monospace)
                                    .size(10.0),
                            );
                        });
                    });
                }
            });
    }
}
