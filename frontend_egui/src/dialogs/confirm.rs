use eframe::egui;

/// Generic modal yes/no/N-choice question dialog.
pub struct ConfirmDialog {
    state: Option<State>,
}

struct State {
    title: String,
    body: String,
    buttons: Vec<String>,
    answer: Option<usize>,
}

impl ConfirmDialog {
    pub fn new() -> Self {
        Self { state: None }
    }

    /// Opens the dialog with the given title, body and button labels.
    /// Replaces any active question.
    pub fn ask(&mut self, title: impl Into<String>, body: impl Into<String>, buttons: Vec<String>) {
        self.state = Some(State {
            title: title.into(),
            body: body.into(),
            buttons,
            answer: None,
        });
    }

    pub fn is_open(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.answer.is_none())
    }

    pub fn update(&mut self, ctx: &egui::Context) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.answer.is_some() {
            return;
        }

        egui::Modal::new(egui::Id::new("ConfirmDialog")).show(ctx, |ui| {
            ui.set_max_width(450.0);
            ui.heading(&state.title);
            ui.add_space(8.0);
            ui.label(&state.body);
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (idx, label) in state.buttons.iter().enumerate() {
                    if ui.button(label).clicked() {
                        state.answer = Some(idx);
                    }
                }
            });
        });
    }

    /// Returns the index of the clicked button (matching the order passed
    /// to [`Self::ask`]) and closes the dialog. Returns `None` if no
    /// answer yet.
    pub fn take_answer(&mut self) -> Option<usize> {
        let answer = self.state.as_ref()?.answer?;
        self.state = None;
        Some(answer)
    }
}

impl Default for ConfirmDialog {
    fn default() -> Self {
        Self::new()
    }
}
