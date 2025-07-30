use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::consts;
use crate::helpers::{left_sized, left_sized_f, left_sized_icon};
use crate::uniform::{uniform_error, UniformMethods};

use anyhow::Result;
use eframe::egui;
use eframe::egui::Ui;
use egui_file_dialog::FileDialog;
use snow_core::bus::Address;
use snow_core::cpu_m68k::cpu::{HistoryEntry, HistoryEntryInstruction};
use snow_core::cpu_m68k::disassembler::{Disassembler, DisassemblyEntry};
use snow_core::tickable::Ticks;
use snow_core::types::Long;

/// Widget to display CPU instruction history
pub struct InstructionHistoryWidget {
    /// Last entry to detect changes
    last: Option<HistoryEntry>,
    /// File export dialog
    export_dialog: FileDialog,
}

impl Default for InstructionHistoryWidget {
    fn default() -> Self {
        Self {
            last: Default::default(),
            export_dialog: FileDialog::new()
                .add_save_extension("Pipe-separated text file", "txt")
                .default_save_extension("Pipe-separated text file")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
        }
    }
}

impl InstructionHistoryWidget {
    fn export_file(&self, history: &[HistoryEntry], filename: &Path) -> Result<()> {
        let mut f = File::create(filename)?;
        writeln!(
            f,
            "PC|Raw|Cycles|Instruction|D0|D1|D2|D3|D4|D5|D6|D7|A0|A1|A2|A3|A4|A5|A6|A7|SR"
        )?;
        for entry in history {
            match entry {
                HistoryEntry::Instruction(HistoryEntryInstruction {
                    pc,
                    raw,
                    cycles,
                    initial_regs,
                    ..
                }) => {
                    let r = initial_regs.clone().unwrap_or_default();
                    writeln!(
                        f,
                        "{:08X}|{}|{}|{}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:08X}|{:04X}",
                        pc,
                        Self::format_raw(raw),
                        cycles,
                        Self::disassemble(raw, *pc).map(|e| e.str).unwrap_or_else(|| "<invalid>".to_string()),
                        r.read_d::<Long>(0),
                        r.read_d::<Long>(1),
                        r.read_d::<Long>(2),
                        r.read_d::<Long>(3),
                        r.read_d::<Long>(4),
                        r.read_d::<Long>(5),
                        r.read_d::<Long>(6),
                        r.read_d::<Long>(7),
                        r.read_a::<Long>(0),
                        r.read_a::<Long>(1),
                        r.read_a::<Long>(2),
                        r.read_a::<Long>(3),
                        r.read_a::<Long>(4),
                        r.read_a::<Long>(5),
                        r.read_a::<Long>(6),
                        r.read_a::<Long>(7),
                        r.sr.0,
                    )?;
                }
                HistoryEntry::Exception { vector, .. } => {
                    writeln!(f, "--- {}", self.text_exception(*vector))?;
                }
                HistoryEntry::Pagefault { address, write } => {
                    writeln!(f, "--- {}", self.text_pagefault(*address, *write))?;
                }
            }
        }
        Ok(())
    }

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
        self.export_dialog.update(ui.ctx());
        if let Some(f) = self.export_dialog.take_picked() {
            if let Err(e) = self.export_file(history, &f) {
                uniform_error(format!("Error exporting to file: {}", e));
            }
        }
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = [2.0, 0.0].into();

