use std::{path::PathBuf, sync::Arc};

use eframe::egui;
use egui_file_dialog as efd;

enum RfdState {
    /// No `rfd` dialog is active
    Inactive,
    /// An `rfd` dialog will be launched on the next call to update
    Pending,
    /// An `rfd` dialog is active
    Active(egui_async::Bind<Option<Picked>, ()>),
}

#[derive(Clone, Debug, Default)]
enum Picked {
    #[default]
    None,
    Single(PathBuf),
    Multiple(Vec<PathBuf>),
}

/// A file dialog that can be implemented using either an egui file dialog or a
/// native file dialog provided by `rfd`, depending on user settings.
///
/// Note that the egui_async plugin must be run at the top-level App update
/// function. See egui_async docs for more info.
pub struct SnowFileDialog {
    // Config data is stored in the egui file dialog regardless of whether
    // `egui-file-dialog` or `rfd` are enabled.
    efd_dialog: efd::FileDialog,
    rfd_dialog: rfd::AsyncFileDialog,
    rfd_state: RfdState,
    mode: efd::DialogMode,
    picked: Picked,
}

impl Default for SnowFileDialog {
    fn default() -> Self {
        Self {
            efd_dialog: efd::FileDialog::new(),
            rfd_dialog: rfd::AsyncFileDialog::new(),
            rfd_state: RfdState::Inactive,
            mode: efd::DialogMode::PickFile,
            picked: Picked::None,
        }
    }
}

impl SnowFileDialog {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn update(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // Always call update on the egui file dialog even if it isn't currently used.
        self.efd_dialog.update(ctx);

        match &self.rfd_state {
            RfdState::Pending => {
                self.launch_rfd(frame);
            }
            RfdState::Active(_) => {
                self.read_rfd();
            }
            _ => match self.efd_dialog.state() {
                efd::DialogState::Picked(_) => {
                    self.picked = Picked::Single(self.efd_dialog.take_picked().unwrap());
                }
                efd::DialogState::PickedMultiple(_) => {
                    self.picked = Picked::Multiple(self.efd_dialog.take_picked_multiple().unwrap());
                }
                _ => (),
            },
        }
    }

    /// Add a filename filter to the dialog. The first filter added will be the default.
    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        let name = name.into();
        let efd_extensions: Vec<_> = extensions.iter().map(|item| item.to_string()).collect();

        self.efd_dialog = self.efd_dialog.add_file_filter(
            &name,
            Arc::new(move |p| {
                let ext = p
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                efd_extensions.iter().any(|s| ext.eq_ignore_ascii_case(s))
            }),
        );

        // The first filter added will be set as the default.
        if self.efd_dialog.config_mut().default_file_filter.is_none() {
            self.efd_dialog = self.efd_dialog.default_file_filter(&name);
        }

        self.rfd_dialog = self.rfd_dialog.add_filter(name, extensions);

