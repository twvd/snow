use eframe::egui;
use snow_core::{bus::Address, tickable::Ticks};
use std::time::{Duration, Instant};

/// Represents the data type of a watchpoint
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchpointType {
    U8,
    U16,
    U32,
    String(usize), // String with length
}

impl WatchpointType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::String(_) => "string",
        }
    }

    fn size_bytes(&self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16 => 2,
            Self::U32 => 4,
            Self::String(len) => *len,
        }
    }
}

impl std::fmt::Display for WatchpointType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String(len) => write!(f, "string({})", len),
            _ => write!(f, "{}", self.as_str()),
        }
    }
}

/// Represents a single watchpoint
#[derive(Debug, Clone)]
pub struct Watchpoint {
    /// Memory address to watch
    address: Address,
    /// Type of data to watch
    data_type: WatchpointType,
    /// User description
    description: String,
    /// Current value as string representation
    current_value: String,
    /// Previous value as string representation
    previous_value: String,
    /// CPU cycles since last change
    last_change_cycles: Ticks,
    /// Time of last change
    last_change_time: Instant,
}

impl Watchpoint {
    pub fn new(address: Address, data_type: WatchpointType, description: String) -> Self {
        Self {
            address,
            data_type,
            description,
            current_value: "Unknown".to_string(),
            previous_value: "Unknown".to_string(),
            last_change_cycles: 0,
            last_change_time: Instant::now(),
        }
    }

    /// Update the watchpoint with new memory data
    pub fn update(&mut self, memory: &[u8], current_cycles: Ticks) -> bool {
        let size = self.data_type.size_bytes();
        if (self.address as usize + size) > memory.len() {
            return false;
        }

        let new_value = match self.data_type {
            WatchpointType::U8 => {
                let value = memory[self.address as usize];
                format!("0x{:02X} ({})", value, value)
            }
            WatchpointType::U16 => {
                let value = u16::from_be_bytes([
                    memory[self.address as usize],
                    memory[self.address as usize + 1],
                ]);
                format!("0x{:04X} ({})", value, value)
            }
            WatchpointType::U32 => {
                let value = u32::from_be_bytes([
                    memory[self.address as usize],
                    memory[self.address as usize + 1],
                    memory[self.address as usize + 2],
                    memory[self.address as usize + 3],
                ]);
                format!("0x{:08X} ({})", value, value)
            }
            WatchpointType::String(len) => {
                let end = std::cmp::min(self.address as usize + len, memory.len());
                let bytes = &memory[self.address as usize..end];
                let mut string_value = String::new();
                for &byte in bytes {
                    if byte == 0 {
                        break;
                    }
                    if byte.is_ascii() && !byte.is_ascii_control() {
                        string_value.push(byte as char);
                    } else {
                        string_value.push('.');
                    }
                }
                format!("\"{}\"", string_value)
            }
        };

        if new_value != self.current_value {
            self.previous_value = self.current_value.clone();
            self.current_value = new_value;
            self.last_change_cycles = current_cycles;
            self.last_change_time = Instant::now();
            true
        } else {
            false
        }
    }
}

/// Widget for managing and displaying watchpoints
pub struct WatchpointsWidget {
    watchpoints: Vec<Watchpoint>,
    address_input: String,
    description_input: String,
    string_length_input: String,
    selected_type: WatchpointType,
}

impl Default for WatchpointsWidget {
    fn default() -> Self {
        Self {
            watchpoints: Vec::new(),
            address_input: String::new(),
            description_input: String::new(),
            string_length_input: "16".to_string(),
            selected_type: WatchpointType::U8,
        }
    }
}

impl WatchpointsWidget {
    /// Add a new watchpoint
    pub fn add_watchpoint(
        &mut self,
        address: Address,
        data_type: WatchpointType,
        description: String,
    ) {
        let watchpoint = Watchpoint::new(address, data_type, description);
        self.watchpoints.push(watchpoint);
    }

    /// Update watchpoints with new memory data
    pub fn update_watchpoints(&mut self, memory: &[u8], current_cycles: Ticks) {
        for watchpoint in &mut self.watchpoints {
            watchpoint.update(memory, current_cycles);
        }
    }

