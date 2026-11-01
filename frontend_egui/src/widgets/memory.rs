use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui::Ui;
use eframe::egui::ahash::HashMap;
use snow_core::bus::Address;
use snow_core::cpu_m68k::bus::InspectView;
use snow_core::emulator::comm::MemoryNotifierRange;

use crate::dialogs::filedialog::SnowFileDialog;
use crate::settings::AppSettings;

/// Bytes per row
const ROW_BYTES: usize = 16;
/// Part of the address space the scroll area covers at a time: scrolling all of it
/// is more than the precision of a scroll position
const WINDOW_BYTES: usize = 8 * 1024 * 1024;
/// How far the window moves as the view gets near one of its ends
const WINDOW_STEP: usize = WINDOW_BYTES / 2;
/// Last start of the window
const WINDOW_LAST: usize = (1 << 32) - WINDOW_BYTES;
/// Rows read around those on screen, for scrolling not to show holes
const ROWS_AROUND: usize = 64;

/// A request to export memory
pub struct MemoryExportRequest {
    pub path: PathBuf,
    pub start: Address,
    pub len: usize,
    pub view: InspectView,
}

pub struct MemoryViewerWidget {
    /// The memory around what is on screen, None for what can't be read
    data: Vec<Option<u8>>,
    /// Address of `data`
    data_addr: usize,
    /// Looks at physical addresses, else logical ones
    physical: bool,
    /// Address of the first row of the scroll area
    window_addr: usize,
    /// Rows of the scroll area on screen
    visible_rows: Range<usize>,
    /// Address input field for jumping to a specific address
    address_input: String,
    /// Currently editing byte, by address
    editing: Option<(usize, String)>,
    /// Last edited byte, and if its address is physical
    edited: Option<(Address, u8, bool)>,
    /// Scroll to a row
    scroll_to_row: Option<usize>,
    /// String search input box
    stringsearch_input: String,
    /// Hex search input box
    hexsearch_input: String,
    /// Current search highlight
    highlight: Vec<u8>,
    /// Search to run: what to look for, from where
    search_request: Option<(Vec<u8>, Address)>,
    /// Changed addresses for highlighting
    changes: HashMap<usize, Instant>,
    /// File export dialog
    export_dialog: SnowFileDialog,
    /// Address to export from
    export_start_input: String,
    /// Bytes to export
    export_len_input: String,
    /// Export to run
    export_request: Option<MemoryExportRequest>,
}

impl Default for MemoryViewerWidget {
    fn default() -> Self {
        Self {
            data: Vec::new(),
            data_addr: 0,
            physical: true,
            window_addr: 0,
            visible_rows: 0..0,
            address_input: String::new(),
            editing: None,
            edited: None,
            scroll_to_row: None,
            stringsearch_input: String::new(),
            hexsearch_input: String::new(),
            highlight: Vec::new(),
            search_request: None,
            changes: HashMap::default(),
            export_dialog: SnowFileDialog::new()
                .add_save_extension("Binary file", "bin")
                .default_save_extension("Binary file")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
            export_start_input: "0".to_string(),
            export_len_input: String::new(),
            export_request: None,
        }
    }
}

impl MemoryViewerWidget {
    /// How the memory is being looked at
    fn view(&self) -> InspectView {
        if self.physical {
            InspectView::Physical
        } else {
            InspectView::Virtual
        }
    }

    /// The range of memory to be told about
    pub fn range(&self) -> MemoryNotifierRange {
        let first = self.visible_rows.start.saturating_sub(ROWS_AROUND);
        let last = self.visible_rows.end + ROWS_AROUND;
        let start = self.window_addr + first * ROW_BYTES;
        let end = (self.window_addr + last * ROW_BYTES).min(1 << 32);
        MemoryNotifierRange {
            addr: start as Address,
            len: (end - start) as u32,
            view: self.view(),
            follow_pc: false,
        }
    }

    /// Update the memory data to display
    pub fn update_memory(&mut self, addr: Address, data: Vec<Option<u8>>) {
        let addr = addr as usize;

        // Update change highlights, of what is on screen in both
        let expires = Instant::now() + Duration::from_secs(1);
        for (i, &byte) in data.iter().enumerate() {
            if self.has_byte(addr + i) && self.byte(addr + i) != byte {
                self.changes.insert(addr + i, expires);
            }
        }

        self.data = data;
        self.data_addr = addr;
    }