        self
    }

    pub fn add_save_extension(mut self, name: &str, file_extension: &str) -> Self {
        self.efd_dialog = self.efd_dialog.add_save_extension(name, file_extension);
        self
    }

    pub fn default_save_extension(mut self, name: &str) -> Self {
        self.efd_dialog = self.efd_dialog.default_save_extension(name);
        self
    }

    pub fn allow_path_edit_to_save_file_without_extension(mut self, allow: bool) -> Self {
        self.efd_dialog = self
            .efd_dialog
            .allow_path_edit_to_save_file_without_extension(allow);
        self
    }

    pub fn opening_mode(mut self, opening_mode: efd::OpeningMode) -> Self {
        self.efd_dialog = self.efd_dialog.opening_mode(opening_mode);
        self
    }

    pub fn initial_directory(mut self, directory: PathBuf) -> Self {
        self.efd_dialog = self.efd_dialog.initial_directory(directory);
        self
    }

    pub fn show_pinned_folders(mut self, show_pinned_folders: bool) -> Self {
        self.efd_dialog = self.efd_dialog.show_pinned_folders(show_pinned_folders);
        self
    }

    pub fn storage(mut self, storage: efd::FileDialogStorage) -> Self {
        self.efd_dialog = self.efd_dialog.storage(storage);
        self
    }

    pub fn pick_file(&mut self, use_native: bool) {
        self.mode = efd::DialogMode::PickFile;

        if use_native {
            self.rfd_state = RfdState::Pending;
        } else {
            self.efd_dialog.pick_file();
        }
    }

    pub fn pick_directory(&mut self, use_native: bool) {
        self.mode = efd::DialogMode::PickDirectory;

        if use_native {
            self.rfd_state = RfdState::Pending;
        } else {
            self.efd_dialog.pick_directory();
        }
    }

    pub fn pick_multiple(&mut self, use_native: bool) {
        self.mode = efd::DialogMode::PickMultiple;

        if use_native {
            self.rfd_state = RfdState::Pending;
        } else {
            self.efd_dialog.pick_multiple();
        }
    }

    pub fn save_file(&mut self, use_native: bool) {
        self.mode = efd::DialogMode::SaveFile;

        if use_native {
            self.rfd_state = RfdState::Pending;
        } else {
            self.efd_dialog.save_file();
        }
    }

    pub fn config_mut(&mut self) -> &mut efd::FileDialogConfig {
        self.efd_dialog.config_mut()
    }

    pub fn storage_mut(&mut self) -> &mut efd::FileDialogStorage {
        self.efd_dialog.storage_mut()
    }

    pub const fn mode(&self) -> efd::DialogMode {
        self.mode
    }

    pub const fn state(&self) -> &efd::DialogState {
        if matches!(self.rfd_state, RfdState::Pending | RfdState::Active(_)) {
            &efd::DialogState::Open
        } else {
            self.efd_dialog.state()
        }
    }

    pub fn take_picked(&mut self) -> Option<PathBuf> {
        let picked = std::mem::take(&mut self.picked);
        match picked {
            Picked::None => None,
            Picked::Single(path) => Some(path),
            Picked::Multiple(_) => unreachable!(),
        }
    }

    pub fn take_picked_multiple(&mut self) -> Option<Vec<PathBuf>> {
        let picked = std::mem::take(&mut self.picked);
        match picked {
            Picked::None => None,
            Picked::Single(_) => unreachable!(),
            Picked::Multiple(paths) => Some(paths),
        }
    }

    fn build_rfd_dialog(&mut self, frame: &eframe::Frame) -> rfd::AsyncFileDialog {
        let mut dialog = self.rfd_dialog.clone();

        dialog = dialog.set_parent(frame);

        // Always add a filter for all files.
        // Whether to use "All files" or "All Files" is inconsistent across Windows
        // apps, even ones designed by Microsoft.
        dialog = dialog.add_filter("All files", &["*"]);

        // See `get_initial_directory` in egui-file-dialog
        let directory = match self.efd_dialog.config_mut().opening_mode {
            efd::OpeningMode::AlwaysInitialDir => {
                self.efd_dialog.config_mut().initial_directory.clone()
            }
            efd::OpeningMode::LastPickedDir => self
                .efd_dialog
                .storage_mut()
                .last_picked_dir
                .clone()
                .unwrap_or_else(|| self.efd_dialog.config_mut().initial_directory.clone()),
            efd::OpeningMode::LastVisitedDir => self
                .efd_dialog
                .storage_mut()
                .last_visited_dir
                .clone()
                .unwrap_or_else(|| self.efd_dialog.config_mut().initial_directory.clone()),
        };

        dialog = dialog.set_directory(directory);

        dialog
    }

    fn launch_rfd(&mut self, frame: &eframe::Frame) {
        assert!(matches!(self.rfd_state, RfdState::Pending));

        let dialog = self.build_rfd_dialog(frame);

        match self.mode {
            efd::DialogMode::PickFile => {
                let mut req = egui_async::Bind::<Option<Picked>, ()>::default();
                let rfd_task = dialog.pick_file();
                req.request(async { Ok(rfd_task.await.map(|item| Picked::Single(item.into()))) });
                self.rfd_state = RfdState::Active(req);
            }
            efd::DialogMode::PickDirectory => {
                let mut req = egui_async::Bind::<Option<Picked>, ()>::default();
                let rfd_task = dialog.pick_folder();
                req.request(async { Ok(rfd_task.await.map(|item| Picked::Single(item.into()))) });
                self.rfd_state = RfdState::Active(req);
            }
            efd::DialogMode::PickMultiple => {
                let mut req = egui_async::Bind::<Option<Picked>, ()>::default();
                let rfd_task = dialog.pick_files();
                req.request(async {
                    Ok(rfd_task.await.map(|item| {
                        Picked::Multiple(item.into_iter().map(|item| item.into()).collect())
                    }))
                });
                self.rfd_state = RfdState::Active(req);
            }
            efd::DialogMode::SaveFile => {
                let mut req = egui_async::Bind::<Option<Picked>, ()>::default();
                let rfd_task = dialog.save_file();
                req.request(async { Ok(rfd_task.await.map(|item| Picked::Single(item.into()))) });
                self.rfd_state = RfdState::Active(req);
            }
        }

        self.read_rfd();
    }

    fn read_rfd(&mut self) {
        match &mut self.rfd_state {
            RfdState::Active(bind) => match bind.read() {
                Some(Ok(res)) => {
                    let res = res.clone();
                    self.rfd_state = RfdState::Inactive;

                    // Store last visited directory to efd dialog
                    if let Some(ref res) = res {
                        let last_visited_dir = match res {
                            Picked::Single(path) => path.to_path_buf(),
                            Picked::Multiple(paths) => paths[0].clone(),
                            _ => unreachable!(),
                        };

                        let last_visited_dir = if self.mode == efd::DialogMode::PickDirectory {
                            Some(last_visited_dir)
                        } else {
                            last_visited_dir.parent().map(|path| path.to_path_buf())
                        };

                        self.efd_dialog.storage_mut().last_visited_dir = last_visited_dir.clone();
                        self.efd_dialog.storage_mut().last_picked_dir = last_visited_dir;
                    }

                    self.picked = res.unwrap_or(Picked::None);
                }
                Some(_) => unreachable!(),
                None => self.picked = Picked::None,
            },
            _ => unreachable!(),
        }
    }
}
