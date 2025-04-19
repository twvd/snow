use crate::consts;
use eframe::egui;
use eframe::egui::Ui;
use snow_core::bus::Address;
use snow_core::cpu_m68k::cpu::{HistoryEntry, HistoryEntryInstruction};
use snow_core::cpu_m68k::disassembler::Disassembler;
use snow_core::tickable::Ticks;

/// Widget to display CPU instruction history
#[derive(Default)]
pub struct InstructionHistoryWidget {
    /// Whether history collection is enabled
    enabled: bool,
    /// Last entry to detect changes
    last: Option<HistoryEntry>,
    /// Show register changes
    regdiff: bool,
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

    /// Helper to create a UI that takes up the size but aligns left, with
    /// a material icon adjusted to fit in line with the monospace font.
    fn left_sized_icon(
        ui: &mut egui::Ui,
        max_size: &(impl Into<egui::Vec2> + Clone),
        icon: &str,
        color: Option<egui::Color32>,
    ) -> egui::Response {
        ui.scope(|ui| {
            ui.set_max_size(max_size.clone().into());
            ui.set_min_size(max_size.clone().into());
            ui.put(
                // Slightly nudge the icon up so it sits nicely in line with the
                // monospace font.
                ui.cursor().translate([0.0, -2.0].into()),
                egui::Label::new(
                    egui::RichText::new(icon)
                        .color(color.unwrap_or(egui::Color32::PLACEHOLDER))
                        .size(12.0),
                ),
            );
        })
        .response
    }

    fn column_status(history: &[HistoryEntry], row_height: f32, row_idx: usize, ui: &mut egui::Ui) {
        // Last instruction indicator
        Self::left_sized_icon(
            ui,
            &[12.0, row_height],
            if row_idx == history.len() - 1 {
                egui_material_icons::icons::ICON_PLAY_ARROW
            } else {
                ""
            },
            Some(egui::Color32::WHITE),
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
            if ui
                .checkbox(&mut self.regdiff, "Show register changes")
                .clicked()
            {}
        });

        ui.separator();

        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = [2.0, 0.0].into();

            // Column headers
            ui.horizontal(|ui| {
                Self::left_sized(ui, [12.0, 20.0], egui::Label::new(""));
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
                    [50.0, 20.0],
                    egui::Label::new(egui::RichText::new("Cycles").strong()),
                );
                Self::left_sized(ui, [10.0, 20.0], egui::Label::new(""));
                Self::left_sized(
                    ui,
                    [
                        if self.regdiff {
                            200.0
                        } else {
                            ui.available_width()
                        },
                        20.0,
                    ],
                    egui::Label::new(egui::RichText::new("Instruction").strong()),
                );
                if self.regdiff {
                    Self::left_sized(
                        ui,
                        [ui.available_width(), 20.0],
                        egui::Label::new(egui::RichText::new("Changes").strong()),
                    );
                }
            });

            // Virtual scrolling area
            let row_height = 16.0;
            let sa_row_height = row_height;
            let total_height =
                (history.len() as f32) * (sa_row_height + ui.spacing().item_spacing.y);

            let mut scroll_area = egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .max_height(ui.available_height());

            // Scroll to the end on updates
            if self.last.as_ref() != history.last() {
                scroll_area = scroll_area.vertical_scroll_offset(total_height);
                self.last = history.last().cloned();
            }

            scroll_area.show_rows(ui, sa_row_height, history.len(), |ui, row_range| {
                for row_idx in row_range {
                    let entry = &history[row_idx];

                    match entry {
                        HistoryEntry::Exception { vector, cycles } => {
                            self.row_exception(history, row_height, row_idx, ui, vector, cycles);
                        }
                        HistoryEntry::Instruction(entry) => {
                            self.row_instruction(history, row_height, row_idx, ui, entry);
                        }
                    }
                }
            });
        });
    }

    fn col_regdiff(&self, row_height: f32, ui: &mut Ui, entry: &HistoryEntryInstruction) {
        if let HistoryEntryInstruction {
            initial_regs: Some(initial),
            final_regs: Some(fin),
            ..
        } = entry
        {
            let diffstr = initial.diff_str(fin);

            Self::left_sized(
                ui,
                [ui.available_width(), row_height],
                egui::Label::new(if diffstr.is_empty() {
                    egui::RichText::new("No changes")
                        .family(egui::FontFamily::Monospace)
                        .italics()
                        .color(egui::Color32::DARK_GRAY)
                        .size(10.0)
                } else {
                    egui::RichText::new(diffstr)
                        .family(egui::FontFamily::Monospace)
                        .color(egui::Color32::DARK_GRAY)
                        .size(10.0)
                }),
            );
        }
    }

    fn row_instruction(
        &self,
        history: &[HistoryEntry],
        row_height: f32,
        row_idx: usize,
        ui: &mut Ui,
        entry: &HistoryEntryInstruction,
    ) {
        // Disassemble the instruction
        let mut iter = entry.raw.iter().copied();
        let mut disasm = Disassembler::from(&mut iter, entry.pc);
        let disasm_entry = disasm.next();

        ui.horizontal(|ui| {
            ui.set_max_height(row_height);

            if row_idx % 2 == 0 {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, ui.style().visuals.faint_bg_color);
            }

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
                let _ = std::fmt::Write::write_fmt(&mut output, format_args!("{:02X}", b));
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
                [50.0, row_height],
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{}{}",
                        entry.cycles,
                        if entry.waitstates { "*" } else { "" }
                    ))
                    .family(egui::FontFamily::Monospace)
                    .size(10.0),
                ),
            );

            // Branch indicator
            if let HistoryEntry::Instruction(HistoryEntryInstruction {
                branch_taken: Some(branch_taken),
                ..
            }) = history[row_idx]
            {
                Self::left_sized_icon(
                    ui,
                    &[10.0, row_height],
                    egui_material_icons::icons::ICON_ALT_ROUTE,
                    Some(if branch_taken {
                        egui::Color32::LIGHT_GREEN
                    } else {
                        egui::Color32::GRAY
                    }),
                );
            } else {
                Self::left_sized_icon(ui, &[10.0, row_height], "", None);
            }
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
                [
                    if self.regdiff {
                        200.0
                    } else {
                        ui.available_width()
                    },
                    row_height,
                ],
                egui::Label::new(instr_text),
            );
            if self.regdiff {
                self.col_regdiff(row_height, ui, entry);
            }
        });
    }

    fn row_exception(
        &self,
        history: &[HistoryEntry],
        row_height: f32,
        row_idx: usize,
        ui: &mut Ui,
        vector: &Address,
        cycles: &Ticks,
    ) {
        ui.horizontal(|ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, egui::Color32::DARK_BLUE);
            Self::column_status(history, row_height, row_idx, ui);
            Self::left_sized(ui, [120.0, row_height], egui::Label::new(""));
            Self::left_sized(ui, [60.0, row_height], egui::Label::new(""));
            // Cycles column
            Self::left_sized(
                ui,
                [50.0, row_height],
                egui::Label::new(
                    egui::RichText::new(format!("{}", cycles))
                        .family(egui::FontFamily::Monospace)
                        .size(10.0),
                ),
            );
            Self::left_sized_icon(
                ui,
                &[10.0, row_height],
                egui_material_icons::icons::ICON_REPORT,
                Some(egui::Color32::LIGHT_GRAY),
            );
            Self::left_sized(
                ui,
                [ui.available_width(), row_height],
                egui::Label::new(
                    egui::RichText::new(format!(
                        "Exception: {} (${:06X})",
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
}
