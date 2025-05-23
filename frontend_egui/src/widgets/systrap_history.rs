use eframe::egui;
use eframe::egui::Ui;
use snow_core::cpu_m68k::cpu::SystrapHistoryEntry;

/// Widget to display system trap history
#[derive(Default)]
pub struct SystrapHistoryWidget {
    /// Last entry to detect changes
    last: Option<SystrapHistoryEntry>,
}

impl SystrapHistoryWidget {
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

    pub fn draw(&mut self, ui: &mut egui::Ui, history: &[SystrapHistoryEntry]) {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = [2.0, 0.0].into();

            // Column headers
            ui.horizontal(|ui| {
                Self::left_sized(ui, [12.0, 20.0], egui::Label::new(""));
                Self::left_sized(
                    ui,
                    [60.0, 20.0],
                    egui::Label::new(egui::RichText::new("PC").strong()),
                );

                Self::left_sized(
                    ui,
                    [40.0, 20.0],
                    egui::Label::new(egui::RichText::new("Raw").strong()),
                );
                Self::left_sized(
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

            if row_idx % 2 == 0 {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, ui.style().visuals.faint_bg_color);
            }

            Self::left_sized_icon(
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
            Self::left_sized(
                ui,
                [60.0, row_height],
                egui::Label::new(
                    egui::RichText::new(format!(":{:08X}", entry.pc))
                        .family(egui::FontFamily::Monospace)
                        .size(10.0),
                ),
            );

            // Raw column
            Self::left_sized(
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
            Self::left_sized(
                ui,
                [200.0, row_height],
                egui::Label::new(
                    egui::RichText::new(
                        crate::consts::TRAPS
                            .iter()
                            .find(|(i, _)| *i == entry.trap)
                            .map(|(_, s)| *s)
                            .unwrap_or("<unknown>"),
                    )
                    .family(egui::FontFamily::Monospace)
                    .size(10.0),
                ),
            )
        });
    }
}
