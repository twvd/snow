use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use crate::emulator::EmulatorInitArgs;
use crate::joystick::JoyBindings;
use crate::shader_pipeline::ShaderConfig;
use crate::util::relativepath::RelativePath;
use crate::widgets::framebuffer::ScalingAlgorithm;
use anyhow::{Context, Result};
use eframe::egui;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use snow_core::emulator::devices::{
    AppleIIePdsCardSettings, DeviceConfig, DeviceModel, DisplayCard824Settings, LisaVideoSettings,
    bus_id,
};
use snow_core::mac::maclc::pds_iie::IieDiskDevice;
use snow_core::mac::serial_bridge::SerialBridgeConfig;
use snow_core::mac::swim::drive::DriveType;
use snow_core::mac::{MacModel, MacMonitor, NubusDeviceKind, PdsCardKind};

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

/// What older workspaces said was on SCSI
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub enum WorkspaceScsiTarget {
    #[default]
    None,
    Disk(PathBuf),
    Cdrom,
    CdromFolder(PathBuf),
    Ethernet,
    Printer,
}

/// Custom deserializer that skips entries that can't be used at all, which means they have no
/// bus or slot. An entry that merely can't be understood is kept: see DeviceConfig.
fn deserialize_devices_lenient<'de, D>(deserializer: D) -> Result<Vec<DeviceConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let values: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|value| match serde_json::from_value(value) {
            Ok(config) => Some(config),
            Err(e) => {
                log::warn!("Skipping invalid device in the workspace: {}", e);
                None
            }
        })
        .collect())
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

    /// Deprecated; now in devices
    #[serde(skip_serializing)]
    display_card_rom_path: Option<PathBuf>,

    /// Last specified PRAM path
    pram_path: Option<RelativePath>,

    /// Last specified extension ROM path
    extension_rom_path: Option<RelativePath>,

    /// Deprecated; now in devices
    #[serde(skip_serializing)]
    disks: [Option<PathBuf>; 7],

    /// Deprecated; now in devices
    #[serde(skip_serializing)]
    scsi_targets: [WorkspaceScsiTarget; 7],

    /// The devices in the machine, with absolute paths once loaded
    #[serde(deserialize_with = "deserialize_devices_lenient")]
    pub devices: Vec<DeviceConfig>,

    /// Entries the machine couldn't use. They stay in the file.
    #[serde(skip)]
    pub kept_devices: Vec<DeviceConfig>,

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

    /// Deprecated; now in devices
    #[serde(skip_serializing)]
    floppy_images: Vec<PathBuf>,

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

    /// Deprecated; now in devices
    #[serde(skip_serializing)]
    ethernet_link_type: Option<serde_json::Value>,

    /// Deprecated; now in devices
    #[serde(skip_serializing)]
    serial_bridges: [Option<SerialBridgeConfig>; 2],

    /// The Apple IIe joystick bindings; None to use those of the settings
    #[serde(deserialize_with = "deserialize_bindings_lenient")]
    pub iie_joystick_bindings: Option<JoyBindings>,
}

