use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui;
use eframe::egui::ahash::HashMap;
use eframe::egui::Ui;
use egui_file_dialog::FileDialog;
use snow_core::bus::Address;

use crate::uniform::uniform_error;

pub struct MemoryViewerWidget {
    /// The memory data to display
    memory: Vec<u8>,
    /// Address input field for jumping to a specific address
    address_input: String,
    /// Currently editing byte
    editing: Option<(usize, String)>,
    /// Last edited byte
    edited: Option<(Address, u8)>,
    /// Scroll to a row
    scroll_to_row: Option<usize>,
    /// Current top row
    top_row: usize,
    /// String search input box
    stringsearch_input: String,
    /// Hex search input box
    hexsearch_input: String,
    /// Current search highlight
    highlight: Vec<u8>,
    /// Changed addresses for highlighting
    changes: HashMap<Address, Instant>,
    /// File export dialog
    export_dialog: FileDialog,
}

impl Default for MemoryViewerWidget {
    fn default() -> Self {
        Self {
            memory: Vec::new(),
            address_input: String::new(),
            editing: None,
            edited: None,
            scroll_to_row: None,
            top_row: 0,
            stringsearch_input: String::new(),
            hexsearch_input: String::new(),
            highlight: Vec::new(),
            changes: HashMap::default(),
            export_dialog: FileDialog::new()
                .add_save_extension("Binary file", "bin")
                .default_save_extension("Binary file")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
        }
    }
}

impl MemoryViewerWidget {
    /// Update the memory data to display
    pub fn update_memory(&mut self, addr: Address, data: &[u8]) {
        let addr = addr as usize;
        let end = addr + data.len();

        if self.memory.len() < end {
            self.memory.resize(end, 0);
        }

        // Update change highlights
        let expires = Instant::now() + Duration::from_secs(1);
        self.memory[addr..end]
            .iter()
            .zip(data)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .for_each(|(i, _)| {
                self.changes.insert((addr + i) as Address, expires);
            });

        self.memory[addr..end].copy_from_slice(data);
    }

    /// Get a reference to the current memory buffer
    #[allow(dead_code)]
    pub fn get_memory(&self) -> &[u8] {
        &self.memory
    }

    /// Jump to a specific address
    pub fn go_to_address(&mut self, address: u32) {
        self.scroll_to_row = Some((address / 16) as usize);
    }

    /// Export the entire RAM to a binary file
    fn export_ram(&self, filename: &Path) -> Result<()> {
        let mut f = File::create(filename)?;
        f.write_all(&self.memory)?;
        Ok(())
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) {
        // Handle export dialog
        self.export_dialog.update(ui.ctx());
        if let Some(f) = self.export_dialog.take_picked() {
            if let Err(e) = self.export_ram(&f) {
                uniform_error(format!("Error exporting RAM to file: {}", e));
            }
        }

        // Discard expired change highlights
        let now = Instant::now();
        self.changes.retain(|_, v| *v > now);

        // Address navigation controls
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.top_row > 0,
                    egui::Button::new(egui_material_icons::icons::ICON_SKIP_PREVIOUS),
                )
                .clicked()
            {
                self.scroll_to_row = Some(0);
            }
            if ui
                .add_enabled(
                    self.top_row > 0,
                    egui::Button::new(format!(
                        "{} 1000",
                        egui_material_icons::icons::ICON_ARROW_BACK_IOS
                    )),
                )
                .clicked()
            {
                self.scroll_to_row = Some(self.top_row.saturating_sub(0x1000 / 16));
            }
            if ui
                .add_enabled(
                    self.top_row > 0,
                    egui::Button::new(format!(
                        "{} 100",
                        egui_material_icons::icons::ICON_ARROW_BACK_IOS
                    )),
                )
                .clicked()
            {
                self.scroll_to_row = Some(self.top_row.saturating_sub(0x100 / 16));
            }

            // Address select
            ui.text_edit_singleline(&mut self.address_input);
            if ui
                .add_enabled(
                    Address::from_str_radix(&self.address_input, 16).is_ok(),
                    egui::Button::new("Go"),
                )
                .clicked()
            {
                let addr = Address::from_str_radix(&self.address_input, 16).unwrap();
                self.go_to_address(addr & !0x0F);
                self.address_input.clear();
            }

