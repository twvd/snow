use crate::{emulator::EmulatorState, uniform::UniformMethods};
use eframe::egui;
use snow_core::cpu_m68k::cpu::Breakpoint;
use snow_core::mac::MacModel;
use std::collections::{BTreeMap, HashMap};

pub struct DisassemblyWidget {
    /// Low memory addresses and names
    low_memory: BTreeMap<u32, String>,
    /// Function addresses and names
    function_names: BTreeMap<u32, String>,
    /// Label addresses and names
    label_names: BTreeMap<u32, String>,
    /// List of embedded map files
    map_files: HashMap<&'static str, &'static str>,
}

impl DisassemblyWidget {
    pub fn new() -> Self {
        let low_memory: BTreeMap<u32, String> = BTreeMap::new();
        let function_names: BTreeMap<u32, String> = BTreeMap::new();
        let label_names: BTreeMap<u32, String> = BTreeMap::new();

        // Embed the map files in the binary
        let map_files: HashMap<&str, &str> = include!("../../map_files.rs.inc");

        Self {
            low_memory,
            function_names,
            label_names,
            map_files,
        }
    }

    /// Get the name for an address
    fn get_name_for_address(&self, addr: u32) -> Option<&str> {
        self.function_names
            .get(&addr)
            .or_else(|| self.label_names.get(&addr))
            .or_else(|| self.low_memory.get(&addr))
            .map(|s| s.as_str())
    }

    /// Find the nearest prior function and return its address
    fn find_nearest_function(&self, target: u32) -> Option<u32> {
        self.function_names
            .range(..=target)
            .next_back()
            .map(|(addr, _)| *addr)
    }

    /// Returns a string of the nearest prior function and the distance from it
    fn get_nearest_function_with_distance(&self, addr: u32) -> String {
        let nearest_function = self.find_nearest_function(addr);
        if addr == nearest_function.unwrap_or(0) {
            self.get_name_for_address(addr).unwrap_or("???").to_string()
        } else {
            format!(
                "{}+{:X}",
                self.get_name_for_address(nearest_function.unwrap_or(0))
                    .unwrap_or("???"),
                addr.saturating_sub(nearest_function.unwrap_or(0))
            )
        }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, state: &EmulatorState, labels: bool) {
        use egui_extras::{Column, TableBuilder};

        let code = state.get_disassembly();
        let pc = state.get_pc();
        let Some(model) = state.get_model() else {
            // Emulator not initialized
            return;
        };

        let rom_size = match model {
            MacModel::Plus => 0x20000,
            MacModel::SE
            | MacModel::SeFdhd
            | MacModel::MacII
            | MacModel::MacIIx
            | MacModel::MacIIcx
            | MacModel::SE30 => 0x40000,
            MacModel::Classic => 0x80000,
            _ => 0x200000,
        };

        // Get the map file for this model and load into the tables
        if let Some(map_filename) = match model {
            MacModel::Plus => Some("MacPlusROM"),
            MacModel::SE | MacModel::SeFdhd => Some("MacSEROM"),
            MacModel::Classic => Some("MacClassicROM"),
            MacModel::MacII => Some("MacIIROM"),
            MacModel::MacIIFDHD | MacModel::MacIIx | MacModel::MacIIcx | MacModel::SE30 => {
                Some("MacIIxROM")
            }
            _ => None,
        } {
            if let Some(map_file) = self.map_files.get(map_filename) {
                for line in map_file.lines() {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(addr) = u32::from_str_radix(parts[1], 16) {
                            if parts.get(2).is_some_and(|&t| t == "f") {
                                self.function_names.insert(addr, parts[0].to_string());
                            } else {
                                self.label_names.insert(addr, parts[0].to_string());
                            }
                        }
                    }
                }
            }
        }