    /// Remove a watchpoint at the given index
    pub fn remove_watchpoint(&mut self, index: usize) {
        if index < self.watchpoints.len() {
            self.watchpoints.remove(index);
        }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, memory: &[u8], current_cycles: Ticks) {
        // Update all watchpoints with current memory data
        self.update_watchpoints(memory, current_cycles);

        ui.vertical(|ui| {
            // Add new watchpoint controls
            ui.collapsing("Add Watchpoint", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Address (hex):");
                    ui.text_edit_singleline(&mut self.address_input);
                });

                ui.horizontal(|ui| {
                    ui.label("Description:");
                    ui.text_edit_singleline(&mut self.description_input);
                });

                ui.horizontal(|ui| {
                    ui.label("Type:");
                    ui.radio_value(&mut self.selected_type, WatchpointType::U8, "u8");
                    ui.radio_value(&mut self.selected_type, WatchpointType::U16, "u16");
                    ui.radio_value(&mut self.selected_type, WatchpointType::U32, "u32");

                    if ui
                        .radio_value(
                            &mut self.selected_type,
                            WatchpointType::String(self.string_length_input.parse().unwrap_or(16)),
                            "string",
                        )
                        .clicked()
                    {
                        self.selected_type =
                            WatchpointType::String(self.string_length_input.parse().unwrap_or(16));
                    }

                    if matches!(self.selected_type, WatchpointType::String(_)) {
                        ui.label("Length:");
                        let response = ui.text_edit_singleline(&mut self.string_length_input);
                        if response.changed() {
                            if let Ok(len) = self.string_length_input.parse() {
                                self.selected_type = WatchpointType::String(len);
                            }
                        }
                    }
                });

                let valid_address = Address::from_str_radix(&self.address_input, 16).is_ok();
                let valid_strlen = !matches!(self.selected_type, WatchpointType::String(_))
                    || self.string_length_input.parse::<usize>().is_ok();
                if ui
                    .add_enabled(
                        valid_address && valid_strlen,
                        egui::Button::new("Add watchpoint"),
                    )
                    .clicked()
                {
                    if let Ok(address) = Address::from_str_radix(&self.address_input, 16) {
                        self.add_watchpoint(
                            address,
                            self.selected_type,
                            self.description_input.clone(),
                        );
                        self.address_input.clear();
                        self.description_input.clear();
                    }
                }
            });

            ui.separator();

            // Use a table like in the Breakpoints widget
            use egui_extras::{Column, TableBuilder};

            let available_height = ui.available_height();

            TableBuilder::new(ui)
                .max_scroll_height(available_height)
                .auto_shrink(false)
                .column(Column::exact(24.0)) // Delete button
                .column(Column::exact(80.0)) // Address
                .column(Column::exact(80.0)) // Type
                .column(Column::exact(180.0)) // Description
                .column(Column::exact(200.0)) // Value
                .column(Column::remainder()) // Last Change
                .striped(true)
                .header(24.0, |mut header| {
                    header.col(|_| {}); // Empty header for delete button
                    header.col(|ui| {
                        ui.label(egui::RichText::new("Address").strong());
                    });
                    header.col(|ui| {
                        ui.label(egui::RichText::new("Type").strong());
                    });
                    header.col(|ui| {
                        ui.label(egui::RichText::new("Description").strong());
                    });
                    header.col(|ui| {
                        ui.label(egui::RichText::new("Value").strong());
                    });
                    header.col(|ui| {
                        ui.label(egui::RichText::new("Cycles since change").strong());
                    });
                })
                .body(|mut body| {
                    for (i, watchpoint) in self.watchpoints.clone().iter().enumerate() {
                        let now = Instant::now();
                        let time_since_change = now.duration_since(watchpoint.last_change_time);
                        let cycles_since_change = current_cycles - watchpoint.last_change_cycles;

                        body.row(24.0, |mut row| {
                            // Delete button
                            row.col(|ui| {
                                if ui.button(egui_material_icons::icons::ICON_DELETE).clicked() {
                                    self.remove_watchpoint(i);
                                }
                            });

                            // Address
                            row.col(|ui| {
                                ui.label(format!("${:06X}", watchpoint.address));
                            });

                            // Type
                            row.col(|ui| {
                                ui.label(watchpoint.data_type.to_string());
                            });

                            // Description
                            row.col(|ui| {
                                ui.label(&watchpoint.description);
                            });

                            // Value (highlight if recently changed)
                            row.col(|ui| {
                                let value_text = egui::RichText::new(&watchpoint.current_value);
                                let value_text = if time_since_change < Duration::from_secs(1) {
                                    value_text.color(egui::Color32::YELLOW)
                                } else {
                                    value_text
                                };
                                ui.label(value_text);
                            });

                            // Last change cycles
                            row.col(|ui| {
                                ui.label(format!("{}", cycles_since_change));
                            });
                        });
                    }
                });
        });
    }
}
