use eframe::egui;
use std::collections::VecDeque;

/// Display mode for terminal data
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DisplayMode {
    /// Try to display as ASCII text (non-printable shown as dots)
    Text,
    /// Display as hex bytes
    Hex,
    /// Display both hex and ASCII
    Both,
}

/// Widget for displaying a terminal interface with binary input and output
pub struct TerminalWidget {
    /// Buffer for received data
    buffer: Vec<u8>,
    /// Display buffer (formatted for current display mode)
    display_buffer: String,
    /// Input field for transmit data
    input: String,
    /// Queue of transmitted data
    tx_queue: VecDeque<Vec<u8>>,
    /// Maximum buffer size in bytes
    max_buffer_size: usize,
    /// Whether to echo input to the terminal
    local_echo: bool,
    /// Scrolled to bottom flag
    auto_scroll: bool,
    /// How to display the data
    display_mode: DisplayMode,
    /// Input mode (text or hex)
    hex_input: bool,
    /// Display buffer needs refresh
    needs_refresh: bool,
}

impl Default for TerminalWidget {
    fn default() -> Self {
        Self {
            buffer: Vec::with_capacity(8192),
            display_buffer: String::with_capacity(16384),
            input: String::new(),
            tx_queue: VecDeque::new(),
            max_buffer_size: 8192,
            local_echo: true,
            auto_scroll: true,
            display_mode: DisplayMode::Both,
            hex_input: false,
            needs_refresh: true,
        }
    }
}

impl TerminalWidget {
    /// Push received data to the terminal
    pub fn push_rx(&mut self, data: &[u8]) {
        // Append data to the buffer
        self.buffer.extend_from_slice(data);

        // Enforce buffer size limit if needed
        if self.buffer.len() > self.max_buffer_size {
            let excess = self.buffer.len() - self.max_buffer_size;
            self.buffer.drain(0..excess);
        }

        // Mark display buffer for refresh
        self.needs_refresh = true;
    }

    /// Pop next transmitted data from the queue
    pub fn pop_tx(&mut self) -> Option<Vec<u8>> {
        self.tx_queue.pop_front()
    }

    /// Check if there are transmitted data in the queue
    #[allow(dead_code)]
    pub fn has_tx(&self) -> bool {
        !self.tx_queue.is_empty()
    }

    /// Format the raw buffer according to the current display mode
    fn refresh_display_buffer(&mut self) {
        if !self.needs_refresh {
            return;
        }

        self.display_buffer.clear();

        match self.display_mode {
            DisplayMode::Text => {
                // Text mode - convert bytes to printable ASCII
                for byte in &self.buffer {
                    if (32..=126).contains(byte) {
                        // Printable ASCII
                        self.display_buffer.push(*byte as char);
                    } else if *byte == b'\n' || *byte == b'\r' {
                        // Newlines
                        self.display_buffer.push('\n');
                    } else {
                        // Non-printable
                        self.display_buffer.push('.');
                    }
                }
            }
            DisplayMode::Hex => {
                // Hex mode - show as hex bytes
                for (i, byte) in self.buffer.iter().enumerate() {
                    // 16 bytes per line
                    if i > 0 && i % 16 == 0 {
                        self.display_buffer.push('\n');
                    } else if i > 0 {
                        self.display_buffer.push(' ');
                    }

                    self.display_buffer.push_str(&format!("{:02X}", byte));
                }
            }
            DisplayMode::Both => {
                // Both - show as hex + ASCII
                for i in 0..(self.buffer.len() + 15) / 16 {
                    let start = i * 16;
                    let end = (start + 16).min(self.buffer.len());

                    // Address
                    self.display_buffer.push_str(&format!("{:04X}: ", start));

                    // Hex portion
                    for j in start..end {
                        self.display_buffer
                            .push_str(&format!("{:02X} ", self.buffer[j]));
                    }

                    // Padding for incomplete lines
                    for _ in end..(start + 16) {
                        self.display_buffer.push_str("   ");
                    }

                    self.display_buffer.push_str(" | ");

                    // ASCII portion
                    for j in start..end {
                        let byte = self.buffer[j];
                        if (32..=126).contains(&byte) {
                            // Printable ASCII
                            self.display_buffer.push(byte as char);
                        } else {
                            // Non-printable
                            self.display_buffer.push('.');
                        }
                    }

                    self.display_buffer.push('\n');
                }
            }
        }

        self.needs_refresh = false;
    }

