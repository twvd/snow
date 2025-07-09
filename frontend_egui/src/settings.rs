use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_RECENT_WORKSPACES: usize = 10;

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct AppSettings {
    pub recent_workspaces: Vec<PathBuf>,
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

    pub fn save(&self) -> Result<()> {
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
        if let Err(e) = self.save() {
            log::warn!("Failed to save settings: {}", e);
        }
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
}
