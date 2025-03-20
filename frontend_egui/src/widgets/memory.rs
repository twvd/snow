use eframe::egui;
use egui_extras::{Column, TableBuilder};
use snow_core::bus::Address;

#[derive(Default)]
pub struct MemoryViewerWidget {
    /// The memory data to display
    memory: Vec<u8>,
    /// The base address to display from
    start_address: u32,
    /// Address input field for jumping to a specific address
    address_input: String,
    /// Currently editing byte
    editing: Option<(usize, String)>,
    /// Last edited byte
    edited: Option<(Address, u8)>,
}

impl MemoryViewerWidget {
    /// Update the memory data to display
    pub fn update_memory(&mut self, memory: Vec<u8>) {
        self.memory = memory;
        self.start_address = std::cmp::min(self.start_address, (self.memory.len() - 1) as u32);
    }

    /// Get a reference to the current memory buffer
    #[allow(dead_code)]
    pub fn get_memory(&self) -> &[u8] {
        &self.memory
    }

    /// Get a mutable reference to the current memory buffer
    #[allow(dead_code)]
    pub fn get_memory_mut(&mut self) -> &mut Vec<u8> {
        &mut self.memory
    }

    /// Jump to a specific address
    pub fn go_to_address(&mut self, address: u32) {
        self.start_address = address;
        self.start_address = std::cmp::min(self.start_address, (self.memory.len() - 1) as u32);
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) {
        // Address select
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.start_address > 0,
                    egui::Button::new(format!(
                        "{} 1000",
                        egui_material_icons::icons::ICON_ARROW_BACK_IOS
                    )),
                )
                .clicked()
            {
                self.start_address = self.start_address.saturating_sub(0x1000);
            }
            if ui
                .add_enabled(
                    self.start_address > 0,
                    egui::Button::new(format!(
                        "{} 100",
                        egui_material_icons::icons::ICON_ARROW_BACK_IOS
                    )),
                )
                .clicked()
            {
                self.start_address = self.start_address.saturating_sub(0x100);
            }
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
                    self.start_address != self.memory.len() as u32,
                    egui::Button::new(format!(
                        "100 {}",
                        egui_material_icons::icons::ICON_ARROW_FORWARD_IOS
                    )),
                )
                .clicked()
            {
                self.start_address =
                    std::cmp::min(self.memory.len() as u32, self.start_address + 0x100);
            }
            if ui
                .add_enabled(
                    self.start_address != self.memory.len() as u32,
                    egui::Button::new(format!(
                        "1000 {}",
                        egui_material_icons::icons::ICON_ARROW_FORWARD_IOS
                    )),
                )
                .clicked()
            {
                self.start_address =
                    std::cmp::min(self.memory.len() as u32, self.start_address + 0x1000);
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
        //let rows = (self.memory.len() + 15) / 16;
        let visible_rows = (available_height / 20.0).ceil() as usize;

        TableBuilder::new(ui)
            .max_scroll_height(available_height)
            .auto_shrink(false)
            .column(Column::exact(60.0)) // Address column
            .column(Column::exact(430.0)) // Hex bytes column
            .column(Column::remainder()) // ASCII column
            .striped(true)
            .body(|mut body| {
                for row in 0..visible_rows {
                    let row_addr = self.start_address + (row * 16) as u32;
                    let row_start = row_addr as usize;
                    let row_end = std::cmp::min(row_start + 16, self.memory.len());

                    body.row(20.0, |mut row| {
                        // Address column
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{:06X}", row_addr))
                                        .family(egui::FontFamily::Monospace)
                                        .size(10.0),
                                )
                            });
                        });

                        // Hex bytes column
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                for i in row_start..row_end {
                                    // Check if this byte is being edited
                                    if let Some((edit_offset, ref mut edit_value)) = self.editing {
                                        if edit_offset == i {
                                            // This byte is being edited, show text field
                                            let edit_response = ui.add(
                                                egui::TextEdit::singleline(edit_value)
                                                    .desired_width(20.0)
                                                    .font(egui::TextStyle::Monospace),
                                            );

                                            // Process result of editing
                                            if edit_response.lost_focus() {
                                                if let Ok(value) =
                                                    u8::from_str_radix(edit_value, 16)
                                                {
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
                                    let byte_text = format!("{:02X}", self.memory[i]);
                                    let response = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(byte_text)
                                                .family(egui::FontFamily::Monospace)
                                                .size(10.0),
                                        )
                                        .sense(egui::Sense::click()),
                                    );
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
                            });
                        });

                        // ASCII column
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let mut ascii_str = String::new();
                                for i in row_start..row_end {
                                    let byte = self.memory[i];
                                    if (32..=126).contains(&byte) {
                                        // Printable ASCII
                                        ascii_str.push(byte as char);
                                    } else {
                                        // Non-printable
                                        ascii_str.push('.');
                                    }
                                }
                                ui.label(
                                    egui::RichText::new(ascii_str)
                                        .family(egui::FontFamily::Monospace)
                                        .size(10.0),
                                );
                            });
                        });
                    });
                }
            });
    }

    pub fn take_edited(&mut self) -> Option<(Address, u8)> {
        self.edited.take()
    }
}
