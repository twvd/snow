use std::cell::RefCell;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use relative_path::{PathExt, RelativePathBuf};
use serde::{Deserialize, Serialize};

/// A workspace representation which contains:
/// * (Paths to) loaded assets
/// * View configuration of the egui frontend
/// * ..probably some hardware configuration
///
/// It does not contain the state of the running emulator, but mirrors
/// part of it. It can be used to re-construct a previously running
/// emulator.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Workspace {
    /// Meta-variable which stores the last saved from/loaded to filename
    /// to resolve relative paths.
    #[serde(skip)]
    file_location: RefCell<Option<PathBuf>>,

    pub log_open: bool,
    pub disassembly_open: bool,
    pub registers_open: bool,
    pub breakpoints_open: bool,
    pub center_viewport_v: bool,
    pub viewport_scale: f32,

    /// Last opened Mac ROM
    rom_path: Option<RelativePathBuf>,

    /// Last loaded disks
    disks: [Option<RelativePathBuf>; 7],
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            file_location: RefCell::new(None),

            viewport_scale: 1.5,
            log_open: false,
            disassembly_open: false,
            registers_open: false,
            breakpoints_open: false,
            center_viewport_v: false,
            rom_path: None,
            disks: core::array::from_fn(|_| None),
        }
    }
}

impl Workspace {
    fn basedir(&self) -> PathBuf {
        use std::env::current_dir;
        if let Some(ref p) = *self.file_location.borrow() {
            p.parent().unwrap().to_path_buf()
        } else {
            current_dir().unwrap()
        }
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let result: Self = serde_json::from_reader(reader)?;
        *result.file_location.borrow_mut() = Some(path.to_path_buf());
        Ok(result)
    }

    pub fn write_file(&mut self, path: &Path) -> Result<()> {
        let newabs = path.parent().context("Invalid path")?;
        if self.file_location.borrow().is_some() {
            // Fix relative paths
            let oldabs = self.basedir();
            self.rom_path = self
                .rom_path
                .clone()
                .map(|p| p.to_path(&oldabs).relative_to(newabs).unwrap().normalize());
            for d in &mut self.disks {
                *d = d
                    .clone()
                    .map(|p| p.to_path(&oldabs).relative_to(newabs).unwrap().normalize());
            }
        }
        *self.file_location.borrow_mut() = Some(path.to_path_buf());

        let file = File::create(path)?;
        Ok(serde_json::to_writer_pretty(file, self)?)
    }

    pub fn get_disk_paths(&self) -> [Option<PathBuf>; 7] {
        let basedir = self.basedir();

        core::array::from_fn(|i| self.disks[i].clone().map(|p| p.to_path(&basedir)))
    }

    pub fn set_disk_paths(&mut self, disks: &[Option<PathBuf>; 7]) {
        let basedir = self.basedir();

        self.disks =
            core::array::from_fn(|i| disks[i].clone().map(|d| d.relative_to(&basedir).unwrap()));
    }

    pub fn set_rom_path(&mut self, p: &Path) {
        self.rom_path = Some(p.relative_to(self.basedir()).unwrap());
    }

    pub fn get_rom_path(&self) -> Option<PathBuf> {
        log::debug!(
            "{:?} {:?} {:?}",
            self.rom_path,
            self.basedir(),
            self.rom_path.clone()?.to_path(self.basedir())
        );
        Some(self.rom_path.clone()?.to_path(self.basedir()))
    }
}
