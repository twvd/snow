use std::path::PathBuf;

use eframe::egui;
use egui_file_dialog as efd;

/// A file dialog that can be implemented using either an egui file dialog or a
/// native file dialog provided by `rfd`, depending on user settings.
///
/// Note that the egui_async plugin must be run at the top-level App update
/// function. See egui_async docs for more info.
pub struct SnowFileDialog {
    // Config data is stored in the egui file dialog regardless of whether
    // `egui-file-dialog` or `rfd` are enabled.
    efd_dialog: efd::FileDialog,
    mode: efd::DialogMode,
    rfd_dialog: rfd::AsyncFileDialog,
    rfd_bind: Option<egui_async::Bind<Option<rfd::FileHandle>, ()>>,
    picked: Option<PathBuf>,
}

impl SnowFileDialog {
    pub fn new() -> Self {
        Self {
            efd_dialog: efd::FileDialog::new(),
            mode: efd::DialogMode::PickFile,
            rfd_dialog: rfd::AsyncFileDialog::new(),
            rfd_bind: None,
            picked: None,
        }
    }

    pub fn update(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // Call update on the egui file dialog even if it isn't currently used.
        self.efd_dialog.update(ctx);

        // TODO: only do this in `rfd` mode
        if self.rfd_bind.is_some() {
            self.picked = self.do_rfd_request(frame);
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

    pub fn opening_mode(mut self, opening_mode: efd::OpeningMode) -> Self {
        self.efd_dialog = self.efd_dialog.opening_mode(opening_mode);
        self
    }

    pub fn initial_directory(mut self, directory: PathBuf) -> Self {
        self.efd_dialog = self.efd_dialog.initial_directory(directory);
        self
    }

    pub fn storage(mut self, storage: efd::FileDialogStorage) -> Self {
        self.efd_dialog = self.efd_dialog.storage(storage);
        self
    }

    pub fn pick_file(&mut self) {
        self.mode = efd::DialogMode::PickFile;

        // TODO: choose between `egui_file_dialog` or `rfd`
        self.rfd_bind = Some(Default::default());
    }

    pub fn save_file(&mut self) {
        self.mode = efd::DialogMode::SaveFile;

        // TODO: choose between `egui_file_dialog` or `rfd`
        self.rfd_bind = Some(Default::default());
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
        self.picked.take()
    }

    fn do_rfd_request(&mut self, frame: &eframe::Frame) -> Option<PathBuf> {
        let res = match self.mode {
            efd::DialogMode::PickFile => self.request_rfd_pick_file(frame),
            efd::DialogMode::SaveFile => self.request_rfd_save_file(frame),
            _ => todo!(),
        };

        if let Some(res) = res {
            println!("rfd_task result: {:#?}", res);

            if let Ok(Some(file)) = res {
                let result = Some(file.path().into());
                self.rfd_bind = None;
                return result;
            }

            self.rfd_bind = None;
        }

        None
    }

    fn request_rfd_pick_file(
        &mut self,
        frame: &eframe::Frame,
    ) -> Option<&Result<Option<rfd::FileHandle>, ()>> {
        assert_eq!(self.mode, efd::DialogMode::PickFile);

        self.rfd_bind.as_mut().unwrap().read_or_request(|| {
            // FIXME: Construct rfd_dialog based on current efd config
            let rfd_task = self.rfd_dialog.clone().set_parent(frame).pick_file();
            async { Ok(rfd_task.await) }
        })
    }

    fn request_rfd_save_file(
        &mut self,
        frame: &eframe::Frame,
    ) -> Option<&Result<Option<rfd::FileHandle>, ()>> {
        assert_eq!(self.mode, efd::DialogMode::SaveFile);

        self.rfd_bind.as_mut().unwrap().read_or_request(|| {
            // FIXME: Construct rfd_dialog based on current efd config
            let rfd_task = self.rfd_dialog.clone().set_parent(frame).save_file();
            async { Ok(rfd_task.await) }
        })
    }
}
