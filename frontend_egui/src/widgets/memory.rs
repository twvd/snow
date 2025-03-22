use eframe::egui;
use eframe::egui::Ui;
use snow_core::bus::Address;

#[derive(Default)]
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
}

impl MemoryViewerWidget {
    /// Update the memory data to display
    pub fn update_memory(&mut self, addr: Address, data: &[u8]) {
        let sz = addr as usize + data.len();
        if self.memory.len() < sz {
            self.memory.resize(sz, 0);
        }
        self.memory[(addr as usize)..sz].copy_from_slice(data);
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
        self.scroll_to_row = Some((address / 16) as usize);
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) {
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
        });

        ui.separator();

        // String search
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
                let start_addr = ((self.top_row + 1) * 16) % self.memory.len();
                let needle = self.stringsearch_input.as_bytes();

                self.scroll_to_row = self.memory[start_addr..]
                    .windows(needle.len())
                    .position(|w| w == needle)
                    .map(|p| (start_addr + p) / 16);
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
        let rows = (self.memory.len() + 15) / 16;

        // Create a scroll area with row-based virtualization
        let mut scroll_area = egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .max_height(available_height);

        // Handle scroll to row request
        if let Some(row) = self.scroll_to_row.take() {
            scroll_area = scroll_area
                .vertical_scroll_offset(row as f32 * (row_height + ui.spacing().item_spacing.y));
        }

        scroll_area.show_rows(ui, row_height, rows, |ui, row_range| {
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
                            egui::RichText::new(format!(":{:06X}", row_addr))
                                .family(egui::FontFamily::Monospace)
                                .size(10.0),
                        ),
                    );
                    // Hex bytes column
                    self.draw_hex(row_start, row_end, ui);
                    // ASCII column
                    self.draw_ascii(row_start, row_end, ui);
                });
            }
        });
    }

    fn draw_hex(&mut self, row_start: usize, row_end: usize, ui: &mut Ui) {
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
    }

    fn draw_ascii(&self, row_start: usize, row_end: usize, ui: &mut Ui) {
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
    }

    pub fn take_edited(&mut self) -> Option<(Address, u8)> {
        self.edited.take()
    }
}