    /// Whether the data covers an address
    fn has_byte(&self, addr: usize) -> bool {
        addr >= self.data_addr && addr - self.data_addr < self.data.len()
    }

    /// The byte at an address, if it is known and can be read
    fn byte(&self, addr: usize) -> Option<u8> {
        self.data
            .get(addr.wrapping_sub(self.data_addr))
            .copied()
            .flatten()
    }

    /// Address of the first row on screen
    fn top_addr(&self) -> usize {
        self.window_addr + self.visible_rows.start * ROW_BYTES
    }

    /// Jump to a specific address
    pub fn go_to_address(&mut self, address: u32) {
        let address = address as usize;
        self.window_addr = (address / WINDOW_STEP * WINDOW_STEP).min(WINDOW_LAST);
        self.scroll_to_row = Some((address - self.window_addr) / ROW_BYTES);
    }

    /// Jump to a specific logical address
    pub fn go_to_logical_address(&mut self, address: u32) {
        self.set_physical(false);
        self.go_to_address(address);
    }

    /// Looks at physical addresses, else logical ones
    fn set_physical(&mut self, physical: bool) {
        if self.physical != physical {
            self.physical = physical;
            self.data.clear();
            self.changes.clear();
        }
    }

    /// Moves the window of the scroll area when the view gets near one of its
    /// ends, staying at the same address
    fn follow_window(&mut self) {
        let row_step = WINDOW_STEP / ROW_BYTES;
        let rows = WINDOW_BYTES / ROW_BYTES;
        let top = self.visible_rows.start;
        if top > rows - rows / 4 && self.window_addr < WINDOW_LAST {
            self.window_addr += WINDOW_STEP;
            self.visible_rows = (top - row_step)..(self.visible_rows.end - row_step);
            self.scroll_to_row = Some(top - row_step);
        } else if top < rows / 4 && self.window_addr > 0 {
            self.window_addr -= WINDOW_STEP;
            self.visible_rows = (top + row_step)..(self.visible_rows.end + row_step);
            self.scroll_to_row = Some(top + row_step);
        }
    }

    /// Takes a search to run, what to look for and from where, as what kind of
    /// address
    pub fn take_search_request(&mut self) -> Option<(Vec<u8>, Address, InspectView)> {
        let view = self.view();
        self.search_request
            .take()
            .map(|(needle, start)| (needle, start, view))
    }

    /// Takes an export to run
    pub fn take_export_request(&mut self) -> Option<MemoryExportRequest> {
        self.export_request.take()
    }

