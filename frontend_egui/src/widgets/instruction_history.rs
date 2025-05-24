use crate::consts;
use crate::helpers::{left_sized, left_sized_f, left_sized_icon};
use crate::uniform::UniformMethods;
use eframe::egui;
use eframe::egui::Ui;
use snow_core::bus::Address;
use snow_core::cpu_m68k::cpu::{HistoryEntry, HistoryEntryInstruction};
use snow_core::cpu_m68k::disassembler::Disassembler;
use snow_core::tickable::Ticks;

/// Widget to display CPU instruction history
#[derive(Default)]
pub struct InstructionHistoryWidget {
    /// Last entry to detect changes
    last: Option<HistoryEntry>,
}

impl InstructionHistoryWidget {
    fn column_status(history: &[HistoryEntry], row_height: f32, row_idx: usize, ui: &mut egui::Ui) {
        // Last instruction indicator
        left_sized_icon(
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
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = [2.0, 0.0].into();

            // Column headers
            ui.horizontal(|ui| {
                left_sized(ui, [12.0, 20.0], egui::Label::new(""));
                left_sized(
                    ui,
                    [60.0, 20.0],
                    egui::Label::new(egui::RichText::new("Address").strong()),
                );
                left_sized(
                    ui,
                    [120.0, 20.0],
                    egui::Label::new(egui::RichText::new("Raw").strong()),
                );
                left_sized(
                    ui,
                    [50.0, 20.0],
                    egui::Label::new(egui::RichText::new("Cycles").strong()),
                );
                left_sized(ui, [10.0, 20.0], egui::Label::new(""));
                left_sized(
                    ui,
                    [200.0, 20.0],
                    egui::Label::new(egui::RichText::new("Instruction").strong()),
                );
                left_sized(
                    ui,
                    [50.0, 20.0],
                    egui::Label::new(egui::RichText::new("EA").strong()),
                );
                left_sized(
                    ui,
                    [ui.available_width(), 20.0],
                    egui::Label::new(egui::RichText::new("Changes").strong()),
                );
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

            left_sized(
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
            left_sized_f(ui, [60.0, row_height], |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(":{:08X}", entry.pc))
                            .family(egui::FontFamily::Monospace)
                            .size(10.0),
                    )
                    .sense(egui::Sense::click()),
                )
                .context_address(entry.pc);
            });

            // Raw bytes column
            let raw_str = entry.raw.iter().fold(String::new(), |mut output, b| {
                let _ = std::fmt::Write::write_fmt(&mut output, format_args!("{:02X}", b));
                output
            });

            left_sized(
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
            left_sized(
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
                left_sized_icon(
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
                left_sized_icon(ui, &[10.0, row_height], "", None);
            }
            // Instruction column
            let instr_text = if let Some(ref instr) = disasm_entry {
                let mut text = instr.str.to_string();
                if instr.raw.len() == 2 && instr.raw[0] & 0xF0 == 0xA0 {
                    // A-line annotation
                    let opcode = ((instr.raw[0] as u16) << 8) | (instr.raw[1] as u16);
                    if let Some((_, s)) = crate::consts::TRAPS.iter().find(|(i, _)| *i == opcode) {
                        text.push_str(&format!(" ; {}", s));
                    }
                }
                egui::RichText::new(&text)
                    .family(egui::FontFamily::Monospace)
                    .size(10.0)
            } else {
                egui::RichText::new("<invalid>")
                    .family(egui::FontFamily::Monospace)
                    .size(10.0)
            };
            left_sized(ui, [200.0, row_height], egui::Label::new(instr_text));

            // Effective Address column
            left_sized_f(ui, [50.0, row_height], |ui| {
                let response = ui.add(egui::Label::new(
                    egui::RichText::new(if let Some(ea) = entry.ea {
                        format!("{:08X}", ea)
                    } else {
                        "-".to_string()
                    })
                    .family(egui::FontFamily::Monospace)
                    .size(10.0),
                ));
                if let Some(ea) = entry.ea {
                    response.context_address(ea);
                }
            });

            self.col_regdiff(row_height, ui, entry);
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
            left_sized(ui, [120.0, row_height], egui::Label::new(""));
            left_sized(ui, [60.0, row_height], egui::Label::new(""));
            // Cycles column
            left_sized(
                ui,
                [50.0, row_height],
                egui::Label::new(
                    egui::RichText::new(format!("{}", cycles))
                        .family(egui::FontFamily::Monospace)
                        .size(10.0),
                ),
            );
            left_sized_icon(
                ui,
                &[10.0, row_height],
                egui_material_icons::icons::ICON_REPORT,
                Some(egui::Color32::LIGHT_GRAY),
            );
            left_sized(
                ui,
                [ui.available_width(), row_height],
                egui::Label::new(
                    egui::RichText::new(format!(
                        "Exception: {} (${:08X})",
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
