use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use crate::emulator::{EmulatorInitArgs, SavedDevice};
use crate::shader_pipeline::ShaderConfig;
use crate::util::relativepath::RelativePath;
use crate::widgets::framebuffer::ScalingAlgorithm;
use anyhow::{Context, Result};
use eframe::egui;
use serde::{Deserialize, Deserializer, Serialize};
use snow_core::emulator::ports::NewDevice;
use snow_core::mac::MacModel;
#[cfg(feature = "ethernet")]
use snow_core::mac::scsi::ethernet::EthernetLinkType;
use snow_core::mac::serial_bridge::SerialBridgeConfig;

/// Custom deserializer that skips invalid shader configs instead of failing entirely
fn deserialize_shader_configs_lenient<'de, D>(
    deserializer: D,
) -> Result<Vec<ShaderConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let values: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
    let mut configs = Vec::new();

    for (i, value) in values.into_iter().enumerate() {
        match serde_json::from_value::<ShaderConfig>(value) {
            Ok(config) => configs.push(config),
            Err(e) => {
                log::warn!("Skipping invalid shader config at index {}: {}", i, e);
            }
        }
    }

    Ok(configs)
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FramebufferMode {
    CenteredHorizontally,
    #[default]
    Centered,
    Detached,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub enum WorkspaceDeviceKind {
    #[default]
    None,
    Disk(RelativePath),
    ReadOnlyDisk(RelativePath),
    Cdrom,
    /// Folder-backed ("Toolbox managed") CD-ROM: the guest swaps between the
    /// images in this folder via the BlueSCSI Toolbox CD commands.
    CdromFolder(RelativePath),
    // Do not feature gate Ethernet here to avoid problems with loading
    // workspaces on builds without the ethernet feature
    Ethernet,
    Printer,
}

/// A device and the port it's in
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WorkspaceDevice {
    family: String,
    id: String,
    device: WorkspaceDeviceKind,
}

impl WorkspaceDevice {
    fn from_saved(saved: &SavedDevice) -> Self {
        let device = match &saved.device {
            NewDevice::HardDisk(p) => WorkspaceDeviceKind::Disk(RelativePath::from_absolute(p)),
            NewDevice::ReadOnlyHardDisk(p) => {
                WorkspaceDeviceKind::ReadOnlyDisk(RelativePath::from_absolute(p))
            }
            NewDevice::CdromDrive => WorkspaceDeviceKind::Cdrom,
            NewDevice::CdromFolder(p) => {
                WorkspaceDeviceKind::CdromFolder(RelativePath::from_absolute(p))
            }
            #[cfg(feature = "ethernet")]
            NewDevice::Ethernet => WorkspaceDeviceKind::Ethernet,
            #[cfg(feature = "printer")]
            NewDevice::Printer(_) => WorkspaceDeviceKind::Printer,
        };
        Self {
            family: saved.family.clone(),
            id: saved.id.clone(),
            device,
        }
    }

    fn to_saved(&self) -> Option<SavedDevice> {
        let device = match &self.device {
            WorkspaceDeviceKind::None => return None,
            WorkspaceDeviceKind::Disk(p) => NewDevice::HardDisk(p.get_absolute()),
            WorkspaceDeviceKind::ReadOnlyDisk(p) => NewDevice::ReadOnlyHardDisk(p.get_absolute()),
            WorkspaceDeviceKind::Cdrom => NewDevice::CdromDrive,
            WorkspaceDeviceKind::CdromFolder(p) => NewDevice::CdromFolder(p.get_absolute()),
            #[cfg(feature = "ethernet")]
            WorkspaceDeviceKind::Ethernet => NewDevice::Ethernet,
            #[cfg(not(feature = "ethernet"))]
            WorkspaceDeviceKind::Ethernet => return None,
            #[cfg(feature = "printer")]
            WorkspaceDeviceKind::Printer => {
                NewDevice::Printer(crate::emulator::EmulatorState::printer_output_dir())
            }
            #[cfg(not(feature = "printer"))]
            WorkspaceDeviceKind::Printer => return None,
        };
        Some(SavedDevice {
            family: self.family.clone(),
            id: self.id.clone(),
            device,
        })
    }
}

/// Workspace variant of link mode for ethernet device
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum WorkspaceEthernetLinkType {
    /// Userland NAT
    #[cfg(feature = "ethernet_nat")]
    NAT,
    /// Userland NAT with HTTPS stripping
    #[cfg(feature = "ethernet_nat_https_stripping")]
    NATHttpsStripping,
    /// Raw sockets based bridge
    #[cfg(feature = "ethernet_raw")]
    Bridge(u32),
    /// Tap interface based bridge
    #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
    TapBridge(String),
    /// No link
    #[serde(other)]
    Down,
}

#[allow(clippy::derivable_impls)]
impl Default for WorkspaceEthernetLinkType {
    fn default() -> Self {
        #[cfg(feature = "ethernet_nat")]
        {
            Self::NAT
        }
        #[cfg(not(feature = "ethernet_nat"))]
        {
            Self::Down
        }
    }
}

#[cfg(feature = "ethernet")]
impl From<EthernetLinkType> for WorkspaceEthernetLinkType {
    fn from(ty: EthernetLinkType) -> Self {
        match ty {
            EthernetLinkType::Down => Self::Down,
            #[cfg(feature = "ethernet_nat")]
            EthernetLinkType::NAT => Self::NAT,
            #[cfg(feature = "ethernet_nat_https_stripping")]
            EthernetLinkType::NATHttpsStripping => Self::NATHttpsStripping,
            #[cfg(feature = "ethernet_raw")]
            EthernetLinkType::Bridge(i) => Self::Bridge(i),
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            EthernetLinkType::TapBridge(s) => Self::TapBridge(s),
        }
    }
}

#[cfg(feature = "ethernet")]
impl From<WorkspaceEthernetLinkType> for EthernetLinkType {
    fn from(ty: WorkspaceEthernetLinkType) -> Self {
        match ty {
            WorkspaceEthernetLinkType::Down => Self::Down,
            #[cfg(feature = "ethernet_nat")]
            WorkspaceEthernetLinkType::NAT => Self::NAT,
            #[cfg(feature = "ethernet_nat_https_stripping")]
            WorkspaceEthernetLinkType::NATHttpsStripping => Self::NATHttpsStripping,
            #[cfg(feature = "ethernet_raw")]
            WorkspaceEthernetLinkType::Bridge(i) => Self::Bridge(i),
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            WorkspaceEthernetLinkType::TapBridge(s) => Self::TapBridge(s),
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

    /// Deprecated
    #[serde(skip_serializing)]
    center_viewport_v: Option<bool>,

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

    /// Deprecated; now devices
    #[serde(skip_serializing)]
    scsi_targets: [WorkspaceDeviceKind; 7],

    /// Devices in the machine's ports
    devices: Vec<WorkspaceDevice>,

    /// Window positions
    windows: HashMap<String, [f32; 4]>,

    /// Last emulator initialization args
    pub init_args: EmulatorInitArgs,

    /// Emulated model (None for autodetect)
    pub model: Option<MacModel>,

    /// Scaling algorithm in use
    pub scaling_algorithm: ScalingAlgorithm,

    /// Pause emulator after loading a state/state file
    pub pause_on_state_load: bool,

    /// Shared directory for BlueSCSI toolbox
    shared_dir: Option<RelativePath>,

    /// Show labels in disassembly
    pub disassembly_labels: bool,
    /// The disassembly looks at the PC as a physical address
    pub disassembly_physical: bool,

    /// Floppy disk images to auto-insert on workspace load
    floppy_images: Vec<RelativePath>,

    /// Custom date/time to set the RTC to on startup.
    /// Format: "YYYY-MM-DD" or "YYYY-MM-DD HH:MM:SS"
    /// Useful for testing date-dependent software (e.g., easter eggs).
    pub custom_datetime: Option<String>,

    /// Framebuffer positioning mode
    pub framebuffer_mode: FramebufferMode,

    /// CRT shader enabled
    pub shader_enabled: bool,

    /// Shader pipeline configuration
    #[serde(deserialize_with = "deserialize_shader_configs_lenient")]
    pub shader_configs: Vec<ShaderConfig>,

    /// Ethernet link type
    pub ethernet_link_type: WorkspaceEthernetLinkType,

    /// Serial bridges per SCC channel (index 0 = Channel A, index 1 = Channel B)
    pub serial_bridges: [Option<SerialBridgeConfig>; 2],
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            viewport_scale: 2.0,
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
            center_viewport_v: None,
            rom_path: None,
            display_card_rom_path: None,
            pram_path: None,
            extension_rom_path: None,
            disks: Default::default(),
            scsi_targets: Default::default(),
            devices: Vec::new(),
            windows: HashMap::new(),
            init_args: EmulatorInitArgs::default(),
            model: None,
            scaling_algorithm: ScalingAlgorithm::Linear,
            pause_on_state_load: false,
            shared_dir: None,
            disassembly_labels: true,
            disassembly_physical: false,
            floppy_images: Vec::new(),
            custom_datetime: None,
            framebuffer_mode: FramebufferMode::default(),
            shader_enabled: false,
            shader_configs: Vec::new(),
            ethernet_link_type: WorkspaceEthernetLinkType::default(),
            serial_bridges: [None, None],
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
        "Display",
    ];

    pub fn from_file(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let mut result: Self = serde_json::from_reader(reader)?;

        let parent = path.parent().context("Cannot resolve parent path")?;
        // Default to the current working directory if the input path has no parent. It could be
        // relative to the current directory and provided as only the filename, in which case
        // parent() returns an empty string.
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };

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
        if let Some(p) = result.shared_dir.as_mut() {
            p.after_deserialize(parent)?;
        }
        for d in result
            .scsi_targets
            .iter_mut()
            .chain(result.devices.iter_mut().map(|d| &mut d.device))
        {
            match d {
                WorkspaceDeviceKind::Disk(p)
                | WorkspaceDeviceKind::ReadOnlyDisk(p)
                | WorkspaceDeviceKind::CdromFolder(p) => {
                    p.after_deserialize(parent)?;
                }
                WorkspaceDeviceKind::None
                | WorkspaceDeviceKind::Cdrom
                | WorkspaceDeviceKind::Ethernet
                | WorkspaceDeviceKind::Printer => (),
            }
        }
        for (i, d) in result.disks.iter_mut().enumerate() {
            if let Some(p) = d.as_mut() {
                p.after_deserialize(parent)?;

                // Migrate
                result.scsi_targets[i] = WorkspaceDeviceKind::Disk(p.clone());
            }
        }
        result.disks = core::array::from_fn(|_| None);

        for (i, d) in result.scsi_targets.iter().enumerate() {
            if !matches!(d, WorkspaceDeviceKind::None) {
                result.devices.push(WorkspaceDevice {
                    family: "SCSI".to_owned(),
                    id: i.to_string(),
                    device: d.clone(),
                });
            }
        }
        result.scsi_targets = Default::default();

        for p in &mut result.floppy_images {
            p.after_deserialize(parent)?;
        }

        // Migrate old framebuffer positioning fields to new enum
        if let Some(v) = result.center_viewport_v {
            if v {
                result.framebuffer_mode = FramebufferMode::Centered;
            } else {
                result.framebuffer_mode = FramebufferMode::CenteredHorizontally;
            }
        }

        Ok(result)
    }

    pub fn write_file(&mut self, path: &Path) -> Result<()> {
        // Resolve relative paths
        let parent = path.parent().context("Cannot resolve parent path")?;
        // Default to the current working directory if the input path has no parent. It could be
        // relative to the current directory and provided as only the filename, in which case
        // parent() returns an empty string.
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };

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
        if let Some(p) = self.shared_dir.as_mut() {
            p.before_serialize(parent)?;
        }
        // disks and scsi_targets are deprecated
        for d in self.devices.iter_mut().map(|d| &mut d.device) {
            match d {
                WorkspaceDeviceKind::Disk(p)
                | WorkspaceDeviceKind::ReadOnlyDisk(p)
                | WorkspaceDeviceKind::CdromFolder(p) => {
                    p.before_serialize(parent)?;
                }
                WorkspaceDeviceKind::None
                | WorkspaceDeviceKind::Cdrom
                | WorkspaceDeviceKind::Ethernet
                | WorkspaceDeviceKind::Printer => (),
            }
        }
        for p in &mut self.floppy_images {
            p.before_serialize(parent)?;
        }

        let file = File::create(path)?;
        Ok(serde_json::to_writer_pretty(file, self)?)
    }

    pub fn saved_devices(&self) -> Vec<SavedDevice> {
        self.devices
            .iter()
            .filter_map(WorkspaceDevice::to_saved)
            .collect()
    }

    pub fn set_saved_devices(&mut self, saved: &[SavedDevice]) {
        self.devices = saved.iter().map(WorkspaceDevice::from_saved).collect();
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

    pub fn set_shared_dir(&mut self, p: Option<&Path>) {
        self.shared_dir = p.map(RelativePath::from_absolute);
    }

    pub fn get_shared_dir(&self) -> Option<PathBuf> {
        self.shared_dir.clone().map(|d| d.get_absolute())
    }

    pub fn get_floppy_images(&self) -> Vec<PathBuf> {
        self.floppy_images
            .iter()
            .map(|p| p.get_absolute())
            .collect()
    }

    #[cfg(feature = "ethernet")]
    pub fn get_ethernet_link_type(&self) -> EthernetLinkType {
        self.ethernet_link_type.clone().into()
    }

    #[cfg(feature = "ethernet")]
    pub fn set_ethernet_link_type(&mut self, ethernet_link_type: EthernetLinkType) {
        self.ethernet_link_type = ethernet_link_type.into();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_scsi_targets_become_devices() {
        let dir = std::env::temp_dir().join(format!("snow_ws_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        let image = dir.join("a.img");
        std::fs::write(&image, []).unwrap();
        let path = dir.join("old.snoww");
        std::fs::write(
            &path,
            r#"{"scsi_targets":[{"Disk":"a.img"},"None","Cdrom","None","None","None","Ethernet"]}"#,
        )
        .unwrap();

        let check = |workspace: &Workspace| {
            let saved = workspace.saved_devices();
            assert_eq!(saved[0].family, "SCSI");
            assert_eq!(saved[0].id, "0");
            assert!(matches!(&saved[0].device, NewDevice::HardDisk(p) if p == &image));
            assert_eq!(saved[1].id, "2");
            assert!(matches!(saved[1].device, NewDevice::CdromDrive));
            assert_eq!(saved[2].id, "6");
        };
        let mut workspace = Workspace::from_file(&path).unwrap();
        check(&workspace);

        // Written back as devices, and read again the same
        let path = dir.join("new.snoww");
        workspace.write_file(&path).unwrap();
        check(&Workspace::from_file(&path).unwrap());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
