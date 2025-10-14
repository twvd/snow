use anyhow::Result;
use egui_file_dialog::FileDialogStorage;
use serde::{Deserialize, Serialize};
use snow_core::mac::MacModel;
use std::path::{Path, PathBuf};

const MAX_RECENT_WORKSPACES: usize = 10;
const MAX_RECENT_IMAGES: usize = 10;

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
#[serde(default)]
pub struct AppSettings {
    pub recent_workspaces: Vec<PathBuf>,
    pub recent_floppy_images: Vec<PathBuf>,
    pub recent_cd_images: Vec<PathBuf>,
    pub last_roms: Vec<(MacModel, PathBuf)>,
    pub last_display_roms: Vec<(MacModel, PathBuf)>,

    pub fd_hdd: FileDialogStorage,
    pub fd_cdrom: FileDialogStorage,
    pub fd_cdrom_files: FileDialogStorage,
    pub fd_floppy: FileDialogStorage,
    pub fd_record: FileDialogStorage,
    pub fd_workspace: FileDialogStorage,
    pub fd_state: FileDialogStorage,
    pub fd_shared_dir: FileDialogStorage,
}

impl AppSettings {
    fn config_path() -> Result<PathBuf> {
        if let Some(mut config_dir) = dirs::config_dir() {
            config_dir.push("snowemu"); // Add vendor
            config_dir.push("Snow"); // Add application name
            std::fs::create_dir_all(&config_dir)?;
            Ok(config_dir.join("settings.json"))
        } else {
            anyhow::bail!("Could not determine config directory");
        }
    }

    pub fn load() -> Self {
        if let Ok(path) = Self::config_path() {
            if let Ok(file) = std::fs::File::open(path) {
                if let Ok(settings) = serde_json::from_reader(std::io::BufReader::new(file)) {
                    return settings;
                }
            }
        }
        Default::default()
    }

    pub fn save(&self) {
        if let Err(e) = self.try_save() {
            log::warn!("Failed to save settings: {}", e);
        }
    }

    pub fn try_save(&self) -> Result<()> {
        let path = Self::config_path()?;
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }

    pub fn add_recent_workspace(&mut self, path: &Path) {
        let path = if let Ok(p) = path.canonicalize() {
            p
        } else {
            path.to_path_buf()
        };

        self.recent_workspaces.retain(|p| {
            if let Ok(rp) = p.canonicalize() {
                rp != path
            } else {
                *p != path
            }
        });
        self.recent_workspaces.insert(0, path);
        self.recent_workspaces.truncate(MAX_RECENT_WORKSPACES);
        self.save();
    }

    pub fn get_recent_workspaces_for_display(&self) -> Vec<(usize, PathBuf, String)> {
        self.recent_workspaces
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let display_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                (i + 1, path.clone(), display_name)
            })
            .collect()
    }

    pub fn add_recent_floppy_image(&mut self, path: &Path) {
        let path = if let Ok(p) = path.canonicalize() {
            p
        } else {
            path.to_path_buf()
        };

        self.recent_floppy_images.retain(|p| {
            if let Ok(rp) = p.canonicalize() {
                rp != path
            } else {
                *p != path
            }
        });
        self.recent_floppy_images.insert(0, path);
        self.recent_floppy_images.truncate(MAX_RECENT_IMAGES);
        self.save();
    }

    pub fn add_recent_cd_image(&mut self, path: &Path) {
        let path = if let Ok(p) = path.canonicalize() {
            p
        } else {
            path.to_path_buf()
        };

        self.recent_cd_images.retain(|p| {
            if let Ok(rp) = p.canonicalize() {
                rp != path
            } else {
                *p != path
            }
        });
        self.recent_cd_images.insert(0, path);
        self.recent_cd_images.truncate(MAX_RECENT_IMAGES);
        self.save();
    }

    pub fn set_last_rom(&mut self, model: MacModel, path: &Path) {
        self.last_roms.retain(|(m, _)| *m != model);
        self.last_roms.push((model, path.to_path_buf()));
        self.save();
    }

    pub fn get_last_roms(&self) -> Vec<(MacModel, PathBuf)> {
        self.last_roms.clone()
    }

    pub fn get_last_display_roms(&self) -> Vec<(MacModel, PathBuf)> {
        self.last_display_roms.clone()
    }

    pub fn set_last_display_rom(&mut self, model: MacModel, path: &Path) {
        self.last_display_roms.retain(|(m, _)| *m != model);
        self.last_display_roms.push((model, path.to_path_buf()));
        self.save();
    }

    pub fn get_recent_floppy_images_for_display(&self) -> Vec<(usize, PathBuf, String)> {
        self.recent_floppy_images
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let display_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                (i + 1, path.clone(), display_name)
            })
            .collect()
    }

    pub fn get_recent_cd_images_for_display(&self) -> Vec<(usize, PathBuf, String)> {
        self.recent_cd_images
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let display_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                (i + 1, path.clone(), display_name)
            })
            .collect()
    }
}