            if ui
                .add_enabled(
                    true,
                    egui::Button::new(format!(
                        "100 {}",
                        egui_material_icons::icons::ICON_ARROW_FORWARD_IOS
                    )),
                )
                .clicked()
            {
                self.scroll_to_row = Some(self.top_row + 0x100 / 16);
            }
            if ui
                .add_enabled(
                    true,
                    egui::Button::new(format!(
                        "1000 {}",
                        egui_material_icons::icons::ICON_ARROW_FORWARD_IOS
                    )),
                )
                .clicked()
            {
                self.scroll_to_row = Some(self.top_row + 0x1000 / 16);
            }

            ui.separator();

            // Export button
            if ui
                .add_enabled(
                    !self.memory.is_empty(),
                    egui::Button::new(egui_material_icons::icons::ICON_SAVE),
                )
                .on_hover_text("Export RAM to binary file...")
                .clicked()
            {
                self.export_dialog.save_file();
            }
        });

        ui.separator();

        // Search features
        ui.horizontal(|ui| {
            ui.label("Search for (hex): ");
            ui.text_edit_singleline(&mut self.hexsearch_input);
            if ui
                .add_enabled(
                    !self.hexsearch_input.is_empty() && hex::decode(&self.hexsearch_input).is_ok(),
                    egui::Button::new("Search next"),
                )
                .clicked()
            {
                self.search(hex::decode(&self.hexsearch_input).unwrap());
            }
        });
        ui.horizontal(|ui| {
            ui.label("String search: ");
            ui.text_edit_singleline(&mut self.stringsearch_input);
            if ui
                .add_enabled(
                    !self.stringsearch_input.is_empty() && self.stringsearch_input.is_ascii(),
                    egui::Button::new("Search next"),
                )
                .clicked()
            {
                self.search(self.stringsearch_input.as_bytes().to_vec());
            }
        });

        ui.separator();

        let available_height = ui.available_height();

        // If there's no memory to display, show a message
        if self.memory.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("No memory data to display");
            });
            return;
        }

        // Calculate number of rows needed
        let row_height = 20.0;
        let rows = self.memory.len().div_ceil(16);

        // Create a scroll area with row-based virtualization
        let mut scroll_area = egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .max_height(available_height);

        // Handle scroll to row request
        if let Some(row) = self.scroll_to_row.take() {
            scroll_area = scroll_area
                .vertical_scroll_offset(row as f32 * (row_height + ui.spacing().item_spacing.y));
        }

        ui.horizontal(|ui| {
            // Address
            ui.add_sized([60.0, row_height], egui::Label::new(""));
            for i in 0..=0x0F {
                let byte_text = format!("{:02X}", i);
                let text = egui::RichText::new(byte_text)
                    .family(egui::FontFamily::Monospace)
                    .size(10.0);
                ui.add(egui::Label::new(text));
                ui.add_space(4.0);
            }
        });

        scroll_area.show_rows(ui, row_height, rows, |ui, row_range| {
            let mut hl_hex = 0;
            let mut hl_ascii = 0;

            self.top_row = row_range.start;
            for row in row_range {
                let row_addr = (row * 16) as Address;
                let row_start = row_addr as usize;
                let row_end = std::cmp::min(row_start + 16, self.memory.len());

                ui.horizontal(|ui| {
                    // Address
                    ui.add_sized(
                        [60.0, row_height],
                        egui::Label::new(
                            egui::RichText::new(format!(":{:08X}", row_addr))
                                .family(egui::FontFamily::Monospace)
                                .size(10.0),
                        ),
                    );
                    // Hex bytes column
                    self.draw_hex(row_start, row_end, ui, &mut hl_hex);
                    // ASCII column
                    self.draw_ascii(row_start, row_end, ui, &mut hl_ascii);
                });
            }
        });
    }

    fn search(&mut self, needle: Vec<u8>) {
        let start_addr = ((self.top_row + 1) * 16) % self.memory.len();

        self.scroll_to_row = self.memory[start_addr..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| (start_addr + p) / 16);
        self.highlight = needle;
    }

    fn draw_hex(&mut self, row_start: usize, row_end: usize, ui: &mut Ui, hl_left: &mut usize) {
        for i in row_start..row_end {
            // Search highlighting
            if !self.highlight.is_empty()
                && (i + self.highlight.len()) < self.memory.len()
                && self.memory[i..(i + self.highlight.len())] == self.highlight
            {
                *hl_left = self.highlight.len();
            }
            let highlighted = *hl_left > 0;
            *hl_left = hl_left.saturating_sub(1);

            // Check if this byte is being edited
            if let Some((edit_offset, ref mut edit_value)) = self.editing {
                if edit_offset == i {
                    // This byte is being edited, show text field
                    let edit_response = ui.add(
                        egui::TextEdit::singleline(edit_value)
                            .desired_width(20.0)
                            .font(egui::TextStyle::Monospace),
                    );

                    // Escape is cancel
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.editing = None;
                    } else if edit_response.lost_focus()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        // Process result of editing
                        if let Ok(value) = u8::from_str_radix(edit_value, 16) {
                            // Valid hex value, update memory
                            if i < self.memory.len() {
                                self.memory[i] = value;
                                self.edited = Some((i as Address, value));
                            }
                        }
                        // Clear editing state
                        self.editing = None;
                    }

                    continue;
                }
            }

            // Regular byte display with click to edit
            let changed_recently = self.changes.contains_key(&(i as Address));
            let byte = self.memory[i];
            let byte_text = format!("{:02X}", byte);
            let mut text = egui::RichText::new(byte_text)
                .family(egui::FontFamily::Monospace)
                .size(10.0);
            if highlighted {
                if changed_recently {
                    text = text
                        .background_color(egui::Color32::YELLOW)
                        .color(egui::Color32::BLACK);
                } else {
                    text = text
                        .background_color(egui::Color32::WHITE)
                        .color(egui::Color32::BLACK);
                }
            } else if changed_recently {
                text = text.color(egui::Color32::YELLOW);
            } else if byte == 0 {
                text = text.color(egui::Color32::DARK_GRAY);
            }
            let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
            ui.add_space(4.0);

            // If clicked, start editing this byte
            if response.clicked() {
                self.editing = Some((i, format!("{:02X}", self.memory[i])));
            }
        }

        // Pad with spaces for incomplete rows
        for _ in row_end..row_start + 16 {
            ui.label(
                egui::RichText::new("  ")
                    .family(egui::FontFamily::Monospace)
                    .size(10.0),
            );
            ui.add_space(4.0);
        }
    }

    fn draw_ascii(&self, row_start: usize, row_end: usize, ui: &mut Ui, hl_left: &mut usize) {
        let oldspacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.x = 0.0;

        for i in row_start..row_end {
            // Search highlighting
            if !self.highlight.is_empty()
                && self.memory[i..(i + self.highlight.len())] == self.highlight
            {
                *hl_left = self.highlight.len();
            }
            let highlighted = *hl_left > 0;
            *hl_left = hl_left.saturating_sub(1);

            let changed_recently = self.changes.contains_key(&(i as Address));

            let byte = self.memory[i];
            let byte_text = if (32..=126).contains(&byte) {
                // Printable ASCII
                byte as char
            } else {
                // Non-printable
                '.'
            };

            let mut text = egui::RichText::new(byte_text)
                .family(egui::FontFamily::Monospace)
                .size(10.0);
            if highlighted {
                if changed_recently {
                    text = text
                        .background_color(egui::Color32::YELLOW)
                        .color(egui::Color32::BLACK);
                } else {
                    text = text
                        .background_color(egui::Color32::WHITE)
                        .color(egui::Color32::BLACK);
                }
            } else if changed_recently {
                text = text.color(egui::Color32::YELLOW);
            } else if byte == 0 {
                text = text.color(egui::Color32::DARK_GRAY);
            }
            ui.label(text);
        }

        ui.spacing_mut().item_spacing = oldspacing;
    }

    pub fn take_edited(&mut self) -> Option<(Address, u8)> {
        self.edited.take()
    }
}
