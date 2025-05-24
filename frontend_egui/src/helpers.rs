//! egui extensions and helper functions

use eframe::egui;

/// Helper to create a UI that takes up the size but aligns left
pub fn left_sized(
    ui: &mut egui::Ui,
    max_size: impl Into<egui::Vec2> + Clone,
    widget: impl egui::Widget,
) -> egui::Response {
    left_sized_f(ui, max_size, |ui| {
        ui.add(widget);
    })
}

/// Helper to create a UI that takes up the size but aligns left
pub fn left_sized_f(
    ui: &mut egui::Ui,
    max_size: impl Into<egui::Vec2> + Clone,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    ui.scope(|ui| {
        ui.set_max_size(max_size.clone().into());
        ui.set_min_size(max_size.into());
        add_contents(ui);
    })
    .response
}

/// Helper to create a UI that takes up the size but aligns left, with
/// a material icon adjusted to fit in line with the monospace font.
pub fn left_sized_icon(
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
