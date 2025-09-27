use eframe::egui;
use eframe::egui::Ui;
use snow_core::cpu_m68k::cpu::SystrapHistoryEntry;

use crate::helpers::{left_sized, left_sized_f, left_sized_icon};
use crate::uniform::UniformMethods;

/// Widget to display system trap history
#[derive(Default)]
pub struct SystrapHistoryWidget {
    /// Last entry to detect changes
    last: Option<SystrapHistoryEntry>,
}

impl SystrapHistoryWidget {
    pub fn draw(&mut self, ui: &mut egui::Ui, history: &[SystrapHistoryEntry]) {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = [2.0, 0.0].into();

            // Column headers
            ui.horizontal(|ui| {
                left_sized(ui, [12.0, 20.0], egui::Label::new(""));
                left_sized(
                    ui,
                    [60.0, 20.0],
                    egui::Label::new(egui::RichText::new("PC").strong()),
                );

                left_sized(
                    ui,
                    [40.0, 20.0],
                    egui::Label::new(egui::RichText::new("Raw").strong()),
                );
                left_sized(
                    ui,
                    [200.0, 20.0],
                    egui::Label::new(egui::RichText::new("System trap").strong()),
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

                    self.row(row_height, row_idx, history.len(), ui, entry);
                }
            });
        });
    }

    fn row(
        &self,
        row_height: f32,
        row_idx: usize,
        history_len: usize,
        ui: &mut Ui,
        entry: &SystrapHistoryEntry,
    ) {
        ui.horizontal(|ui| {
            ui.set_max_height(row_height);

            if row_idx.is_multiple_of(2) {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, ui.style().visuals.faint_bg_color);
            }

            left_sized_icon(
                ui,
                &[12.0, row_height],
                if row_idx == history_len - 1 {
                    egui_material_icons::icons::ICON_PLAY_ARROW
                } else {
                    ""
                },
                Some(egui::Color32::WHITE),
            );

            // PC column
            left_sized_f(ui, [60.0, row_height], |ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new(format!(":{:08X}", entry.pc))
                        .family(egui::FontFamily::Monospace)
                        .size(10.0),
                ))
                .context_address(entry.pc);
            });

            // Raw column
            left_sized(
                ui,
                [40.0, row_height],
                egui::Label::new(
                    egui::RichText::new(format!("{:04X}", entry.trap))
                        .family(egui::FontFamily::Monospace)
                        .size(10.0)
                        .color(egui::Color32::DARK_GRAY),
                ),
            );

            // System trap
            let cleaned_trap = if entry.trap & (1 << 11) != 0 {
                // OS trap
                // Mask Flags and return/save A0 bit
                entry.trap & 0b1111_1000_1111_1111
            } else {
                // Toolbox (ROM) trap
                // Mask auto-pop bit
                entry.trap & 0b1111_1011_1111_1111
            };
            left_sized_f(ui, [200.0, row_height], |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            crate::consts::TRAPS
                                .iter()
                                .find(|(i, _)| *i == cleaned_trap)
                                .map(|(_, s)| *s)
                                .unwrap_or("<unknown>"),
                        )
                        .family(egui::FontFamily::Monospace)
                        .size(10.0),
                    )
                    .sense(egui::Sense::click()),
                )
                .context_linea(entry.trap);
            });
        });
    }
}