    /// Draw the terminal widget
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let available_height = ui.available_height();

        // Refresh display buffer if needed
        self.refresh_display_buffer();

        ui.vertical(|ui| {
            // Terminal output area
            let output_height = available_height - 80.0; // Reserve space for input area and controls

            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .stick_to_bottom(self.auto_scroll)
                .max_height(output_height)
                .show(ui, |ui| {
                    let text = egui::RichText::new(&self.display_buffer)
                        .family(egui::FontFamily::Monospace)
                        .size(14.0);

                    ui.add(egui::Label::new(text).wrap());
                });

            ui.separator();

            // Controls
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.local_echo, "Local echo");
                ui.checkbox(&mut self.auto_scroll, "Auto-scroll");

                ui.separator();

                // Display mode selector
                ui.label("Display:");
                if ui
                    .radio_value(&mut self.display_mode, DisplayMode::Text, "Text")
                    .clicked()
                {
                    self.needs_refresh = true;
                }
                if ui
                    .radio_value(&mut self.display_mode, DisplayMode::Hex, "Hex")
                    .clicked()
                {
                    self.needs_refresh = true;
                }
                if ui
                    .radio_value(&mut self.display_mode, DisplayMode::Both, "Both")
                    .clicked()
                {
                    self.needs_refresh = true;
                }

                ui.separator();

                // Input mode selector
                ui.checkbox(&mut self.hex_input, "Hex input");

                if ui.button("Clear").clicked() {
                    self.clear();
                }
            });

            // Input hint text based on mode
            let hint_text = if self.hex_input {
                "Enter hex bytes (e.g. 01 AF 3D)..."
            } else {
                "Enter text to transmit..."
            };

            let response = ui.add(
                egui::TextEdit::multiline(&mut self.input)
                    .desired_width(ui.available_width())
                    .desired_rows(1)
                    .hint_text(hint_text)
                    .lock_focus(true)
                    .font(egui::TextStyle::Monospace)
                    .return_key(Some(egui::KeyboardShortcut::new(
                        egui::Modifiers::SHIFT,
                        egui::Key::Enter,
                    ))),
            );

            // Process input when Enter is pressed (without Shift)
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.input.is_empty() {
                let data = if self.hex_input {
                    // Parse hex string to bytes
                    self.parse_hex_input()
                } else {
                    // Convert text to bytes
                    self.input.as_bytes().to_vec()
                };

                // Add local echo if enabled
                if self.local_echo && !data.is_empty() {
                    self.push_rx(&data);
                }

                // Queue the input for transmission
                if !data.is_empty() {
                    self.tx_queue.push_back(data);
                }

                // Clear the input field
                self.input.clear();

                // Request focus back
                response.request_focus();
            }
        });
    }

    /// Parse hex input string into bytes
    fn parse_hex_input(&self) -> Vec<u8> {
        self.input
            .split_whitespace()
            .filter_map(|token| u8::from_str_radix(token, 16).ok())
            .collect()
    }

    /// Set the maximum buffer size in bytes
    #[allow(dead_code)]
    pub fn set_max_buffer_size(&mut self, max_size: usize) {
        self.max_buffer_size = max_size;

        // Enforce immediately if needed
        if self.buffer.len() > self.max_buffer_size {
            let excess = self.buffer.len() - self.max_buffer_size;
            self.buffer.drain(0..excess);
            self.needs_refresh = true;
        }
    }

    /// Clear the terminal buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.display_buffer.clear();
    }

    /// Set the display mode
    #[allow(dead_code)]
    pub fn set_display_mode(&mut self, mode: DisplayMode) {
        self.display_mode = mode;
        self.needs_refresh = true;
    }
}
