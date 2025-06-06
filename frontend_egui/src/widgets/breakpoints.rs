use crate::consts::{TRAPS, VECTORS};
use crate::emulator::EmulatorState;
use eframe::egui;
use eframe::egui::RichText;
use snow_core::bus::Address;
use snow_core::cpu_m68k::cpu::{Breakpoint, BusBreakpoint};

pub struct BreakpointsWidget {
    exec_input: String,
    bus_input: String,
    systrap_input: String,
    linea_input: String,
    linef_input: String,
    vector_input: String,
    vector_search_input: String,
    intlevel_input: u8,
    bus_r: bool,
    bus_w: bool,
    added_bp: Option<Breakpoint>,
    traps: Vec<String>,
    vectors: Vec<String>,
}

impl Default for BreakpointsWidget {
    fn default() -> Self {
        Self {
            exec_input: String::new(),
            bus_input: String::new(),
            systrap_input: String::new(),
            linea_input: String::new(),
            linef_input: String::new(),
            vector_input: String::new(),
            vector_search_input: String::new(),
            intlevel_input: 1,
            bus_r: true,
            bus_w: false,
            added_bp: None,
            traps: Vec::from_iter(
                crate::consts::TRAPS
                    .iter()
                    .map(|(a, t)| format!("{} (${:04X})", t, a)),
            ),
            vectors: Vec::from_iter(
                crate::consts::VECTORS
                    .iter()
                    .map(|(a, t)| format!("{} (${:08X})", t, a)),
            ),
        }
    }
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
                        self.bus_input.clear();
                    }
                });
            });
            ui.collapsing("Add A-line trap (system trap) breakpoint", |ui| {
                ui.horizontal(|ui| {
                    ui.label("System trap: ");
                    ui.add(
                        egui_dropdown::DropDownBox::from_iter(
                            &self.traps,
                            "breakpoints_systrap",
                            &mut self.systrap_input,
                            |ui, trap| ui.selectable_label(false, trap),
                        )
                        .filter_by_input(true)
                        .select_on_focus(true)
                        .hint_text("Search system traps"),
                    );

                    let selected = self
                        .systrap_input
                        .chars()
                        .skip_while(|c| *c != '$')
                        .skip(1)
                        .take(4)
                        .collect::<String>();
                    if ui
                        .add_enabled(
                            u16::from_str_radix(&selected, 16).is_ok(),
                            egui::Button::new("Add breakpoint"),
                        )
                        .clicked()
                    {
                        self.added_bp = Some(Breakpoint::LineA(
                            u16::from_str_radix(&selected, 16).unwrap(),
                        ));
                        self.systrap_input.clear();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Opcode (hex, Axxx): ");
                    ui.text_edit_singleline(&mut self.linea_input);
                    if ui
                        .add_enabled(
                            u16::from_str_radix(&self.linea_input, 16)
                                .is_ok_and(|a| a & 0xF000 == 0xA000),
                            egui::Button::new("Add breakpoint"),
                        )
                        .clicked()
                    {
                        self.added_bp = Some(Breakpoint::LineA(
                            u16::from_str_radix(&self.linea_input, 16).unwrap(),
                        ));
                        self.linea_input.clear();
                    }
                });
            });
            ui.collapsing("Add F-line trap breakpoint", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Opcode (hex, Fxxx): ");
                    ui.text_edit_singleline(&mut self.linef_input);
                    if ui
                        .add_enabled(
                            u16::from_str_radix(&self.linef_input, 16)
                                .is_ok_and(|a| a & 0xF000 == 0xF000),
                            egui::Button::new("Add breakpoint"),
                        )
                        .clicked()
                    {
                        self.added_bp = Some(Breakpoint::LineF(
                            u16::from_str_radix(&self.linef_input, 16).unwrap(),
                        ));
                        self.linef_input.clear();
                    }
                });
            });
            ui.collapsing("Add interrupt level breakpoint", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Level: ");
                    ui.add(egui::Slider::new(&mut self.intlevel_input, 1..=7));
                    if ui.button("Add breakpoint").clicked() {
                        self.added_bp = Some(Breakpoint::InterruptLevel(self.intlevel_input));
                        self.vector_search_input.clear();
                    }
                });
            });
            ui.collapsing("Add exception vector breakpoint", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Vector: ");
                    ui.add(
                        egui_dropdown::DropDownBox::from_iter(
                            &self.vectors,
                            "breakpoints_vectors",
                            &mut self.vector_search_input,
                            |ui, trap| ui.selectable_label(false, trap),
                        )
                        .filter_by_input(true)
                        .select_on_focus(true)
                        .hint_text("Search exception vectors"),
                    );

                    let selected = self
                        .vector_search_input
                        .chars()
                        .skip_while(|c| *c != '$')
                        .skip(1)
                        .take(8)
                        .collect::<String>();
                    if ui
                        .add_enabled(
                            u16::from_str_radix(&selected, 16).is_ok(),
                            egui::Button::new("Add breakpoint"),
                        )
                        .clicked()
                    {
                        self.added_bp = Some(Breakpoint::ExceptionVector(
                            Address::from_str_radix(&selected, 16).unwrap(),
                        ));
                        self.vector_search_input.clear();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Vector address (hex): ");
                    ui.text_edit_singleline(&mut self.vector_input);
                    if ui
                        .add_enabled(
                            Address::from_str_radix(&self.vector_input, 16).is_ok(),
                            egui::Button::new("Add breakpoint"),
                        )
                        .clicked()
                    {
                        self.added_bp = Some(Breakpoint::ExceptionVector(
                            Address::from_str_radix(&self.vector_input, 16).unwrap(),
                        ));
                        self.vector_input.clear();
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
                    for &bp in state.get_breakpoints().iter().filter(|&&bp| {
                        !matches!(bp, Breakpoint::StepOut(_) | Breakpoint::StepOver(_))
                    }) {
                        body.row(18.0, |mut row| {
                            row.col(|ui| {
                                if ui.button(egui_material_icons::icons::ICON_DELETE).clicked() {
                                    state.toggle_breakpoint(bp);
                                }
                            });
                            row.col(|ui| {
                                ui.label(RichText::from(match bp {
                                    Breakpoint::Execution(addr) => {
                                        format!("Execution: ${:08X}", addr)
                                    }
                                    Breakpoint::Bus(BusBreakpoint::Read, addr) => {
                                        format!("Bus access (R): ${:08X}", addr)
                                    }
                                    Breakpoint::Bus(BusBreakpoint::Write, addr) => {
                                        format!("Bus access (W): ${:08X}", addr)
                                    }
                                    Breakpoint::Bus(BusBreakpoint::ReadWrite, addr) => {
                                        format!("Bus access (R/W): ${:08X}", addr)
                                    }
                                    Breakpoint::InterruptLevel(i) => {
                                        format!("Int level: {}", i)
                                    }
                                    Breakpoint::LineA(i) => {
                                        format!(
                                            "LINEA: ${:04X} {}",
                                            i,
                                            TRAPS
                                                .iter()
                                                .find(|(t, _)| i == *t)
                                                .map(|s| format!("({})", s.1))
                                                .unwrap_or_default()
                                        )
                                    }
                                    Breakpoint::LineF(i) => {
                                        format!("LINEF: ${:04X}", i)
                                    }
                                    Breakpoint::ExceptionVector(i) => {
                                        format!(
                                            "Vector: ${:08X} {}",
                                            i,
                                            VECTORS
                                                .iter()
                                                .find(|(t, _)| i == *t)
                                                .map(|s| format!("({})", s.1))
                                                .unwrap_or_default()
                                        )
                                    }
                                    Breakpoint::StepOver(_) => unreachable!(),
                                    Breakpoint::StepOut(_) => unreachable!(),
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
