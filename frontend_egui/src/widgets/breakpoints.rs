use crate::emulator::EmulatorState;
use eframe::egui;
use eframe::egui::RichText;
use snow_core::bus::Address;
use snow_core::cpu_m68k::cpu::{Breakpoint, BusBreakpoint};

#[derive(Default)]
pub struct BreakpointsWidget {
    exec_input: String,
    bus_input: String,
    bus_r: bool,
    bus_w: bool,
    added_bp: Option<Breakpoint>,
}

impl BreakpointsWidget {
    pub fn draw(&mut self, ui: &mut egui::Ui, state: &EmulatorState) {
        use egui_extras::{Column, TableBuilder};
        let available_height = ui.available_height();

        ui.vertical(|ui| {
            ui.collapsing("Add execution breakpoint", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Address (hex): ");
                    ui.text_edit_singleline(&mut self.exec_input);
                    if ui
                        .add_enabled(
                            Address::from_str_radix(&self.exec_input, 16).is_ok_and(|a| a & 1 == 0),
                            egui::Button::new("Add breakpoint"),
                        )
                        .clicked()
                    {
                        self.added_bp = Some(Breakpoint::Execution(
                            Address::from_str_radix(&self.exec_input, 16).unwrap(),
                        ));
                        self.exec_input.clear();
                    }
                });
            });
            ui.collapsing("Add bus access breakpoint", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Address (hex): ");
                    ui.text_edit_singleline(&mut self.bus_input);
                    ui.checkbox(&mut self.bus_r, "R");
                    ui.checkbox(&mut self.bus_w, "W");
                    if ui
                        .add_enabled(
                            Address::from_str_radix(&self.bus_input, 16).is_ok()
                                && (self.bus_r || self.bus_w),
                            egui::Button::new("Add breakpoint"),
                        )
                        .clicked()
                    {
                        self.added_bp = Some(Breakpoint::Bus(
                            match (self.bus_r, self.bus_w) {
                                (true, false) => BusBreakpoint::Read,
                                (false, true) => BusBreakpoint::Write,
                                (true, true) => BusBreakpoint::ReadWrite,
                                _ => unreachable!(),
                            },
                            Address::from_str_radix(&self.bus_input, 16).unwrap(),
                        ));
                        self.exec_input.clear();
                    }
                });
            });
            ui.separator();

            TableBuilder::new(ui)
                .max_scroll_height(available_height)
                .auto_shrink(false)
                .column(Column::exact(20.0))
                .column(Column::remainder())
                .striped(true)
                .body(|mut body| {
                    for &bp in state.get_breakpoints() {
                        body.row(18.0, |mut row| {
                            row.col(|ui| {
                                if ui.button(egui_material_icons::icons::ICON_DELETE).clicked() {
                                    state.toggle_breakpoint(bp);
                                }
                            });
                            row.col(|ui| {
                                ui.label(RichText::from(match bp {
                                    Breakpoint::Execution(addr) => {
                                        format!("Execution: ${:06X}", addr)
                                    }
                                    Breakpoint::Bus(BusBreakpoint::Read, addr) => {
                                        format!("Bus access (R): ${:06X}", addr)
                                    }
                                    Breakpoint::Bus(BusBreakpoint::Write, addr) => {
                                        format!("Bus access (W): ${:06X}", addr)
                                    }
                                    Breakpoint::Bus(BusBreakpoint::ReadWrite, addr) => {
                                        format!("Bus access (R/W): ${:06X}", addr)
                                    }
                                    Breakpoint::InterruptLevel(_) => todo!(),
                                    Breakpoint::InterruptVector(_) => todo!(),
                                    Breakpoint::LineA(_) => todo!(),
                                    Breakpoint::LineF(_) => todo!(),
                                }));
                            });
                        });
                    }
                });
        });
    }

    pub fn take_added_bp(&mut self) -> Option<Breakpoint> {
        self.added_bp.take()
    }
}