/// Custom deserializer for the joystick bindings: None (the settings'
/// bindings) if they cannot be parsed
fn deserialize_bindings_lenient<'de, D>(deserializer: D) -> Result<Option<JoyBindings>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_else(|e| {
        log::warn!(
            "Invalid Apple IIe joystick bindings in the workspace: {}",
            e
        );
        None
    }))
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
            kept_devices: Vec::new(),
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
            ethernet_link_type: None,
            serial_bridges: [None, None],
            iie_joystick_bindings: None,
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
        if let Some(p) = result.pram_path.as_mut() {
            p.after_deserialize(parent)?;
        }
        if let Some(p) = result.extension_rom_path.as_mut() {
            p.after_deserialize(parent)?;
        }
        if let Some(p) = result.shared_dir.as_mut() {
            p.after_deserialize(parent)?;
        }
        result.convert_old_fields();
        for config in &mut result.devices {
            absolute_paths(config, parent);
        }
        result.sync_args_from_devices();

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
        if let Some(p) = self.pram_path.as_mut() {
            p.before_serialize(parent)?;
        }
        if let Some(p) = self.extension_rom_path.as_mut() {
            p.before_serialize(parent)?;
        }
        if let Some(p) = self.shared_dir.as_mut() {
            p.before_serialize(parent)?;
        }
        // A workspace named by its bare file name has "." here, and paths aren't relative to that
        let base = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        let absolute = self.devices.clone();
        for config in &mut self.devices {
            relative_paths(config, &base);
        }
        let result = File::create(path)
            .map_err(anyhow::Error::from)
            .and_then(|file| Ok(serde_json::to_writer_pretty(file, self)?));
        self.devices = absolute;
        result
    }

    /// The devices of the machine are these, and what it couldn't use stays in the file. A live
    /// device wins over an entry for its slot.
    pub fn set_live_devices(&mut self, live: Vec<DeviceConfig>) {
        let kept = self
            .kept_devices
            .iter()
            .filter(|k| !live.iter().any(|d| d.bus == k.bus && d.slot == k.slot))
            .cloned()
            .collect::<Vec<_>>();
        self.devices = live;
        self.devices.extend(kept);
    }

    /// Turns the fields of older workspaces into entries. A file saved since has none of them.
    fn convert_old_fields(&mut self) {
        let mut targets = std::mem::take(&mut self.scsi_targets);
        for (i, d) in std::mem::take(&mut self.disks).into_iter().enumerate() {
            if let Some(p) = d {
                targets[i] = WorkspaceScsiTarget::Disk(p);
            }
        }
        for (i, target) in targets.into_iter().enumerate() {
            let id = i.to_string();
            let entry = |device, settings| old_entry(bus_id::SCSI, &id, Some(device), settings);
            match target {
                WorkspaceScsiTarget::None => continue,
                WorkspaceScsiTarget::Disk(p) => {
                    self.devices
                        .push(entry("ScsiHardDisk", json!({ "media": p })));
                }
                WorkspaceScsiTarget::Cdrom => {
                    self.devices.push(entry("ScsiCdrom", json!({})));
                }
                WorkspaceScsiTarget::CdromFolder(p) => {
                    self.devices.push(entry("ScsiCdrom", json!({ "media": p })));
                }
                WorkspaceScsiTarget::Ethernet => {
                    let settings = match &self.ethernet_link_type {
                        Some(link) => json!({ "link": link }),
                        None => json!({}),
                    };
                    self.devices.push(entry("DaynaPortScsiLink", settings));
                }
                WorkspaceScsiTarget::Printer => {
                    self.devices.push(entry("LaserWriterIISC", json!({})));
                }
            }
        }
        self.ethernet_link_type = None;

        // Only the first one was ever used
        if let Some(image) = std::mem::take(&mut self.floppy_images).into_iter().next() {
            self.devices.push(old_entry(
                bus_id::SWIM,
                "1",
                None,
                json!({ "media": image }),
            ));
        }

        for (slot, bridge) in ["A", "B"]
            .iter()
            .zip(std::mem::take(&mut self.serial_bridges))
        {
            let entry = |device, settings| old_entry(bus_id::SERIAL, slot, Some(device), settings);
            match bridge {
                None => (),
                Some(SerialBridgeConfig::Pty) => {
                    self.devices.push(entry("SerialBridge", json!({})));
                }
                Some(SerialBridgeConfig::Tcp(port)) => {
                    self.devices
                        .push(entry("SerialBridge", json!({ "tcp_port": port })));
                }
                Some(SerialBridgeConfig::LocalTalk) => {
                    self.devices.push(entry("LocalTalkAdapter", json!({})));
                }
            }
        }

        let model = self.model.or_else(|| self.detect_model());
        let mut video = vec![];
        if let Some(rom) = self.display_card_rom_path.take() {
            video.push(if model == Some(MacModel::SE30) {
                VideoCardEntry {
                    slot: 0xE,
                    kind: NubusDeviceKind::SE30Video,
                    rom,
                    monitor: None,
                }
            } else if model.is_some_and(MacModel::is_lisa) {
                VideoCardEntry {
                    slot: 0,
                    kind: NubusDeviceKind::LisaVideo,
                    rom,
                    monitor: None,
                }
            } else {
                VideoCardEntry {
                    slot: self.init_args.video_card_slot,
                    kind: self.init_args.video_card,
                    rom,
                    monitor: Some(self.init_args.monitor.unwrap_or_default()),
                }
            });
        }
        self.devices
            .extend(card_entries(model, &video, &self.init_args));

        // The monitor of an 8-24 used to be the machine's
        let monitor = self.init_args.monitor.unwrap_or_default();
        for config in &mut self.devices {
            if config.known_model() == Some(DeviceModel::DisplayCard824)
                && !config.settings.contains_key("monitor")
            {
                config.settings.insert("monitor".to_owned(), json!(monitor));
            }
        }
    }

    /// The model of an older workspace that doesn't say, to know where its video card went
    fn detect_model(&self) -> Option<MacModel> {
        let rom = std::fs::read(self.rom_path.as_ref()?.get_absolute()).ok()?;
        MacModel::detect_from_rom(&rom)
    }

    /// The Load ROM dialog still deals in these
    fn sync_args_from_devices(&mut self) {
        let args = &mut self.init_args;
        for config in &self.devices {
            match config.known_model() {
                Some(DeviceModel::AppleIIePdsCard) => {
                    args.pds_card = PdsCardKind::AppleIIe;
                    if let Ok(settings) = config.parse::<AppleIIePdsCardSettings>() {
                        args.iie_card_settings.security_jumper = settings.security_jumper;
                    }
                    args.iie_card_settings.disk_chain = [IieDiskDevice::None; 2];
                }
                Some(DeviceModel::UniDisk35) if config.slot == "1" => {
                    args.iie_card_settings.disk_chain[0] = IieDiskDevice::UniDisk35;
                }
                Some(DeviceModel::Apple525Drive) => {
                    let position = usize::from(config.slot == "2");
                    args.iie_card_settings.disk_chain[position] = IieDiskDevice::Apple525;
                }
                Some(model) if config.bus == bus_id::SWIM => {
                    args.override_fdd_type = model.drive_type();
                }
                // What the Load ROM dialog's 800K option gives
                Some(DeviceModel::Lisa800KDrive) => {
                    args.override_fdd_type = Some(DriveType::GCR800KPWM);
                }
                Some(DeviceModel::Pfg) => args.lisa_pfg = true,
                // The PROM file is found with the cards' ROMs
                _ if self.model.and_then(|mac| config.model_on(mac))
                    == Some(DeviceModel::LisaVideo) =>
                {
                    if let Ok(video) = config.parse::<LisaVideoSettings>() {
                        args.lisa_video = LisaVideoSettings { rom: None, ..video };
                    }
                }
                _ => (),
            }
        }
    }

    /// The machine's video cards, or the SE/30's video, for the Load ROM dialog
    pub fn video_cards(&self) -> Vec<VideoCardEntry> {
        let Some(mac) = self.model else {
            return vec![];
        };
        self.devices
            .iter()
            .filter_map(|config| video_card(config, mac))
            .collect()
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

fn old_entry(
    bus: &str,
    slot: &str,
    device: Option<&str>,
    settings: serde_json::Value,
) -> DeviceConfig {
    DeviceConfig {
        bus: bus.to_owned(),
        slot: slot.to_owned(),
        model: device.map(str::to_owned),
        settings: match settings {
            serde_json::Value::Object(map) => map,
            _ => Default::default(),
        },
    }
}

/// A video card in a NuBus slot, or the SE/30's video in its slot $E, as the Load ROM dialog
/// chooses them
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoCardEntry {
    pub slot: u8,
    pub kind: NubusDeviceKind,
    pub rom: PathBuf,
    /// What's on an 8-24's connector, None for nothing
    pub monitor: Option<MacMonitor>,
}

/// The entries for what the Load ROM dialog chooses: the cards, and the kind of floppy drive
pub fn card_entries(
    model: Option<MacModel>,
    video_cards: &[VideoCardEntry],
    args: &EmulatorInitArgs,
) -> Vec<DeviceConfig> {
    let mut entries = vec![];
    for card in video_cards {
        let slot = format!("{:X}", card.slot);
        entries.push(match card.kind {
            NubusDeviceKind::SE30Video => old_entry(
                bus_id::BUILT_IN,
                "0",
                Some(DeviceModel::SE30Video.key()),
                json!({ "rom": card.rom }),
            ),
            NubusDeviceKind::Toby => old_entry(
                bus_id::NUBUS,
                &slot,
                Some(DeviceModel::MacIIVideoCard.key()),
                json!({ "rom": card.rom }),
            ),
            NubusDeviceKind::Mdc12 => old_entry(
                bus_id::NUBUS,
                &slot,
                Some(DeviceModel::DisplayCard824.key()),
                json!({ "rom": card.rom, "monitor": card.monitor }),
            ),
            NubusDeviceKind::LisaVideo => {
                // Without a PROM file, the PROM is built in from the other settings
                let video = LisaVideoSettings {
                    rom: (!card.rom.as_os_str().is_empty()).then(|| card.rom.clone()),
                    ..args.lisa_video.clone()
                };
                if video == LisaVideoSettings::default() {
                    continue;
                }
                old_entry(
                    bus_id::BUILT_IN,
                    "0",
                    Some(DeviceModel::LisaVideo.key()),
                    json!(video),
                )
            }
        });
    }
    if args.pds_card == PdsCardKind::AppleIIe {
        let settings = &args.iie_card_settings;
        entries.push(old_entry(
            bus_id::PDS_LC,
            "0",
            Some(DeviceModel::AppleIIePdsCard.key()),
            json!({ "security_jumper": settings.security_jumper }),
        ));
        for (i, device) in settings.disk_chain.iter().enumerate() {
            let model = match device {
                IieDiskDevice::None => continue,
                IieDiskDevice::Apple525 => DeviceModel::Apple525Drive,
                IieDiskDevice::UniDisk35 => DeviceModel::UniDisk35,
            };
            entries.push(old_entry(
                bus_id::IIE_DISK,
                &(i + 1).to_string(),
                Some(model.key()),
                json!({}),
            ));
        }
    }
    if model.is_some_and(MacModel::is_lisa) {
        if args.override_fdd_type.is_some() {
            let drive = DeviceModel::Lisa800KDrive.key();
            entries.push(old_entry(bus_id::LISA_FDC, "1", Some(drive), json!({})));
        }
        if args.lisa_pfg {
            let pfg = DeviceModel::Pfg.key();
            entries.push(old_entry(bus_id::BUILT_IN, "1", Some(pfg), json!({})));
        }
    } else if let Some(drive) = args
        .override_fdd_type
        .and_then(DeviceModel::from_drive_type)
    {
        entries.push(old_entry(bus_id::SWIM, "1", Some(drive.key()), json!({})));
    }
    entries
}

/// A video card, or the machine's own video, as the Load ROM dialog has it
fn video_card(config: &DeviceConfig, mac: MacModel) -> Option<VideoCardEntry> {
    let rom = PathBuf::from(config.settings.get("rom")?.as_str()?);
    let slot = u8::from_str_radix(&config.slot, 16).ok();
    match config.model_on(mac)? {
        DeviceModel::SE30Video => Some(VideoCardEntry {
            slot: 0xE,
            kind: NubusDeviceKind::SE30Video,
            rom,
            monitor: None,
        }),
        DeviceModel::LisaVideo => Some(VideoCardEntry {
            slot: 0,
            kind: NubusDeviceKind::LisaVideo,
            rom,
            monitor: None,
        }),
        DeviceModel::MacIIVideoCard => Some(VideoCardEntry {
            slot: slot?,
            kind: NubusDeviceKind::Toby,
            rom,
            monitor: None,
        }),
        DeviceModel::DisplayCard824 => Some(VideoCardEntry {
            slot: slot?,
            kind: NubusDeviceKind::Mdc12,
            rom,
            monitor: config.parse::<DisplayCard824Settings>().ok()?.monitor,
        }),
        _ => None,
    }
}

fn map_paths(config: &mut DeviceConfig, f: impl Fn(&Path) -> PathBuf) {
    for key in config.path_keys() {
        if let Some(value) = config.settings.get_mut(*key)
            && let Some(path) = value.as_str()
        {
            *value = json!(f(Path::new(path)));
        }
    }
}

/// A file that's missing now can come back, so this is not an error
fn absolute_paths(config: &mut DeviceConfig, basedir: &Path) {
    map_paths(config, |path| {
        let joined = basedir.join(path);
        std::fs::canonicalize(&joined).unwrap_or(joined)
    });
}

fn relative_paths(config: &mut DeviceConfig, basedir: &Path) {
    map_paths(config, |path| {
        pathdiff::diff_paths(path, basedir).unwrap_or_else(|| path.to_path_buf())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("snow_ws_{}_{}", name, std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(std::fs::canonicalize(dir).unwrap())
        }

        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, []).unwrap();
            path
        }

        fn load(&self, json: &str) -> Workspace {
            let path = self.0.join("t.snoww");
            std::fs::write(&path, json).unwrap();
            Workspace::from_file(&path).unwrap()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry(ws: &Workspace, bus: &str, slot: &str) -> DeviceConfig {
        ws.devices
            .iter()
            .find(|d| d.bus == bus && d.slot == slot)
            .unwrap_or_else(|| panic!("No {} #{} in {:#?}", bus, slot, ws.devices))
            .clone()
    }

    #[test]
    fn old_fields_become_entries() {
        let dir = TestDir::new("old");
        let image = dir.file("a.img");
        let folder = dir.0.join("cds");
        std::fs::create_dir_all(&folder).unwrap();
        let card = dir.file("card.rom");
        let floppy = dir.file("f.img");
        let ws = dir.load(
            r#"{
            "scsi_targets": [{"Disk":"a.img"},"Cdrom",{"CdromFolder":"cds"},"Ethernet","Printer","None","None"],
            "ethernet_link_type": "NAT",
            "floppy_images": ["f.img", "ignored.img"],
            "serial_bridges": [{"Tcp":5000}, "LocalTalk"],
            "display_card_rom_path": "card.rom",
            "model": "MacII",
            "init_args": { "video_card": "Toby" }
        }"#,
        );
        assert_eq!(entry(&ws, "SCSI", "0").settings["media"], json!(image));
        assert_eq!(entry(&ws, "SCSI", "1").model.as_deref(), Some("ScsiCdrom"));
        assert_eq!(entry(&ws, "SCSI", "2").settings["media"], json!(folder));
        assert_eq!(entry(&ws, "SCSI", "3").settings["link"], json!("NAT"));
        assert_eq!(
            entry(&ws, "SCSI", "4").model.as_deref(),
            Some("LaserWriterIISC")
        );
        assert_eq!(entry(&ws, "SWIM", "1").settings["media"], json!(floppy));
        assert_eq!(entry(&ws, "SWIM", "1").model, None);
        assert_eq!(entry(&ws, "Serial", "A").settings["tcp_port"], json!(5000));
        assert_eq!(
            entry(&ws, "Serial", "B").model.as_deref(),
            Some("LocalTalkAdapter")
        );
        let video = entry(&ws, "NuBus", "9");
        assert_eq!(video.model.as_deref(), Some("MacIIVideoCard"));
        assert_eq!(video.settings["rom"], json!(card));
        assert_eq!(ws.devices.len(), 9);
    }

    #[test]
    fn the_se30_video_is_converted_too() {
        let dir = TestDir::new("se30");
        let rom = dir.file("v.rom");
        let ws = dir.load(r#"{"model":"SE30","display_card_rom_path":"v.rom"}"#);
        assert_eq!(entry(&ws, "Built-in", "0").settings["rom"], json!(rom));
        assert_eq!(ws.video_cards()[0].rom, rom);
    }

    #[test]
    fn the_monitor_of_the_machine_goes_to_its_824() {
        let dir = TestDir::new("824");
        let rom = dir.file("card.rom");
        let ws = dir.load(
            r#"{"model":"MacIIx","display_card_rom_path":"card.rom",
                "init_args":{"monitor":"RGB12","video_card":"Mdc12","video_card_slot":10}}"#,
        );
        assert_eq!(
            ws.video_cards(),
            [VideoCardEntry {
                slot: 0xA,
                kind: NubusDeviceKind::Mdc12,
                rom: rom.clone(),
                monitor: Some(MacMonitor::RGB12),
            }]
        );

        // Saved before a card had its own
        let ws = dir.load(&format!(
            r#"{{"model":"MacIIx","init_args":{{"monitor":"RGB16"}},
                "devices":[{{"bus":"NuBus","slot":"9","device":"DisplayCard824","rom":{}}}]}}"#,
            json!(rom)
        ));
        assert_eq!(ws.video_cards()[0].monitor, Some(MacMonitor::RGB16));
    }

    #[test]
    fn the_lisa_video_comes_back_to_the_dialog() {
        let dir = TestDir::new("lisa");
        let rom = dir.file("p.rom");
        let ws = dir.load(&format!(
            r#"{{"model":"Lisa25","devices":[{{"bus":"Built-in","slot":"0","rom":{},
                "screen_kit":true,"serial":"12345678901234567890"}}]}}"#,
            json!(rom)
        ));
        let video = &ws.init_args.lisa_video;
        assert!(video.screen_kit);
        assert_eq!(video.serial.as_deref(), Some("12345678901234567890"));
        let cards = ws.video_cards();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, NubusDeviceKind::LisaVideo);
        assert_eq!(cards[0].rom, rom);

        // And what the dialog chooses goes back the same
        let entries = card_entries(Some(MacModel::Lisa25), &cards, &ws.init_args);
        assert_eq!(entries.len(), 1);
        let settings = &entries[0].settings;
        assert_eq!(settings["rom"], json!(rom));
        assert_eq!(settings["screen_kit"], json!(true));
        assert_eq!(settings["serial"], json!("12345678901234567890"));
        // Nothing chosen, nothing to say: no PROM file, the defaults
        let built_in = VideoCardEntry {
            rom: PathBuf::new(),
            ..cards[0].clone()
        };
        let args = EmulatorInitArgs::default();
        assert!(card_entries(Some(MacModel::Lisa25), &[built_in], &args).is_empty());
    }

    #[test]
    fn the_rom_of_the_machines_own_video_is_found() {
        // Saved since: the entry is for what the machine has in the slot, so names no model
        let dir = TestDir::new("se30_saved");
        let rom = dir.file("v.rom");
        let ws = dir.load(&format!(
            r#"{{"model":"SE30","devices":[{{"bus":"Built-in","slot":"0","rom":{}}}]}}"#,
            json!(rom)
        ));
        assert_eq!(ws.video_cards()[0].rom, rom);
    }

    #[test]
    fn the_iie_card_is_converted_and_read_back() {
        let dir = TestDir::new("iie");
        let ws = dir.load(
            r#"{"model":"LC","init_args":{"pds_card":"AppleIIe",
                "iie_card_settings":{"security_jumper":true,"disk_chain":["UniDisk35","Apple525"]}}}"#,
        );
        assert_eq!(
            entry(&ws, "PDS_LC", "0").settings["security_jumper"],
            json!(true)
        );
        assert_eq!(
            entry(&ws, "Floppy:PDS_LC.0", "1").model.as_deref(),
            Some("UniDisk35")
        );
        assert_eq!(
            entry(&ws, "Floppy:PDS_LC.0", "2").model.as_deref(),
            Some("Apple525Drive")
        );

        // Saved, the entries are all there is, and the dialog still gets its settings
        let mut saved = ws.clone();
        let path = dir.0.join("saved.snoww");
        saved.write_file(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("pds_card"), "{}", text);
        let again = Workspace::from_file(&path).unwrap();
        assert_eq!(again.devices, ws.devices);
        assert_eq!(again.init_args.pds_card, PdsCardKind::AppleIIe);
        assert!(again.init_args.iie_card_settings.security_jumper);
        assert_eq!(
            again.init_args.iie_card_settings.disk_chain,
            [IieDiskDevice::UniDisk35, IieDiskDevice::Apple525]
        );
    }

    #[test]
    fn paths_are_relative_in_the_file_and_absolute_in_memory() {
        let dir = TestDir::new("paths");
        let image = dir.file("a.img");
        let mut ws = dir.load(
            r#"{"devices":[{"bus":"SCSI","slot":"0","device":"ScsiHardDisk","media":"a.img"}]}"#,
        );
        assert_eq!(ws.devices[0].settings["media"], json!(image));

        let path = dir.0.join("saved.snoww");
        ws.write_file(&path).unwrap();
        let text: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(text["devices"][0]["media"], json!("a.img"));
        assert_eq!(ws.devices[0].settings["media"], json!(image));
    }

    #[test]
    fn a_missing_file_or_an_unknown_device_does_not_cost_the_workspace() {
        let dir = TestDir::new("forgiving");
        let ws = dir.load(
            r#"{"devices":[
                {"bus":"SCSI","slot":"0","device":"ScsiHardDisk","media":"gone.img"},
                {"bus":"NuBus","slot":"A","device":"SomeFutureCard","rom":"x.rom","fast":true},
                {"slot":"1"}
            ],"nubus_slots":[1,2]}"#,
        );
        assert_eq!(ws.devices.len(), 2);
        assert_eq!(
            ws.devices[1].settings["rom"],
            json!("x.rom"),
            "Not a path to this build"
        );
    }

    #[test]
    fn a_live_device_wins_over_what_was_kept() {
        let mut ws = Workspace::default();
        let entry = |slot: &str, device: &str| old_entry("SCSI", slot, Some(device), json!({}));
        ws.kept_devices = vec![entry("0", "SomeFutureDisk"), entry("1", "SomeFutureDisk")];
        ws.set_live_devices(vec![entry("0", "ScsiCdrom")]);
        assert_eq!(
            ws.devices,
            vec![entry("0", "ScsiCdrom"), entry("1", "SomeFutureDisk")]
        );
    }

    /// A machine created from a workspace saves what it was given, and what it couldn't use
    #[test]
    fn a_workspace_survives_a_machine() {
        use snow_core::emulator::Emulator;
        use snow_core::emulator::MouseMode;
        use snow_core::emulator::devices::MachineConfig;

        let dir = TestDir::new("machine");
        std::fs::write(dir.0.join("a.img"), vec![0u8; 1024 * 1024]).unwrap();
        std::fs::write(dir.0.join("f.img"), vec![0u8; 800 * 1024]).unwrap();
        let mut ws = dir.load(
            r#"{"model":"Plus","devices":[
                {"bus":"SCSI","slot":"0","device":"ScsiHardDisk","media":"a.img"},
                {"bus":"SCSI","slot":"3","device":"ScsiCdrom"},
                {"bus":"SCSI","slot":"5","device":"SomeFutureDisk","media":"x.img"},
                {"bus":"SCSI","slot":"6","device":"ScsiHardDisk","media":"gone.img"},
                {"bus":"NuBus","slot":"9","device":"DisplayCard824","rom":"card.rom"},
                {"bus":"SWIM","slot":"1","media":"f.img","read_only":true}
            ]}"#,
        );
        let rom = vec![0; 128 * 1024];
        let (mut emu, _, refused) = Emulator::new_with_devices(
            &MachineConfig {
                rom: &rom,
                model: MacModel::Plus,
                extension_rom: None,
                monitor: None,
                mouse_mode: MouseMode::default(),
                ram_size: None,
                pmmu_enabled: false,
                fpu_enabled: false,
                vram_size: None,
            },
            ws.devices.clone(),
        )
        .unwrap();
        assert_eq!(refused.len(), 3, "{:#?}", refused);
        ws.kept_devices = refused.into_iter().map(|r| r.config).collect();
        ws.set_live_devices(emu.devices());

        let path = dir.0.join("saved.snoww");
        ws.write_file(&path).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut entries = saved["devices"].as_array().unwrap().clone();
        entries.sort_by_key(|e| format!("{}{}", e["bus"], e["slot"]));
        assert_eq!(
            entries,
            vec![
                json!({"bus":"NuBus","slot":"9","device":"DisplayCard824","rom":"card.rom",
                       "monitor":"HiRes14"}),
                json!({"bus":"SCSI","slot":"0","device":"ScsiHardDisk","media":"a.img"}),
                json!({"bus":"SCSI","slot":"3","device":"ScsiCdrom"}),
                json!({"bus":"SCSI","slot":"5","device":"SomeFutureDisk","media":"x.img"}),
                json!({"bus":"SCSI","slot":"6","device":"ScsiHardDisk","media":"gone.img"}),
                json!({"bus":"SWIM","slot":"1","media":"f.img","read_only":true}),
            ]
        );
    }
}
