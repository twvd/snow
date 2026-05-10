use eframe::egui;

/// Result of a confirmation dialog interaction.
pub struct ConfirmAnswer {
    /// Index of the clicked button (matching the order passed to `ask`).
    pub button: usize,
    /// True if the optional "remember" checkbox was ticked. Always false
    /// when the dialog was opened without a remember label.
    pub remember: bool,
}

/// Generic modal yes/no/N-choice question dialog.
pub struct ConfirmDialog {
    state: Option<State>,
}

struct State {
    title: String,
    body: String,
    buttons: Vec<String>,
    /// When `Some`, a checkbox with this label is shown above the buttons.
    remember_label: Option<String>,
    remember: bool,
    answer: Option<usize>,
}

impl ConfirmDialog {
    pub fn new() -> Self {
        Self { state: None }
    }

    /// Opens the dialog with the given title, body and button labels.
    /// Replaces any active question.
    pub fn ask(&mut self, title: impl Into<String>, body: impl Into<String>, buttons: Vec<String>) {
        self.open(title, body, buttons, None);
    }

    /// Opens the dialog with an additional "remember" checkbox above the
    /// buttons. The checkbox state is returned alongside the button index
    /// in [`ConfirmAnswer::remember`].
    pub fn ask_with_remember(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        buttons: Vec<String>,
        remember_label: impl Into<String>,
    ) {
        self.open(title, body, buttons, Some(remember_label.into()));
    }

    fn open(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        buttons: Vec<String>,
        remember_label: Option<String>,
    ) {
        self.state = Some(State {
            title: title.into(),
            body: body.into(),
            buttons,
            remember_label,
            remember: false,
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
            if let Some(label) = state.remember_label.as_deref() {
                ui.checkbox(&mut state.remember, label);
                ui.add_space(4.0);
            }
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

    /// Returns the user's choice and closes the dialog. Returns `None`
    /// while the dialog is still awaiting a click.
    pub fn take_answer(&mut self) -> Option<ConfirmAnswer> {
        let state = self.state.as_ref()?;
        let button = state.answer?;
        let remember = state.remember;
        self.state = None;
        Some(ConfirmAnswer { button, remember })
    }
}

impl Default for ConfirmDialog {
    fn default() -> Self {
        Self::new()
    }
}