    /// Draws the viewer. `ram_size` is how much to export unless told otherwise.
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        settings: &AppSettings,
        ram_size: usize,
    ) {
        // Handle export dialog
        self.export_dialog.update(ui.ctx(), frame);
        if let Some(path) = self.export_dialog.take_picked()
            && let Ok(start) = Address::from_str_radix(&self.export_start_input, 16)
        {
            self.export_request = Some(MemoryExportRequest {
                path,
                start,
                len: usize::from_str_radix(&self.export_len_input, 16).unwrap_or(ram_size),
                view: self.view(),
            });
        }

        // Discard expired change highlights
        let now = Instant::now();
        self.changes.retain(|_, v| *v > now);

        // Address navigation controls
        let top_addr = self.top_addr();
        ui.horizontal(|ui| {
            let mut physical = self.physical;
            if ui
                .checkbox(&mut physical, "Physical")
                .on_hover_text("Look at physical addresses, else logical ones")
                .changed()
            {
                self.set_physical(physical);
            }
            ui.separator();

            if ui
                .add_enabled(
                    top_addr > 0,
                    egui::Button::new(egui_material_icons::icons::ICON_SKIP_PREVIOUS),
                )
                .clicked()
            {
                self.go_to_address(0);
            }
            if ui
                .add_enabled(
                    top_addr > 0,
                    egui::Button::new(format!(
                        "{} 1000",
                        egui_material_icons::icons::ICON_ARROW_BACK_IOS
                    )),
                )
                .clicked()
            {
                self.go_to_address(top_addr.saturating_sub(0x1000) as Address);
            }
            if ui
                .add_enabled(
                    top_addr > 0,
                    egui::Button::new(format!(
                        "{} 100",
                        egui_material_icons::icons::ICON_ARROW_BACK_IOS
                    )),
                )
                .clicked()
            {
                self.go_to_address(top_addr.saturating_sub(0x100) as Address);
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
                self.go_to_address((top_addr + 0x100).min(u32::MAX as usize) as Address);
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
                self.go_to_address((top_addr + 0x1000).min(u32::MAX as usize) as Address);
            }

            ui.separator();

            // Export controls
            ui.label("Export from:");
            ui.add(egui::TextEdit::singleline(&mut self.export_start_input).desired_width(70.0));
            ui.label("bytes:");
            ui.add(
                egui::TextEdit::singleline(&mut self.export_len_input)
                    .desired_width(70.0)
                    .hint_text(format!("{:X}", ram_size)),
            );
            if ui
                .add_enabled(
                    Address::from_str_radix(&self.export_start_input, 16).is_ok()
                        && (self.export_len_input.is_empty()
                            || usize::from_str_radix(&self.export_len_input, 16).is_ok()),
                    egui::Button::new(egui_material_icons::icons::ICON_SAVE),
                )
                .on_hover_text("Export memory to binary file, with zeros for what can't be read...")
                .clicked()
            {
                self.export_dialog.save_file(settings.native_file_dialogs);
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

        let row_height = 20.0;
        let rows = WINDOW_BYTES / ROW_BYTES;

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

            self.visible_rows = row_range.clone();
            for row in row_range {
                let row_start = self.window_addr + row * ROW_BYTES;
                let row_end = row_start + ROW_BYTES;

                ui.horizontal(|ui| {
                    // Address
                    ui.add_sized(
                        [60.0, row_height],
                        egui::Label::new(
                            egui::RichText::new(format!(":{:08X}", row_start))
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
        if self.scroll_to_row.is_none() {
            self.follow_window();
        }
    }

    fn search(&mut self, needle: Vec<u8>) {
        let start = (self.top_addr() + ROW_BYTES).min(u32::MAX as usize) as Address;
        self.search_request = Some((needle.clone(), start));
        self.highlight = needle;
    }

    /// Whether the search highlight starts at an address
    fn highlight_at(&self, addr: usize) -> bool {
        !self.highlight.is_empty()
            && self
                .highlight
                .iter()
                .enumerate()
                .all(|(i, &b)| self.byte(addr + i) == Some(b))
    }

    fn draw_hex(&mut self, row_start: usize, row_end: usize, ui: &mut Ui, hl_left: &mut usize) {
        for i in row_start..row_end {
            // Search highlighting
            if self.highlight_at(i) {
                *hl_left = self.highlight.len();
            }
            let highlighted = *hl_left > 0;
            *hl_left = hl_left.saturating_sub(1);

            // Check if this byte is being edited
            if let Some((edit_offset, ref mut edit_value)) = self.editing
                && edit_offset == i
            {
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
                        if let Some(byte) = self.data.get_mut(i.wrapping_sub(self.data_addr)) {
                            *byte = Some(value);
                        }
                        self.edited = Some((i as Address, value, self.physical));
                    }
                    // Clear editing state
                    self.editing = None;
                }

                continue;
            }

            // Regular byte display with click to edit
            let changed_recently = self.changes.contains_key(&i);
            let Some(byte) = self.byte(i) else {
                // Nothing to read here
                ui.add(egui::Label::new(
                    egui::RichText::new("xx")
                        .family(egui::FontFamily::Monospace)
                        .size(10.0)
                        .color(egui::Color32::DARK_GRAY),
                ));
                ui.add_space(4.0);
                continue;
            };
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
                self.editing = Some((i, format!("{:02X}", byte)));
            }
        }
    }

    fn draw_ascii(&self, row_start: usize, row_end: usize, ui: &mut Ui, hl_left: &mut usize) {
        let oldspacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.x = 0.0;

        for i in row_start..row_end {
            // Search highlighting
            if self.highlight_at(i) {
                *hl_left = self.highlight.len();
            }
            let highlighted = *hl_left > 0;
            *hl_left = hl_left.saturating_sub(1);

            let changed_recently = self.changes.contains_key(&i);

            let Some(byte) = self.byte(i) else {
                ui.label(
                    egui::RichText::new(' ')
                        .family(egui::FontFamily::Monospace)
                        .size(10.0),
                );
                continue;
            };
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

    pub fn take_edited(&mut self) -> Option<(Address, u8, bool)> {
        self.edited.take()
    }
}
