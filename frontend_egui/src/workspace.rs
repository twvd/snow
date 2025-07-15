use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use crate::emulator::{EmulatorInitArgs, ScsiTarget};
use crate::util::relativepath::RelativePath;
use crate::widgets::framebuffer::ScalingAlgorithm;
use anyhow::{Context, Result};
use eframe::egui;
use serde::{Deserialize, Serialize};
use snow_core::mac::scsi::target::ScsiTargetType;
use snow_core::mac::MacModel;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub enum WorkspaceScsiTarget {
    #[default]
    None,
    Disk(RelativePath),
    Cdrom,
}

impl TryFrom<ScsiTarget> for WorkspaceScsiTarget {
    type Error = ();

    fn try_from(value: ScsiTarget) -> std::result::Result<Self, Self::Error> {
        Ok(match value.target_type.ok_or(())? {
            ScsiTargetType::Disk => {
                Self::Disk(RelativePath::from_absolute(&value.image_path.ok_or(())?))
            }
            ScsiTargetType::Cdrom => Self::Cdrom,
        })
    }
}

impl TryFrom<Option<ScsiTarget>> for WorkspaceScsiTarget {
    type Error = ();

    fn try_from(value: Option<ScsiTarget>) -> std::result::Result<Self, Self::Error> {
        match value {
            None => Ok(Self::None),
            Some(v) => Self::try_from(v),
        }
    }
}

// The opposite is not infallible
#[allow(clippy::from_over_into)]
impl Into<ScsiTarget> for WorkspaceScsiTarget {
    fn into(self) -> ScsiTarget {
        match self {
            Self::None => ScsiTarget {
                target_type: None,
                image_path: None,
            },
            Self::Cdrom => ScsiTarget {
                target_type: Some(ScsiTargetType::Cdrom),
                image_path: None,
            },
            Self::Disk(p) => ScsiTarget {
                target_type: Some(ScsiTargetType::Disk),
                image_path: Some(p.get_absolute()),
            },
        }
    }
}

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
    pub systrap_history_open: bool,
    pub peripheral_debug_open: bool,
    pub terminal_open: [bool; 2],
    pub center_viewport_v: bool,
    pub viewport_scale: f32,

    /// Last opened Mac ROM
    rom_path: Option<RelativePath>,

    /// Last opened Display Card ROM
    display_card_rom_path: Option<RelativePath>,

    /// Last specified PRAM path
    pram_path: Option<RelativePath>,

    /// Last specified extension ROM path
    extension_rom_path: Option<RelativePath>,

    /// Last loaded disks
    /// Deprecated; now scsi_targets
    #[serde(skip_serializing)]
    disks: [Option<RelativePath>; 7],

    /// Configured SCSI targets
    scsi_targets: [WorkspaceScsiTarget; 7],

    /// Window positions
    windows: HashMap<String, [f32; 4]>,

    /// Last emulator initialization args
    pub init_args: EmulatorInitArgs,

    /// Emulated model (None for autodetect)
    pub model: Option<MacModel>,

    /// Map Right ALT to Cmd
    pub map_cmd_ralt: bool,

    /// Scaling algorithm in use
    pub scaling_algorithm: ScalingAlgorithm,
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
            systrap_history_open: false,
            peripheral_debug_open: false,
            terminal_open: [false; 2],
            center_viewport_v: false,
            rom_path: None,
            display_card_rom_path: None,
            pram_path: None,
            extension_rom_path: None,
            disks: Default::default(),
            scsi_targets: Default::default(),
            windows: HashMap::new(),
            init_args: EmulatorInitArgs::default(),
            model: None,
            map_cmd_ralt: true,
            scaling_algorithm: ScalingAlgorithm::Linear,
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
        "System trap history",
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
        if let Some(p) = result.display_card_rom_path.as_mut() {
            p.after_deserialize(parent)?;
        }
        if let Some(p) = result.pram_path.as_mut() {
            p.after_deserialize(parent)?;
        }
        if let Some(p) = result.extension_rom_path.as_mut() {
            p.after_deserialize(parent)?;
        }
        for d in &mut result.scsi_targets {
            match d {
                WorkspaceScsiTarget::Disk(ref mut p) => p.after_deserialize(parent)?,
                WorkspaceScsiTarget::None | WorkspaceScsiTarget::Cdrom => (),
            }
        }
        for (i, d) in result.disks.iter_mut().enumerate() {
            if let Some(p) = d.as_mut() {
                p.after_deserialize(parent)?;

                // Migrate
                result.scsi_targets[i] = WorkspaceScsiTarget::Disk(p.clone());
            }
        }
        result.disks = core::array::from_fn(|_| None);

        Ok(result)
    }

    pub fn write_file(&mut self, path: &Path) -> Result<()> {
        // Resolve relative paths
        let parent = path.parent().context("Cannot resolve parent path")?;
        if let Some(p) = self.rom_path.as_mut() {
            p.before_serialize(parent)?;
        }
        if let Some(p) = self.display_card_rom_path.as_mut() {
            p.before_serialize(parent)?;
        }
        if let Some(p) = self.pram_path.as_mut() {
            p.before_serialize(parent)?;
        }
        if let Some(p) = self.extension_rom_path.as_mut() {
            p.before_serialize(parent)?;
        }
        // disks is deprecated
        for d in &mut self.scsi_targets {
            match d {
                WorkspaceScsiTarget::Disk(ref mut p) => p.before_serialize(parent)?,
                WorkspaceScsiTarget::None | WorkspaceScsiTarget::Cdrom => (),
            }
        }

        let file = File::create(path)?;
        Ok(serde_json::to_writer_pretty(file, self)?)
    }

    pub fn scsi_targets(&self) -> [ScsiTarget; 7] {
        core::array::from_fn(|i| self.scsi_targets[i].clone().into())
    }

    pub fn set_scsi_target(&mut self, id: usize, target: impl Into<ScsiTarget>) {
        self.scsi_targets[id] = target.into().try_into().unwrap_or_default();
    }

    pub fn set_rom_path(&mut self, p: &Path) {
        self.rom_path = Some(RelativePath::from_absolute(p));
    }

    pub fn get_rom_path(&self) -> Option<PathBuf> {
        self.rom_path.clone().map(|d| d.get_absolute())
    }

    pub fn set_extension_rom_path(&mut self, p: Option<&Path>) {
        self.extension_rom_path = p.map(RelativePath::from_absolute);
    }

    pub fn get_extension_rom_path(&self) -> Option<PathBuf> {
        self.extension_rom_path.clone().map(|d| d.get_absolute())
    }

    pub fn set_display_card_rom_path(&mut self, p: Option<&Path>) {
        self.display_card_rom_path = p.map(RelativePath::from_absolute);
    }

    pub fn get_display_card_rom_path(&self) -> Option<PathBuf> {
        self.display_card_rom_path.clone().map(|d| d.get_absolute())
    }

    pub fn set_pram_path(&mut self, p: Option<&Path>) {
        self.pram_path = p.map(RelativePath::from_absolute);
    }

    pub fn get_pram_path(&self) -> Option<PathBuf> {
        self.pram_path.clone().map(|d| d.get_absolute())
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
