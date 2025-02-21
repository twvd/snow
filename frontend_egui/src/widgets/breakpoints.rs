use crate::emulator::EmulatorState;
use eframe::egui;
use eframe::egui::RichText;

pub struct BreakpointsWidget {}

impl BreakpointsWidget {
    pub fn new() -> Self {
        Self {}
    }

    pub fn draw(&self, ui: &mut egui::Ui, state: &EmulatorState) {
        use egui_extras::{Column, TableBuilder};
        let available_height = ui.available_height();

        TableBuilder::new(ui)
            .max_scroll_height(available_height)
            .auto_shrink(false)
            .column(Column::exact(20.0))
            .column(Column::exact(100.0))
            .striped(true)
            .body(|mut body| {
                for &addr in state.get_breakpoints() {
                    body.row(18.0, |mut row| {
                        row.col(|ui| {
                            if ui.button(egui_material_icons::icons::ICON_DELETE).clicked() {
                                state.toggle_breakpoint(addr);
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::from(format!("{:06X}", addr)));
                        });
                    });
                }
            });
    }
}
