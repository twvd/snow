use std::path::PathBuf;

use eframe::egui;
use egui_file_dialog as efd;

#[derive(Clone, Debug)]
enum Pick {
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
    rfd_bind: Option<egui_async::Bind<Option<Pick>, ()>>,
    mode: efd::DialogMode,
    pick: Option<Pick>,
}

impl Default for SnowFileDialog {
    fn default() -> Self {
        Self {
            efd_dialog: efd::FileDialog::new(),
            rfd_dialog: rfd::AsyncFileDialog::new(),
            rfd_bind: None,
            mode: efd::DialogMode::PickFile,
            pick: None,
        }
    }
}

impl SnowFileDialog {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn update(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // Call update on the egui file dialog even if it isn't currently used.
        self.efd_dialog.update(ctx);

        if self.rfd_bind.is_some() {
            self.pick = self.do_rfd_request(frame);
        } else if matches!(self.efd_dialog.state(), efd::DialogState::Picked(_)) {
            self.pick = Some(Pick::Single(self.efd_dialog.take_picked().unwrap()));
        } else if matches!(self.efd_dialog.state(), efd::DialogState::PickedMultiple(_)) {
            self.pick = Some(Pick::Multiple(
                self.efd_dialog.take_picked_multiple().unwrap(),
            ))
        }
    }

    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        // TODO: add filter to efd_dialog
        self.rfd_dialog = self.rfd_dialog.add_filter(name, &extensions);
        self
    }

    pub fn default_file_filter(mut self, name: &str) -> Self {
        self.efd_dialog = self.efd_dialog.default_file_filter(name);
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
            self.rfd_bind = Some(Default::default());
        } else {
            self.efd_dialog.pick_file();
        }
    }

    pub fn pick_multiple(&mut self, use_native: bool) {
        self.mode = efd::DialogMode::PickMultiple;

        if use_native {
            self.rfd_bind = Some(Default::default());
        } else {
            self.efd_dialog.pick_multiple();
        }
    }

    pub fn save_file(&mut self, use_native: bool) {
        self.mode = efd::DialogMode::SaveFile;

        if use_native {
            self.rfd_bind = Some(Default::default());
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
        if self.rfd_bind.is_some() {
            &efd::DialogState::Open
        } else {
            self.efd_dialog.state()
        }
    }

    pub fn take_picked(&mut self) -> Option<PathBuf> {
        match &self.pick {
            Some(Pick::Single(path)) => {
                let result = path.clone();
                self.pick = None;
                Some(result)
            }
            Some(_) => unreachable!(),
            None => None,
        }
    }

    pub fn take_picked_multiple(&mut self) -> Option<Vec<PathBuf>> {
        match &self.pick {
            Some(Pick::Multiple(paths)) => {
                let result = paths.clone();
                self.pick = None;
                Some(result)
            }
            Some(_) => unreachable!(),
            None => None,
        }
    }

    fn do_rfd_request(&mut self, frame: &eframe::Frame) -> Option<Pick> {
        let res = match self.mode {
            efd::DialogMode::PickFile => self.request_rfd_pick_file(frame),
            efd::DialogMode::PickMultiple => self.request_rfd_pick_multiple(frame),
            efd::DialogMode::SaveFile => self.request_rfd_save_file(frame),
            _ => todo!(),
        };

        if let Some(res) = res {
            println!("rfd_task result: {:#?}", res);

            if let Ok(Some(file)) = res {
                let result = file.clone();
                self.rfd_bind = None;
                return Some(result);
            }

            self.rfd_bind = None;
        }

        None
    }

    fn request_rfd_pick_file(
        &mut self,
        frame: &eframe::Frame,
    ) -> Option<&Result<Option<Pick>, ()>> {
        assert_eq!(self.mode, efd::DialogMode::PickFile);

        self.rfd_bind.as_mut().unwrap().read_or_request(|| {
            // FIXME: Construct rfd_dialog based on current efd config
            let rfd_task = self.rfd_dialog.clone().set_parent(frame).pick_file();
            async { Ok(rfd_task.await.map(|item| Pick::Single(item.into()))) }
        })
    }

    fn request_rfd_pick_multiple(
        &mut self,
        frame: &eframe::Frame,
    ) -> Option<&Result<Option<Pick>, ()>> {
        assert_eq!(self.mode, efd::DialogMode::PickMultiple);

        self.rfd_bind.as_mut().unwrap().read_or_request(|| {
            // FIXME: Construct rfd_dialog based on current efd config
            let rfd_task = self.rfd_dialog.clone().set_parent(frame).pick_files();
            async {
                Ok(rfd_task
                    .await
                    .map(|item| Pick::Multiple(item.into_iter().map(|item| item.into()).collect())))
            }
        })
    }

    fn request_rfd_save_file(
        &mut self,
        frame: &eframe::Frame,
    ) -> Option<&Result<Option<Pick>, ()>> {
        assert_eq!(self.mode, efd::DialogMode::SaveFile);

        self.rfd_bind.as_mut().unwrap().read_or_request(|| {
            // FIXME: Construct rfd_dialog based on current efd config
            let rfd_task = self.rfd_dialog.clone().set_parent(frame).pick_file();
            async { Ok(rfd_task.await.map(|item| Pick::Single(item.into()))) }
        })
    }
}
