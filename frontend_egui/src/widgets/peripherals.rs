use eframe::egui;
use snow_core::debuggable::{DebuggableProperties, DebuggablePropertyValue};

/// egui widget to display peripheral debug view
pub struct PeripheralsWidget {}

impl PeripheralsWidget {
    //const COLOR_VALUE: egui::Color32 = egui::Color32::WHITE;
    //const COLOR_CHANGED: egui::Color32 = egui::Color32::YELLOW;

    pub fn new() -> Self {
        Self {}
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

    #[allow(clippy::only_used_in_recursion)]
    fn draw_level(&mut self, ui: &mut egui::Ui, pd: &DebuggableProperties) {
        for (row_idx, prop) in pd.iter().enumerate() {
            match prop.value() {
                DebuggablePropertyValue::Header => {
                    ui.label(egui::RichText::new(prop.name()).strong());
                }
                DebuggablePropertyValue::Nested(items) => {
                    ui.collapsing(prop.name(), |ui| self.draw_level(ui, items));
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.set_max_height(16.0);
                        if row_idx % 2 == 0 {
                            ui.painter().rect_filled(
                                ui.max_rect(),
                                0.0,
                                ui.style().visuals.faint_bg_color,
                            );
                        }

                        Self::left_sized(ui, [180.0, 16.0], egui::Label::new(prop.name()));
                        Self::left_sized(
                            ui,
                            [ui.available_width(), 16.0],
                            egui::Label::new(
                                egui::RichText::from(match prop.value() {
                                    DebuggablePropertyValue::Boolean(v) => v.to_string(),
                                    DebuggablePropertyValue::Byte(v) => {
                                        format!("${:02X}", v)
                                    }
                                    DebuggablePropertyValue::ByteBinary(v) => {
                                        format!("{:08b} (${:02X})", v, v)
                                    }
                                    DebuggablePropertyValue::Word(v) => {
                                        format!("${:04X}", v)
                                    }
                                    DebuggablePropertyValue::WordBinary(v) => {
                                        format!("{:016b} (${:04X})", v, v)
                                    }
                                    DebuggablePropertyValue::Long(v) => {
                                        format!("${:08X}", v)
                                    }
                                    DebuggablePropertyValue::SignedDecimal(v) => {
                                        if *v == i64::MAX {
                                            "∞".to_string()
                                        } else {
                                            format!("{}", v)
                                        }
                                    }
                                    DebuggablePropertyValue::UnsignedDecimal(v) => {
                                        if *v == u64::MAX {
                                            "∞".to_string()
                                        } else {
                                            format!("{}", v)
                                        }
                                    }
                                    DebuggablePropertyValue::StaticStr(s) => s.to_string(),
                                    DebuggablePropertyValue::String(s) => s.to_owned(),
                                    DebuggablePropertyValue::Header => unreachable!(),
                                    DebuggablePropertyValue::Nested(_) => unreachable!(),
                                })
                                .monospace(),
                            ),
                        );
                    });
                }
            }
        }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, pd: &DebuggableProperties) {
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .max_height(ui.available_height())
            .show(ui, |ui| self.draw_level(ui, pd));
    }
}
