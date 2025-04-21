use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::util::relativepath::RelativePath;

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
    pub log_open: bool,
    pub disassembly_open: bool,
    pub registers_open: bool,
    pub breakpoints_open: bool,
    pub memory_open: bool,
    pub watchpoints_open: bool,
    pub instruction_history_open: bool,
    pub peripheral_debug_open: bool,
    pub center_viewport_v: bool,
    pub viewport_scale: f32,

    /// Last opened Mac ROM
    rom_path: Option<RelativePath>,

    /// Last loaded disks
    disks: [Option<RelativePath>; 7],

    /// Window positions
    windows: HashMap<String, [f32; 4]>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            viewport_scale: 1.5,
            log_open: false,
            disassembly_open: false,
            registers_open: false,
            breakpoints_open: false,
            memory_open: false,
            watchpoints_open: false,
            instruction_history_open: false,
            peripheral_debug_open: false,
            center_viewport_v: false,
            rom_path: None,
            disks: core::array::from_fn(|_| None),
            windows: HashMap::new(),
        }
    }
}

impl Workspace {
    /// Names of windows to serialize position/size of
    pub const WINDOW_NAMES: &'static [&'static str] = &[
        "Disassembly",
        "Registers",
        "Log",
        "Breakpoints",
        "Memory",
        "Watchpoints",
        "Instruction history",
        "Peripherals",
    ];

    pub fn from_file(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let mut result: Self = serde_json::from_reader(reader)?;
        let parent = path.parent().context("Cannot resolve parent path")?;
        if let Some(p) = result.rom_path.as_mut() {
            p.after_deserialize(parent)?;
        }
        for d in &mut result.disks {
            if let Some(p) = d.as_mut() {
                p.after_deserialize(parent)?;
            }
        }
        Ok(result)
    }

    pub fn write_file(&mut self, path: &Path) -> Result<()> {
        // Resolve relative paths
        let parent = path.parent().context("Cannot resolve parent path")?;
        if let Some(p) = self.rom_path.as_mut() {
            p.before_serialize(parent)?;
        }
        for d in &mut self.disks {
            if let Some(p) = d.as_mut() {
                p.before_serialize(parent)?;
            }
        }

        let file = File::create(path)?;
        Ok(serde_json::to_writer_pretty(file, self)?)
    }

    pub fn get_disk_paths(&self) -> [Option<PathBuf>; 7] {
        core::array::from_fn(|i| self.disks[i].clone().map(|p| p.get_absolute()))
    }

    pub fn set_disk_paths(&mut self, disks: &[Option<PathBuf>; 7]) {
        self.disks =
            core::array::from_fn(|i| disks[i].as_ref().map(|d| RelativePath::from_absolute(d)));
    }

    pub fn set_rom_path(&mut self, p: &Path) {
        self.rom_path = Some(RelativePath::from_absolute(p));
    }

    pub fn get_rom_path(&self) -> Option<PathBuf> {
        self.rom_path.clone().map(|d| d.get_absolute())
    }

    /// Persists a window location
    pub fn save_window(&mut self, name: &str, rect: egui::Rect) {
        self.windows.insert(
            name.to_string(),
            [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
        );
    }

    /// Retrieves a persisted window location and size.
    /// 'None' indicates the default should be used.
    pub fn get_window(&self, name: &str) -> Option<egui::Rect> {
        let r = self.windows.get(name)?;
        Some(egui::Rect {
            min: egui::Pos2 { x: r[0], y: r[1] },
            max: egui::Pos2 { x: r[2], y: r[3] },
        })
    }

    /// Clears all persisted window locations
    pub fn reset_windows(&mut self) {
        self.windows.clear();
    }
}
