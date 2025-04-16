use eframe::egui;
use snow_core::cpu_m68k::cpu::HistoryEntry;
use snow_core::cpu_m68k::disassembler::Disassembler;

use crate::consts;

/// Widget to display CPU instruction history
#[derive(Default)]
pub struct InstructionHistoryWidget {
    /// Whether history collection is enabled
    enabled: bool,
    /// Last entry to detect changes
    last: Option<HistoryEntry>,
}

impl InstructionHistoryWidget {
    /// Returns whether history collection is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Helper to create a UI that takes up the size but aligns left
    fn left_sized(
        ui: &mut egui::Ui,
        max_size: impl Into<egui::Vec2> + Clone,
        widget: impl egui::Widget,
    ) -> egui::Response {
        ui.scope(|ui| {
            ui.set_max_size(max_size.clone().into());
            ui.set_min_size(max_size.into());
            ui.add(widget);
        })
        .response
    }

    fn column_status(history: &[HistoryEntry], row_height: f32, row_idx: usize, ui: &mut egui::Ui) {
        Self::left_sized(
            ui,
            [20.0, row_height],
            if row_idx == history.len() - 1 {
                egui::Label::new(
                    egui::RichText::new(egui_material_icons::icons::ICON_PLAY_ARROW)
                        .color(egui::Color32::WHITE)
                        .size(8.0),
                )
            } else {
                egui::Label::new("")
            },
        );
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, history: &[HistoryEntry]) {
        // Header
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.enabled, "Enable history collection")
                .clicked()
            {
                // The caller can check the enabled state to handle enabling/disabling collection
            }
        });

        ui.separator();

        // Column headers
        ui.horizontal(|ui| {
            Self::left_sized(ui, [20.0, 20.0], egui::Label::new(""));
            Self::left_sized(
                ui,
                [60.0, 20.0],
                egui::Label::new(egui::RichText::new("Address").strong()),
            );
            Self::left_sized(
                ui,
                [120.0, 20.0],
                egui::Label::new(egui::RichText::new("Raw").strong()),
            );
            Self::left_sized(
                ui,
                [60.0, 20.0],
                egui::Label::new(egui::RichText::new("Cycles").strong()),
            );
            ui.add(egui::Label::new(
                egui::RichText::new("Instruction").strong(),
            ));
        });

        // Virtual scrolling area
        let row_height = 20.0;
        let total_height = (history.len() as f32) * (row_height + ui.spacing().item_spacing.y);

        let mut scroll_area = egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .max_height(ui.available_height());

        // Scroll to the end on updates
        if self.last.as_ref() != history.last() {
            scroll_area = scroll_area.vertical_scroll_offset(total_height);
            self.last = history.last().cloned();
        }

        scroll_area.show_rows(ui, row_height, history.len(), |ui, row_range| {
            for row_idx in row_range {
                let entry = &history[row_idx];

                match entry {
                    HistoryEntry::Exception { vector, cycles } => {
                        ui.horizontal(|ui| {
                            ui.painter()
                                .rect_filled(ui.max_rect(), 0.0, egui::Color32::DARK_BLUE);
                            Self::column_status(history, row_height, row_idx, ui);
                            Self::left_sized(ui, [120.0, row_height], egui::Label::new(""));
                            Self::left_sized(ui, [60.0, row_height], egui::Label::new(""));
                            // Cycles column
                            Self::left_sized(
                                ui,
                                [60.0, row_height],
                                egui::Label::new(
                                    egui::RichText::new(format!("{}", cycles))
                                        .family(egui::FontFamily::Monospace)
                                        .size(10.0),
                                ),
                            );
                            Self::left_sized(
                                ui,
                                [ui.available_width(), row_height],
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "Exception: {} ({:06X})",
                                        consts::VECTORS
                                            .iter()
                                            .find(|f| f.0 == *vector)
                                            .map(|f| f.1)
                                            .unwrap_or_default(),
                                        vector,
                                    ))
                                    .family(egui::FontFamily::Monospace)
                                    .size(10.0),
                                ),
                            );
                        });
                    }
                    HistoryEntry::Instruction(entry) => {
                        // Disassemble the instruction
                        let mut iter = entry.raw.iter().copied();
                        let mut disasm = Disassembler::from(&mut iter, entry.pc);
                        let disasm_entry = disasm.next();

                        ui.horizontal(|ui| {
                            Self::column_status(history, row_height, row_idx, ui);
                            // Address column
                            Self::left_sized(
                                ui,
                                [60.0, row_height],
                                egui::Label::new(
                                    egui::RichText::new(format!(":{:06X}", entry.pc))
                                        .family(egui::FontFamily::Monospace)
                                        .size(10.0),
                                ),
                            );

                            // Raw bytes column
                            let raw_str = entry.raw.iter().fold(String::new(), |mut output, b| {
                                let _ = std::fmt::Write::write_fmt(
                                    &mut output,
                                    format_args!("{:02X}", b),
                                );
                                output
                            });

                            Self::left_sized(
                                ui,
                                [120.0, row_height],
                                egui::Label::new(
                                    egui::RichText::new(format!("{:<16}", raw_str))
                                        .family(egui::FontFamily::Monospace)
                                        .size(10.0)
                                        .color(egui::Color32::DARK_GRAY),
                                ),
                            );

                            // Cycles column
                            Self::left_sized(
                                ui,
                                [60.0, row_height],
                                egui::Label::new(
                                    egui::RichText::new(format!("{}", entry.cycles))
                                        .family(egui::FontFamily::Monospace)
                                        .size(10.0),
                                ),
                            );

                            // Instruction column
                            let instr_text = if let Some(ref instr) = disasm_entry {
                                egui::RichText::new(&instr.str)
                                    .family(egui::FontFamily::Monospace)
                                    .size(10.0)
                            } else {
                                egui::RichText::new("<invalid>")
                                    .family(egui::FontFamily::Monospace)
                                    .size(10.0)
                            };

                            Self::left_sized(
                                ui,
                                [ui.available_width(), row_height],
                                egui::Label::new(instr_text),
                            );
                        });
                    }
                }
            }
        });
    }
}
