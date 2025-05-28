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

    /// Try to parse a watchpoint type from a string
    fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "u8" => Some(Self::U8),
            "u16" => Some(Self::U16),
            "u32" => Some(Self::U32),
            s if s.starts_with("string(") && s.ends_with(')') => {
                let len_str = &s[7..s.len() - 1];
                if let Ok(len) = len_str.parse::<usize>() {
                    if len > 0 && len <= 1024 {
                        // Reasonable bounds
                        Some(Self::String(len))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
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

/// Represents which field of a watchpoint is being edited
#[derive(Debug, Clone, PartialEq)]
enum EditingField {
    Description,
    Address,
    Type,
    Value,
}

/// Represents an edited value that needs to be written to memory
#[derive(Debug, Clone)]
pub struct EditedValue {
    pub address: Address,
    pub data: Vec<u8>,
}

/// Represents the editing state
#[derive(Debug, Clone)]
struct EditingState {
    watchpoint_index: usize,
    field: EditingField,
    value: String,
}

/// Widget for managing and displaying watchpoints
pub struct WatchpointsWidget {
    watchpoints: Vec<Watchpoint>,
    address_input: String,
    description_input: String,
    string_length_input: String,
    selected_type: WatchpointType,
    editing: Option<EditingState>,
    edited: Option<EditedValue>,
}

impl Default for WatchpointsWidget {
    fn default() -> Self {
        Self {
            watchpoints: Vec::new(),
            address_input: String::new(),
            description_input: String::new(),
            string_length_input: "16".to_string(),
            selected_type: WatchpointType::U8,
            editing: None,
            edited: None,
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
        if index >= self.watchpoints.len() {
            return;
        }

        self.watchpoints.remove(index);

        // Clear editing state if we're editing the removed watchpoint
        let Some(ref mut editing) = self.editing else {
            return;
        };

        if editing.watchpoint_index == index {
            self.editing = None;
        } else if editing.watchpoint_index > index {
            // Adjust index if editing a watchpoint after the removed one
            editing.watchpoint_index -= 1;
        }
    }

    /// Take the most recently edited value, if any
    pub fn take_edited(&mut self) -> Option<EditedValue> {
        self.edited.take()
    }

    /// Start editing a field
    fn start_editing(&mut self, watchpoint_index: usize, field: EditingField) {
        if watchpoint_index >= self.watchpoints.len() {
            return;
        }

        let watchpoint = &self.watchpoints[watchpoint_index];
        let value = match field {
            EditingField::Description => watchpoint.description.clone(),
            EditingField::Address => format!("{:08X}", watchpoint.address),
            EditingField::Type => watchpoint.data_type.to_string(),
            EditingField::Value => {
                // Extract the hex part from the current value display
                match watchpoint.data_type {
                    WatchpointType::U8 => {
                        // Format: "0x42 (66)" -> extract "42"
                        if let Some(hex_part) = watchpoint.current_value.strip_prefix("0x") {
                            if let Some(space_pos) = hex_part.find(' ') {
                                hex_part[..space_pos].to_string()
                            } else {
                                hex_part.to_string()
                            }
                        } else {
                            "00".to_string()
                        }
                    }
                    WatchpointType::U16 => {
                        if let Some(hex_part) = watchpoint.current_value.strip_prefix("0x") {
                            if let Some(space_pos) = hex_part.find(' ') {
                                hex_part[..space_pos].to_string()
                            } else {
                                hex_part.to_string()
                            }
                        } else {
                            "0000".to_string()
                        }
                    }
                    WatchpointType::U32 => {
                        if let Some(hex_part) = watchpoint.current_value.strip_prefix("0x") {
                            if let Some(space_pos) = hex_part.find(' ') {
                                hex_part[..space_pos].to_string()
                            } else {
                                hex_part.to_string()
                            }
                        } else {
                            "00000000".to_string()
                        }
                    }
                    WatchpointType::String(_) => {
                        // Extract string content from "text" format
                        if let Some(content) = watchpoint.current_value.strip_prefix('"') {
                            if let Some(end_pos) = content.rfind('"') {
                                content[..end_pos].to_string()
                            } else {
                                content.to_string()
                            }
                        } else {
                            String::new()
                        }
                    }
                }
            }
        };

        self.editing = Some(EditingState {
            watchpoint_index,
            field,
            value,
        });
    }

    /// Finish editing and apply changes
    fn finish_editing(&mut self, save: bool) {
        if let Some(editing) = self.editing.take() {
            if save && editing.watchpoint_index < self.watchpoints.len() {
                let watchpoint = &mut self.watchpoints[editing.watchpoint_index];

                match editing.field {
                    EditingField::Description => {
                        watchpoint.description = editing.value;
                    }
                    EditingField::Address => {
                        if let Ok(addr) = Address::from_str_radix(&editing.value, 16) {
                            watchpoint.address = addr;
                        }
                    }
                    EditingField::Type => {
                        if let Some(new_type) = WatchpointType::from_str(&editing.value) {
                            watchpoint.data_type = new_type;
                        }
                    }
                    EditingField::Value => {
                        // Parse the new value and create memory write data
                        let data = match watchpoint.data_type {
                            WatchpointType::U8 => {
                                if let Ok(value) = u8::from_str_radix(&editing.value, 16) {
                                    vec![value]
                                } else {
                                    return; // Invalid input, don't save
                                }
                            }
                            WatchpointType::U16 => {
                                if let Ok(value) = u16::from_str_radix(&editing.value, 16) {
                                    value.to_be_bytes().to_vec()
                                } else {
                                    return; // Invalid input, don't save
                                }
                            }
                            WatchpointType::U32 => {
                                if let Ok(value) = u32::from_str_radix(&editing.value, 16) {
                                    value.to_be_bytes().to_vec()
                                } else {
                                    return; // Invalid input, don't save
                                }
                            }
                            WatchpointType::String(_) => {
                                // Convert string to bytes, null-terminated
                                let mut bytes = editing.value.as_bytes().to_vec();
                                bytes.push(0); // Add null terminator
                                bytes
                            }
                        };

                        // Store the edited value for the main app to process
                        self.edited = Some(EditedValue {
                            address: watchpoint.address,
                            data,
                        });
                    }
                }
            }
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

            // Handle global input for editing
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.finish_editing(false);
            }

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

                            // Address column (editable)
                            row.col(|ui| {
                                if let Some(ref mut editing) = self.editing {
                                    if editing.watchpoint_index == i
                                        && editing.field == EditingField::Address
                                    {
                                        let response = ui.text_edit_singleline(&mut editing.value);

                                        if ui.input(|i| i.key_pressed(egui::Key::Enter))
                                            || response.lost_focus()
                                        {
                                            let valid =
                                                Address::from_str_radix(&editing.value, 16).is_ok();
                                            self.finish_editing(valid);
                                        }
                                        return;
                                    }
                                }

                                let response = ui.add(
                                    egui::Label::new(format!("${:08X}", watchpoint.address))
                                        .sense(egui::Sense::click()),
                                );
                                if response.clicked() {
                                    self.start_editing(i, EditingField::Address);
                                }
                            });

                            // Type column (editable)
                            row.col(|ui| {
                                if let Some(ref mut editing) = self.editing {
                                    if editing.watchpoint_index == i
                                        && editing.field == EditingField::Type
                                    {
                                        let response = ui.text_edit_singleline(&mut editing.value);

                                        if ui.input(|i| i.key_pressed(egui::Key::Enter))
                                            || response.lost_focus()
                                        {
                                            let valid =
                                                WatchpointType::from_str(&editing.value).is_some();
                                            self.finish_editing(valid);
                                        }
                                        return;
                                    }
                                }

                                let response = ui.add(
                                    egui::Label::new(watchpoint.data_type.to_string())
                                        .sense(egui::Sense::click()),
                                );
                                if response.clicked() {
                                    self.start_editing(i, EditingField::Type);
                                }
                            });

                            // Description column (editable)
                            row.col(|ui| {
                                if let Some(ref mut editing) = self.editing {
                                    if editing.watchpoint_index == i
                                        && editing.field == EditingField::Description
                                    {
                                        let response = ui.text_edit_singleline(&mut editing.value);

                                        if ui.input(|i| i.key_pressed(egui::Key::Enter))
                                            || response.lost_focus()
                                        {
                                            self.finish_editing(true); // Description is always valid
                                        }
                                        return;
                                    }
                                }

                                let response = ui.add(
                                    egui::Label::new(&watchpoint.description)
                                        .sense(egui::Sense::click()),
                                );
                                if response.clicked() {
                                    self.start_editing(i, EditingField::Description);
                                }
                            });

                            // Value (editable, highlight if recently changed)
                            row.col(|ui| {
                                if let Some(ref mut editing) = self.editing {
                                    if editing.watchpoint_index == i
                                        && editing.field == EditingField::Value
                                    {
                                        let response = ui.text_edit_singleline(&mut editing.value);

                                        if ui.input(|i| i.key_pressed(egui::Key::Enter))
                                            || response.lost_focus()
                                        {
                                            // Validate input based on data type
                                            let valid = match watchpoint.data_type {
                                                WatchpointType::U8 => {
                                                    u8::from_str_radix(&editing.value, 16).is_ok()
                                                }
                                                WatchpointType::U16 => {
                                                    u16::from_str_radix(&editing.value, 16).is_ok()
                                                }
                                                WatchpointType::U32 => {
                                                    u32::from_str_radix(&editing.value, 16).is_ok()
                                                }
                                                WatchpointType::String(_) => true, // String is always valid
                                            };
                                            self.finish_editing(valid);
                                        }
                                        return;
                                    }
                                }

                                let value_text = egui::RichText::new(&watchpoint.current_value);
                                let value_text = if time_since_change < Duration::from_secs(1) {
                                    value_text.color(egui::Color32::YELLOW)
                                } else {
                                    value_text
                                };

                                let response = ui
                                    .add(egui::Label::new(value_text).sense(egui::Sense::click()));
                                if response.clicked() {
                                    self.start_editing(i, EditingField::Value);
                                }
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
