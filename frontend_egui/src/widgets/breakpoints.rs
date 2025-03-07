use crate::emulator::EmulatorState;
use eframe::egui;
use eframe::egui::RichText;
use snow_core::bus::Address;

#[derive(Default)]
pub struct BreakpointsWidget {
    newbp_input: String,
    added_bp: Option<Address>,
}

impl BreakpointsWidget {
    pub fn draw(&mut self, ui: &mut egui::Ui, state: &EmulatorState) {
        use egui_extras::{Column, TableBuilder};
        let available_height = ui.available_height();

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("Address (hex): ");
                ui.text_edit_singleline(&mut self.newbp_input);
                if ui
                    .add_enabled(
                        Address::from_str_radix(&self.newbp_input, 16).is_ok(),
                        egui::Button::new("Add breakpoint"),
                    )
                    .clicked()
                {
                    self.added_bp = Some(Address::from_str_radix(&self.newbp_input, 16).unwrap());
                    self.newbp_input.clear();
                }
            });
            ui.separator();

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
        });
    }

    pub fn take_added_bp(&mut self) -> Option<Address> {
        self.added_bp.take()
    }
}