        // Load low memory labels
        if let Some(map_file) = self.map_files.get("LowMem") {
            for line in map_file.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(addr) = u32::from_str_radix(parts[1], 16) {
                        self.low_memory.insert(addr, parts[0].to_string());
                    }
                }
            }
        }

        let available_height = ui.available_height();

        TableBuilder::new(ui)
            .max_scroll_height(available_height)
            .auto_shrink(false)
            .column(Column::exact(40.0))
            .column(Column::exact(70.0))
            .column(Column::exact(100.0))
            .column(Column::remainder())
            .striped(true)
            .body(|mut body| {
                if labels {
                    // If we're in the function table, show the nearest function + distance at the top
                    if let Some(first_instruction) = code.first() {
                        let addr = first_instruction.addr;
                        let table_start = *self.function_names.first_key_value().map_or(&0xFFFFFFFF, |(addr, _)| addr);
                        if addr >= table_start && addr <= table_start.saturating_add(rom_size) {
                            body.row(12.0, |mut row| {
                                row.col(|_ui| {});
                                row.col(|_ui| {});
                                row.col(|ui| {
                                    ui.label(
                                        egui::RichText::new(self.get_nearest_function_with_distance(addr).as_str())
                                            .family(egui::FontFamily::Monospace)
                                            .size(10.0)
                                            .strong(),
                                    );
                                });
                                row.col(|_ui| {});
                            });
                        }
                    }
                }
                for c in code {
                    let mut text = c.str.to_string();

                    if labels {
                        text = self.label_code(rom_size, &text);
                    }

                    // Display labels for the execution address
                    if labels {
                        if let Some(name) = self.get_name_for_address(c.addr) {
                            body.row(12.0, |mut row| {
                                row.col(|_ui| {});
                                row.col(|_ui| {});
                                row.col(|ui| {
                                    ui.label(
                                        egui::RichText::new(
                                            format!("{}{}:",
                                                    if !self.function_names.contains_key(&c.addr)
                                                    && !self.low_memory.contains_key(&c.addr) { "@" } else { "" },
                                                    name))
                                            .family(egui::FontFamily::Monospace)
                                            .size(10.0)
                                            .strong(),
                                    );
                                });
                                row.col(|_ui| {});
                            });
                        }
                    }

                    if c.is_linea() {
                        // A-line annotation
                        let opcode = ((c.raw[0] as u16) << 8) | (c.raw[1] as u16);
                        if let Some((_, s)) = crate::consts::TRAPS.iter().find(|(i, _)| *i == opcode) {
                            text.push_str(&format!(" ; {}", s));
                        }
                    }

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
                            ui.add(
                                egui::Label::new(
                                egui::RichText::new(format!(":{:08X}", c.addr))
                                    .family(egui::FontFamily::Monospace)
                                    .size(10.0)).sense(egui::Sense::click()),
                            ).context_address(c.addr);
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
                                egui::RichText::new(text)
                                    .family(egui::FontFamily::Monospace)
                                    .size(10.0),
                            );
                        });
                    });
                }
            });
    }

    fn label_code(&self, rom_size: u32, text: &str) -> String {
        let mut modified_text = text.to_string();

        // Replace addresses in disassembly with labels
        let parts: Vec<&str> = text
            .split(|c: char| !c.is_ascii_hexdigit() && c != '$')
            .collect();
        // TODO: prevent replacement of literals and offsets
        for part in parts {
            let clean_part = part.trim_matches(|c: char| !c.is_ascii_hexdigit());
            if clean_part.len() >= 4 && clean_part.len() <= 8 {
                if let Ok(addr) = u32::from_str_radix(clean_part, 16) {
                    if let Some(name) = self.get_name_for_address(addr) {
                        let table_start = *self
                            .function_names
                            .first_key_value()
                            .map_or(&0xFFFFFFFF, |(addr, _)| addr);
                        if addr >= table_start
                            && addr <= table_start.saturating_add(rom_size)
                            && self.label_names.contains_key(&addr)
                        {
                            modified_text = modified_text.replace(part, &format!("@{}", name));
                        }
                        modified_text = modified_text.replace(part, name);
                    }
                }
            }
        }

        modified_text
    }
}