            ui.horizontal(|ui| {
                ui.style_mut().text_styles.insert(
                    egui::TextStyle::Button,
                    egui::FontId::new(24.0, eframe::epaint::FontFamily::Proportional),
                );

                if ui
                    .add(egui::Button::new(egui_material_icons::icons::ICON_SAVE))
                    .on_hover_text("Export to file...")
                    .clicked()
                {
                    self.export_dialog.save_file();
                }
            });
            ui.separator();

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
                    [130.0, 20.0],
                    egui::Label::new(egui::RichText::new("Raw").strong()),
                );
                left_sized(
                    ui,
                    [40.0, 20.0],
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
                        HistoryEntry::Pagefault { address, write } => {
                            self.row_pagefault(history, row_height, row_idx, ui, *address, *write);
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
        let disasm_entry = Self::disassemble(&entry.raw, entry.pc);

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
            left_sized(
                ui,
                [120.0, row_height],
                egui::Label::new(
                    egui::RichText::new(format!("{:<16}", Self::format_raw(&entry.raw)))
                        .family(egui::FontFamily::Monospace)
                        .size(10.0)
                        .color(egui::Color32::DARK_GRAY),
                ),
            );

            // Cache/waitstate status
            left_sized_icon(
                ui,
                &[10.0, row_height],
                if entry
                    .initial_regs
                    .as_ref()
                    .map(|rf| rf.cacr.e())
                    .unwrap_or(false)
                {
                    egui_material_icons::icons::ICON_SPEED
                } else if entry.waitstates {
                    egui_material_icons::icons::ICON_HOURGLASS_TOP
                } else {
                    ""
                },
                if entry
                    .initial_regs
                    .as_ref()
                    .map(|rf| rf.cacr.e())
                    .unwrap_or(false)
                {
                    Some(match (entry.icache_hit, entry.icache_miss) {
                        (true, false) => egui::Color32::LIGHT_GREEN,
                        (true, true) => egui::Color32::ORANGE,
                        (false, true) | (false, false) => egui::Color32::RED,
                    })
                } else {
                    None
                },
            );
            // Cycles column
            left_sized(
                ui,
                [40.0, row_height],
                egui::Label::new(
                    egui::RichText::new(format!("{}", entry.cycles))
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
            let mut linea = None;
            let instr_text = if let Some(ref instr) = disasm_entry {
                let mut text = instr.str.to_string();
                if instr.is_linea() {
                    // A-line annotation
                    let opcode = instr.opcode();
                    linea = Some(opcode);
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
            left_sized_f(ui, [200.0, row_height], |ui| {
                let response = ui.add(egui::Label::new(instr_text).sense(egui::Sense::click()));
                if let Some(linea) = linea {
                    response.context_linea(linea);
                }
            });

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

    fn text_exception(&self, vector: Address) -> String {
        format!(
            "Exception: {} (${:08X})",
            consts::VECTORS
                .iter()
                .find(|f| f.0 == vector)
                .map(|f| f.1)
                .unwrap_or_default(),
            vector,
        )
    }
    fn text_pagefault(&self, addr: Address, write: bool) -> String {
        format!(
            "MMU Page fault: {:08X} ({})",
            addr,
            if !write { "read" } else { "write" }
        )
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
                    egui::RichText::new(self.text_exception(*vector))
                        .family(egui::FontFamily::Monospace)
                        .size(10.0),
                ),
            );
        });
    }

    fn row_pagefault(
        &self,
        history: &[HistoryEntry],
        row_height: f32,
        row_idx: usize,
        ui: &mut Ui,
        addr: Address,
        write: bool,
    ) {
        ui.horizontal(|ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, egui::Color32::DARK_BLUE);
            Self::column_status(history, row_height, row_idx, ui);
            left_sized(ui, [120.0, row_height], egui::Label::new(""));
            left_sized(ui, [60.0, row_height], egui::Label::new(""));
            // Cycles column
            left_sized(ui, [50.0, row_height], egui::Label::new(""));
            left_sized_icon(
                ui,
                &[10.0, row_height],
                egui_material_icons::icons::ICON_SHIELD_QUESTION,
                Some(egui::Color32::LIGHT_GRAY),
            );
            left_sized(
                ui,
                [ui.available_width(), row_height],
                egui::Label::new(
                    egui::RichText::new(self.text_pagefault(addr, write))
                        .family(egui::FontFamily::Monospace)
                        .size(10.0),
                ),
            );
        });
    }

    fn format_raw(raw: &[u8]) -> String {
        raw.iter().fold(String::new(), |mut output, b| {
            let _ = std::fmt::Write::write_fmt(&mut output, format_args!("{:02X}", b));
            output
        })
    }

    fn disassemble(raw: &[u8], pc: Address) -> Option<DisassemblyEntry> {
        let mut iter = raw.iter().copied();
        let mut disasm = Disassembler::from(&mut iter, pc);
        disasm.next()
    }
}
