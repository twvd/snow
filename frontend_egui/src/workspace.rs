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
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
#[cfg(feature = "ethernet")]
use snow_core::devices::EthernetConfig;
use snow_core::devices::EthernetLinkType;
use snow_core::devices::NatConfig;
#[cfg(feature = "printer")]
use snow_core::devices::PrinterConfig;
use snow_core::devices::{
    CdromConfig, DeviceConfig, DeviceModel, DisplayConfig, FunctionConfig, HardDiskConfig,
    built_in, bus_id,
};
use snow_core::mac::maclc::pds_iie::IieCardSettings;
use snow_core::mac::serial_bridge::SerialBridgeConfig;
use snow_core::mac::swim::drive::DriveType;
use snow_core::mac::{MacModel, MacMonitor};

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

/// A value of the file that is read on its own: one that can't be read doesn't cost the rest
/// of the file
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
enum Lenient<T> {
    Read(T),
    Unread(serde_json::Value),
}

/// A SCSI target of an older workspace. Read, never written.
#[derive(Deserialize, Debug, Clone)]
enum OldScsiTarget {
    None,
    Disk(PathBuf),
    Cdrom,
    /// Folder-backed ("Toolbox managed") CD-ROM: the guest swaps between the
    /// images in this folder via the BlueSCSI Toolbox CD commands.
    CdromFolder(PathBuf),
    Ethernet,
    Printer,
}

/// The video card of an older workspace, and of older settings
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NubusDeviceKind {
    /// Macintosh Display Card 8-24
    // The card of the workspaces from before there was a choice
    #[default]
    Mdc12,
    /// Macintosh II Video Card
    Toby,
    /// The video of an SE/30, in older settings
    SE30Video,
}

impl NubusDeviceKind {
    /// The device model of this kind of card
    pub const fn device_model(self) -> DeviceModel {
        match self {
            Self::Mdc12 => DeviceModel::DisplayCard824,
            Self::Toby => DeviceModel::MacIIVideoCard,
            Self::SE30Video => DeviceModel::SE30Video,
        }
    }
}

/// The card in the PDS slot of an older workspace
#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
enum PdsCardKind {
    #[default]
    None,
    AppleIIe,
}

/// What an older workspace says of the devices of its machine, among the arguments of the
/// Load ROM dialog (`init_args`). Read, never written: a workspace saved since has them as
/// device configs.
#[derive(Debug, Default)]
struct OldInitArgs {
    monitor: Option<MacMonitor>,
    vram_size: Option<usize>,
    video_card: NubusDeviceKind,
    pds_card: PdsCardKind,
    iie_card_settings: IieCardSettings,
    override_fdd_type: Option<DriveType>,
}

impl OldInitArgs {
    /// The devices that these fields describe, as device configs: the video card of a
    /// Macintosh II, the ROM of the video of an SE/30, what was chosen for a built-in video,
    /// the card of the processor-direct slot, and floppy drives of another type than the Mac
    /// model's. `display_rom` is the workspace's ROM file of the video card, or of the video
    /// of an SE/30. A workspace saved since has none of this, and gets no device config.
    fn device_configs(&self, model: MacModel, display_rom: Option<&Path>) -> Vec<DeviceConfig> {
        let buses = model.buses();
        let bus = |id: &str| buses.iter().find(|bus| bus.id == id);
        let mut configs = vec![];

        if let Some(nubus) = bus(bus_id::NUBUS) {
            // The video card, in the first slot
            if let Some(rom) = display_rom {
                configs.push(DeviceConfig {
                    bus: nubus.id.clone(),
                    slot: nubus.slots[0].id.clone(),
                    model: Some(self.video_card.device_model()),
                    rom: Some(rom.to_path_buf()),
                    function: FunctionConfig::Display(DisplayConfig {
                        monitor: self.monitor,
                        vram_size: None,
                    }),
                });
            }
        } else if bus(bus_id::BUILT_IN).is_some()
            && (display_rom.is_some() || self.monitor.is_some() || self.vram_size.is_some())
        {
            configs.push(DeviceConfig {
                bus: bus_id::BUILT_IN.into(),
                slot: built_in::VIDEO.into(),
                model: None,
                rom: display_rom.map(Path::to_path_buf),
                function: FunctionConfig::Display(DisplayConfig {
                    monitor: self.monitor,
                    vram_size: self.vram_size,
                }),
            });
        }

        if self.pds_card == PdsCardKind::AppleIIe
            && let Some(pds) = bus(bus_id::PDS_LC)
        {
            configs.push(DeviceConfig {
                bus: pds.id.clone(),
                slot: pds.slots[0].id.clone(),
                model: Some(DeviceModel::AppleIIePdsCard),
                rom: None,
                function: FunctionConfig::AppleIIeCard(self.iie_card_settings),
            });
        }

        if let Some(drive_type) = self.override_fdd_type
            && let Some(swim) = bus(bus_id::SWIM)
        {
            for slot in &swim.slots {
                configs.push(DeviceConfig {
                    bus: swim.id.clone(),
                    slot: slot.id.clone(),
                    model: Some(drive_type.device_model()),
                    rom: None,
                    function: FunctionConfig::Floppy,
                });
            }
        }
        configs
    }
}

/// One field of an older workspace, read on its own: its default if the file doesn't have it,
/// or if it can't be read, which is noted for the user
fn old_field<T: DeserializeOwned + Default>(
    object: &serde_json::Value,
    key: &str,
    what: &str,
    unread: &mut Vec<String>,
) -> T {
    let Some(value) = object.get(key) else {
        return T::default();
    };
    serde_json::from_value(value.clone()).unwrap_or_else(|_| {
        unread.push(format!("{} can't be read ({})", what, value));
        T::default()
    })
}

/// What can't be read of the file, for the user: the entry of a list, by its place in it
fn unread_entry(list: &str, index: usize, value: &serde_json::Value) -> String {
    format!("{} #{}: can't be read ({})", list, index, value)
}

/// A path of the workspace file, which is relative to the folder of the file
fn absolute_path(parent: &Path, path: &Path) -> PathBuf {
    let joined = parent.join(path);
    joined.canonicalize().unwrap_or(joined)
}

fn relative_path(parent: &Path, path: &Path) -> PathBuf {
    pathdiff::diff_paths(path, parent).unwrap_or_else(|| {
        log::info!(
            "Cannot get relative path for {}, saving absolute path",
            path.display()
        );
        path.to_path_buf()
    })
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
    #[serde(skip_serializing)]
    display_card_rom_path: Option<RelativePath>,

    /// Last specified PRAM path
    pram_path: Option<RelativePath>,

    /// Last specified extension ROM path
    extension_rom_path: Option<RelativePath>,

    /// Last loaded disks
    /// The devices of the machine, as device configs. Their paths are absolute here, and
    /// relative to the workspace file in the file.
    devices: Vec<Lenient<DeviceConfig>>,

    /// What the file had that couldn't be read, for the user
    #[serde(skip)]
    unread: Vec<String>,

    /// Deprecated; now devices
    #[serde(skip_serializing)]
    disks: Vec<Lenient<Option<PathBuf>>>,

    /// Deprecated; now devices
    #[serde(skip_serializing)]
    scsi_targets: Vec<Lenient<OldScsiTarget>>,

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

    /// Deprecated; now the link of each Ethernet adapter in devices
    #[serde(skip_serializing)]
    ethernet_link_type: Lenient<EthernetLinkType>,

    /// Serial bridges per SCC channel (index 0 = Channel A, index 1 = Channel B)
    pub serial_bridges: [Option<SerialBridgeConfig>; 2],

    /// The Apple IIe joystick bindings; None to use those of the settings
    #[serde(deserialize_with = "deserialize_bindings_lenient")]
    pub iie_joystick_bindings: Option<JoyBindings>,

    /// The network of the NAT gateway; None to use the default of the settings
    pub nat_network: Option<NatConfig>,

    /// The folder shared by the built-in AppleTalk file server
    #[cfg(feature = "afp_server")]
    pub afp_shared_dir: Option<RelativePath>,

    /// The volume name of the built-in AppleTalk file server
    #[cfg(feature = "afp_server")]
    #[serde(default = "default_afp_volume_name")]
    pub afp_volume_name: String,

    /// The highest AFP version the server advertises
    #[cfg(feature = "afp_server")]
    pub afp_max_version: snow_core::mac::serial_bridge::AfpMaxVersion,

    /// The advanced settings of the built-in AppleTalk file server
    #[cfg(feature = "afp_server")]
    #[serde(default)]
    pub afp_settings: snow_core::mac::serial_bridge::AfpSettings,
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

/// The default volume name of the built-in AppleTalk file server
#[cfg(feature = "afp_server")]
fn default_afp_volume_name() -> String {
    "Snow".to_string()
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
            devices: Vec::new(),
            unread: Vec::new(),
            disks: Vec::new(),
            scsi_targets: Vec::new(),
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
            ethernet_link_type: Lenient::Read(EthernetLinkType::default()),
            serial_bridges: [None, None],
            iie_joystick_bindings: None,
            nat_network: None,
            #[cfg(feature = "afp_server")]
            afp_shared_dir: None,
            #[cfg(feature = "afp_server")]
            afp_volume_name: "Snow".to_string(),
            #[cfg(feature = "afp_server")]
            afp_max_version: Default::default(),
            #[cfg(feature = "afp_server")]
            afp_settings: Default::default(),
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

        // The file as it is, for the fields of older workspaces that are read one by one
        let file: serde_json::Value = serde_json::from_reader(reader)?;
        let mut result: Self = serde_json::from_value(file.clone())?;

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
        // Workspaces saved before the shared folder was written out hold an
        // empty path: they load with no folder chosen
        #[cfg(feature = "afp_server")]
        result.afp_shared_dir.take_if(|p| p.is_empty());
        #[cfg(feature = "afp_server")]
        if let Some(p) = result.afp_shared_dir.as_mut() {
            p.after_deserialize(parent)?;
        }
        let args = &file["init_args"];
        let unread = &mut result.unread;
        let old = OldInitArgs {
            monitor: old_field(args, "monitor", "The monitor", unread),
            vram_size: old_field(args, "vram_size", "The video RAM size", unread),
            video_card: old_field(args, "video_card", "The video card", unread),
            pds_card: old_field(args, "pds_card", "The PDS card", unread),
            iie_card_settings: old_field(
                args,
                "iie_card_settings",
                "The settings of the Apple IIe Card",
                unread,
            ),
            override_fdd_type: old_field(
                args,
                "override_fdd_type",
                "The type of the floppy drives",
                unread,
            ),
        };
        result.read_devices(parent, &old);

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
        if let Some(p) = self.pram_path.as_mut() {
            p.before_serialize(parent)?;
        }
        if let Some(p) = self.extension_rom_path.as_mut() {
            p.before_serialize(parent)?;
        }
        if let Some(p) = self.shared_dir.as_mut() {
            p.before_serialize(parent)?;
        }
        #[cfg(feature = "afp_server")]
        if let Some(p) = self.afp_shared_dir.as_mut() {
            p.before_serialize(parent)?;
        }
        for p in &mut self.floppy_images {
            p.before_serialize(parent)?;
        }

        // The paths of the devices are relative in the file
        let devices = self.devices.clone();
        for device in &mut self.devices {
            if let Lenient::Read(config) = device {
                config.convert_paths(|path| relative_path(parent, path));
            }
        }
        let written = File::create(path)
            .map_err(anyhow::Error::from)
            .and_then(|file| Ok(serde_json::to_writer_pretty(file, self)?));
        self.devices = devices;
        written
    }

    /// Makes the list of devices of a workspace that was just read: the device configs of the
    /// file that can be read, then those made from the fields of older workspaces, with their
    /// paths made absolute. What can't be read is noted for the user.
    fn read_devices(&mut self, parent: &Path, old: &OldInitArgs) {
        let mut devices = vec![];
        for (i, device) in std::mem::take(&mut self.devices).into_iter().enumerate() {
            match device {
                Lenient::Read(config) => devices.push(config),
                Lenient::Unread(value) => self.unread.push(unread_entry("Device", i, &value)),
            }
        }
        self.convert_old_fields(&mut devices, old);
        for config in &mut devices {
            config.convert_paths(|path| absolute_path(parent, path));
        }
        self.devices = devices.into_iter().map(Lenient::Read).collect();
    }

    /// Turns the fields of older workspaces into device configs. A file saved since has none
    /// of them. The id of a SCSI slot is the SCSI ID, in decimal.
    fn convert_old_fields(&mut self, devices: &mut Vec<DeviceConfig>, old: &OldInitArgs) {
        // The targets, by SCSI ID
        let mut targets = vec![];
        for (id, target) in std::mem::take(&mut self.scsi_targets)
            .into_iter()
            .enumerate()
        {
            targets.push(match target {
                Lenient::Read(target) => target,
                Lenient::Unread(value) => {
                    self.unread.push(unread_entry("SCSI", id, &value));
                    OldScsiTarget::None
                }
            });
        }
        // `disks` came before `scsi_targets`, and only had the images of hard disks
        for (id, disk) in std::mem::take(&mut self.disks).into_iter().enumerate() {
            let disk = match disk {
                Lenient::Read(Some(path)) => OldScsiTarget::Disk(path),
                Lenient::Read(None) => continue,
                Lenient::Unread(value) => {
                    self.unread.push(unread_entry("SCSI", id, &value));
                    continue;
                }
            };
            if targets.len() <= id {
                targets.resize(id + 1, OldScsiTarget::None);
            }
            targets[id] = disk;
        }

        #[cfg(feature = "ethernet")]
        let link = match &self.ethernet_link_type {
            Lenient::Read(link) => link.clone(),
            Lenient::Unread(value) => {
                self.unread
                    .push(format!("The Ethernet link can't be read ({})", value));
                EthernetLinkType::Down
            }
        };
        for (id, target) in targets.into_iter().enumerate() {
            let (model, function) = match target {
                OldScsiTarget::None => continue,
                OldScsiTarget::Disk(path) => (
                    DeviceModel::ScsiHardDisk,
                    FunctionConfig::HardDisk(HardDiskConfig { media: Some(path) }),
                ),
                OldScsiTarget::Cdrom => (
                    DeviceModel::ScsiCdrom,
                    FunctionConfig::Cdrom(CdromConfig::default()),
                ),
                OldScsiTarget::CdromFolder(path) => (
                    DeviceModel::ScsiCdrom,
                    FunctionConfig::Cdrom(CdromConfig { folder: Some(path) }),
                ),
                #[cfg(feature = "ethernet")]
                // The workspace kept no address: the adapter gets a random one
                OldScsiTarget::Ethernet => (
                    DeviceModel::DaynaPortScsiLink,
                    FunctionConfig::Ethernet(EthernetConfig {
                        mac: None,
                        link: link.clone(),
                    }),
                ),
                #[cfg(feature = "printer")]
                OldScsiTarget::Printer => (
                    DeviceModel::LaserWriterIISC,
                    FunctionConfig::Printer(PrinterConfig::default()),
                ),
                // A device this build doesn't have
                #[cfg(not(feature = "ethernet"))]
                OldScsiTarget::Ethernet => {
                    self.unread
                        .push(format!("SCSI #{}: this build has no Ethernet adapter", id));
                    continue;
                }
                #[cfg(not(feature = "printer"))]
                OldScsiTarget::Printer => {
                    self.unread
                        .push(format!("SCSI #{}: this build has no printer", id));
                    continue;
                }
            };
            devices.push(DeviceConfig {
                bus: bus_id::SCSI.into(),
                slot: id.to_string(),
                model: Some(model),
                rom: None,
                function,
            });
        }

        // The devices that older workspaces described with the arguments of the machine and
        // with the ROM of its video. They take the place of a device config of the list for
        // the same slot.
        let detect = || MacModel::detect_from_rom(&std::fs::read(self.get_rom_path()?).ok()?);
        if let Some(model) = self.model.or_else(detect) {
            let display_rom = self.display_card_rom_path.take().map(|p| p.get_absolute());
            for config in old.device_configs(model, display_rom.as_deref()) {
                devices.retain(|d| d.bus != config.bus || d.slot != config.slot);
                devices.push(config);
            }
        }
    }

    /// The devices of the workspace, as device configs
    pub fn devices(&self) -> Vec<DeviceConfig> {
        self.devices
            .iter()
            .filter_map(|device| match device {
                Lenient::Read(config) => Some(config.clone()),
                Lenient::Unread(_) => None,
            })
            .collect()
    }

    /// Takes the devices of a machine, to be saved
    pub fn set_devices(&mut self, devices: Vec<DeviceConfig>) {
        self.devices = devices.into_iter().map(Lenient::Read).collect();
    }

    /// What the file had that couldn't be read, to show the user once
    pub fn take_unread(&mut self) -> Vec<String> {
        std::mem::take(&mut self.unread)
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

    /// The folder shared by the built-in AppleTalk file server
    #[cfg(feature = "afp_server")]
    pub fn set_afp_shared_dir(&mut self, p: Option<&Path>) {
        self.afp_shared_dir = p.map(RelativePath::from_absolute);
    }

    /// The folder shared by the built-in AppleTalk file server
    #[cfg(feature = "afp_server")]
    pub fn get_afp_shared_dir(&self) -> Option<PathBuf> {
        self.afp_shared_dir.clone().map(|d| d.get_absolute())
    }

    pub fn get_floppy_images(&self) -> Vec<PathBuf> {
        self.floppy_images
            .iter()
            .map(|p| p.get_absolute())
            .collect()
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

    /// Workspaces from before the file server, and with it on, as saved by
    /// this version and by earlier ones, all load
    #[test]
    fn workspaces_with_and_without_the_file_server_load() {
        for saved in [
            r#"{}"#,
            r#"{"serial_bridges":[null,{"AppleTalk":{}}]}"#,
            r#"{"serial_bridges":[null,{"AppleTalk":{"root":"/x","volume_name":"Snow"}}],
                "afp_volume_name":"Docs"}"#,
        ] {
            let workspace: Workspace = serde_json::from_str(saved).unwrap();
            assert!(workspace.serial_bridges[0].is_none());
        }
    }

    /// A workspace from before the NAT network setting has none of its own; one with its own
    /// keeps it through a save and a reload
    #[test]
    fn workspace_keeps_its_nat_network() {
        let workspace: Workspace = serde_json::from_str("{}").unwrap();
        assert_eq!(workspace.nat_network, None);

        let mut workspace = Workspace::default();
        let network = NatConfig {
            gateway: [192, 168, 200, 1],
            prefix_len: 24,
            dns: Some("192.168.1.53".parse().unwrap()),
        };
        workspace.nat_network = Some(network);
        let saved = serde_json::to_string(&workspace).unwrap();
        let workspace: Workspace = serde_json::from_str(&saved).unwrap();
        assert_eq!(workspace.nat_network, Some(network));
    }

    /// The shared folder survives a save and a reload, and the empty path
    /// that earlier versions saved loads as no folder
    #[cfg(feature = "afp_server")]
    #[test]
    fn workspace_keeps_the_shared_folder() {
        let dir = std::env::temp_dir().join(format!("snow-ws-afp-{}", std::process::id()));
        let shared = dir.join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        let shared = shared.canonicalize().unwrap();
        let path = dir.join("workspace.json");

        let mut workspace = Workspace::default();
        workspace.set_afp_shared_dir(Some(&shared));
        // What earlier versions saved: the path as set, never made relative
        let saved_before = serde_json::to_string(&workspace).unwrap();
        workspace.write_file(&path).unwrap();
        let loaded = Workspace::from_file(&path).unwrap();
        assert_eq!(loaded.get_afp_shared_dir(), Some(shared));

        std::fs::write(&path, saved_before).unwrap();
        let loaded = Workspace::from_file(&path).unwrap();
        assert_eq!(loaded.get_afp_shared_dir(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder of its own for a test, removed when the test is done
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "snow_workspace_{}_{}",
                name,
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.canonicalize().unwrap())
        }

        /// The workspace of a file with these contents
        fn load(&self, contents: &str) -> Workspace {
            let path = self.0.join("test.snoww");
            std::fs::write(&path, contents).unwrap();
            Workspace::from_file(&path).unwrap()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scsi(slot: &str, model: DeviceModel, function: FunctionConfig) -> DeviceConfig {
        DeviceConfig {
            bus: bus_id::SCSI.into(),
            slot: slot.into(),
            model: Some(model),
            rom: None,
            function,
        }
    }

    fn disk(slot: &str, media: PathBuf) -> DeviceConfig {
        scsi(
            slot,
            DeviceModel::ScsiHardDisk,
            FunctionConfig::HardDisk(HardDiskConfig { media: Some(media) }),
        )
    }

    fn cdrom(slot: &str, folder: Option<PathBuf>) -> DeviceConfig {
        scsi(
            slot,
            DeviceModel::ScsiCdrom,
            FunctionConfig::Cdrom(CdromConfig { folder }),
        )
    }

    #[test]
    fn the_old_fields_of_a_machine_describe_devices() {
        let args = OldInitArgs {
            monitor: Some(MacMonitor::RGB12),
            vram_size: Some(512 * 1024),
            pds_card: PdsCardKind::AppleIIe,
            video_card: NubusDeviceKind::Toby,
            override_fdd_type: Some(DriveType::GCR800KPWM),
            ..Default::default()
        };
        let places = |model, rom| -> Vec<(String, String, Option<DeviceModel>)> {
            args.device_configs(model, rom)
                .into_iter()
                .map(|config| (config.bus, config.slot, config.model))
                .collect()
        };
        let place = |bus: &str, slot: &str, model| (bus.to_string(), slot.to_string(), model);
        let drive = Some(DeviceModel::Mac800KPwmDrive);

        // An LC: its video, the card of its slot, its two drives
        let configs = args.device_configs(MacModel::LC, None);
        assert_eq!(
            configs[0].function,
            FunctionConfig::Display(DisplayConfig {
                monitor: Some(MacMonitor::RGB12),
                vram_size: Some(512 * 1024),
            })
        );
        assert_eq!(
            places(MacModel::LC, None),
            [
                place(bus_id::BUILT_IN, built_in::VIDEO, None),
                place(bus_id::PDS_LC, "0", Some(DeviceModel::AppleIIePdsCard)),
                place(bus_id::SWIM, "1", drive),
                place(bus_id::SWIM, "2", drive),
            ]
        );

        // A Macintosh II: the card that was chosen, with its ROM file and the monitor
        let rom = Path::new("card.rom");
        let configs = args.device_configs(MacModel::MacII, Some(rom));
        assert_eq!(configs[0].rom.as_deref(), Some(rom));
        assert_eq!(
            configs[0].function,
            FunctionConfig::Display(DisplayConfig {
                monitor: Some(MacMonitor::RGB12),
                vram_size: None,
            })
        );
        assert_eq!(
            places(MacModel::MacII, Some(rom))[0],
            place(bus_id::NUBUS, "9", Some(DeviceModel::MacIIVideoCard))
        );

        // An SE/30: the ROM file is that of its video
        let configs = args.device_configs(MacModel::SE30, Some(rom));
        assert_eq!(configs[0].bus, bus_id::BUILT_IN);
        assert_eq!(configs[0].rom.as_deref(), Some(rom));

        // A workspace saved since has none of these fields, and nothing is made from them
        let args = OldInitArgs::default();
        for model in [MacModel::LC, MacModel::MacII, MacModel::SE30] {
            assert_eq!(args.device_configs(model, None), []);
        }
    }

    #[test]
    fn an_old_field_that_cannot_be_read_does_not_cost_the_workspace() {
        let dir = TestDir::new("old_unread");
        let mut workspace = dir.load(
            r#"{"model":"LC","viewport_scale":3.0,
                "init_args":{"monitor":"SomeFutureMonitor","vram_size":262144,
                             "pds_card":"SomeFutureCard","ram_size":4194304}}"#,
        );
        // The rest of the file is read, the fields that can be read too
        assert_eq!(workspace.viewport_scale, 3.0);
        assert_eq!(workspace.init_args.ram_size, Some(4194304));
        let devices = workspace.devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(
            devices[0].function,
            FunctionConfig::Display(DisplayConfig {
                monitor: None,
                vram_size: Some(262144),
            })
        );
        assert_eq!(
            workspace.take_unread(),
            [
                "The monitor can't be read (\"SomeFutureMonitor\")",
                "The PDS card can't be read (\"SomeFutureCard\")",
            ]
        );
    }

    #[test]
    fn the_machine_of_an_older_workspace_becomes_device_configs() {
        let dir = TestDir::new("old_machine");
        std::fs::write(dir.0.join("card.rom"), []).unwrap();
        // A Macintosh II: the video card, its ROM file and its monitor
        let mut workspace = dir.load(
            r#"{"model":"MacII","display_card_rom_path":"card.rom",
                "init_args":{"video_card":"Mdc12","monitor":"RGB12"}}"#,
        );
        let card = DeviceConfig {
            bus: bus_id::NUBUS.into(),
            slot: "9".into(),
            model: Some(DeviceModel::DisplayCard824),
            rom: Some(dir.0.join("card.rom")),
            function: FunctionConfig::Display(DisplayConfig {
                monitor: Some(MacMonitor::RGB12),
                vram_size: None,
            }),
        };
        assert_eq!(workspace.devices(), std::slice::from_ref(&card));
        assert!(workspace.take_unread().is_empty());

        // The fields win over a device config of the same slot, and are not written again
        let mut workspace = dir.load(
            r#"{"model":"MacII","display_card_rom_path":"card.rom",
                "init_args":{"video_card":"Mdc12","monitor":"RGB12"},
                "devices":[{"bus":"NuBus","slot":"9","model":"DisplayCard824",
                            "function":{"Display":{}}},
                           {"bus":"SCSI","slot":"3","model":"ScsiCdrom","function":{"Cdrom":{}}}]}"#,
        );
        assert_eq!(workspace.devices(), [cdrom("3", None), card.clone()]);
        let path = dir.0.join("saved.snoww");
        workspace.write_file(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        for field in ["display_card_rom_path", "video_card", "\"monitor\": null"] {
            assert!(!saved.contains(field), "{field} in {saved}");
        }
        let workspace = Workspace::from_file(&path).unwrap();
        assert_eq!(workspace.devices(), [cdrom("3", None), card]);

        // An LC: what was chosen for its video, and the card of its slot
        let workspace = dir.load(
            r#"{"model":"LC","init_args":{"monitor":"RGB12","vram_size":262144,
                "pds_card":"AppleIIe"}}"#,
        );
        let devices = workspace.devices();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].bus, bus_id::BUILT_IN);
        assert_eq!(devices[0].slot, built_in::VIDEO);
        assert_eq!(
            devices[0].function,
            FunctionConfig::Display(DisplayConfig {
                monitor: Some(MacMonitor::RGB12),
                vram_size: Some(262144),
            })
        );
        assert_eq!(devices[1].model, Some(DeviceModel::AppleIIePdsCard));
    }

    #[test]
    fn scsi_targets_of_an_older_workspace_become_device_configs() {
        let dir = TestDir::new("old");
        let mut workspace = dir.load(
            r#"{"scsi_targets":[{"Disk":"hd.img"},"None","Cdrom",{"CdromFolder":"cds"},
                "Ethernet","Printer","None"],
                "ethernet_link_type":"Down"}"#,
        );
        let devices = workspace.devices();
        assert_eq!(
            devices[..3],
            [
                disk("0", dir.0.join("hd.img")),
                cdrom("2", None),
                cdrom("3", Some(dir.0.join("cds"))),
            ]
        );
        #[cfg(feature = "ethernet")]
        {
            let FunctionConfig::Ethernet(ethernet) = &devices[3].function else {
                panic!("An Ethernet adapter has an Ethernet config");
            };
            assert_eq!(devices[3].model, Some(DeviceModel::DaynaPortScsiLink));
            assert_eq!(devices[3].slot, "4");
            assert_eq!(ethernet.link, EthernetLinkType::Down);
        }
        #[cfg(feature = "printer")]
        assert_eq!(
            devices.last().unwrap().model,
            Some(DeviceModel::LaserWriterIISC)
        );
        // What this build has no device for is said
        let missing = [cfg!(feature = "ethernet"), cfg!(feature = "printer")];
        assert_eq!(
            workspace.take_unread().len(),
            missing.iter().filter(|&&has| !has).count()
        );
    }

    #[test]
    fn disks_of_an_older_workspace_become_hard_disks() {
        let dir = TestDir::new("disks");
        let workspace = dir.load(r#"{"disks":[null,"a.img",null,null,null,null,null]}"#);
        assert_eq!(workspace.devices(), [disk("1", dir.0.join("a.img"))]);
    }

    #[test]
    fn devices_are_saved_with_relative_paths() {
        let dir = TestDir::new("save");
        let devices = vec![
            disk("0", dir.0.join("hd.img")),
            cdrom("3", Some(dir.0.join("sub").join("cds"))),
        ];
        let path = dir.0.join("saved.snoww");
        let mut workspace = Workspace::default();
        workspace.set_devices(devices.clone());
        workspace.write_file(&path).unwrap();
        // The workspace keeps its absolute paths
        assert_eq!(workspace.devices(), devices);

        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            saved["devices"],
            serde_json::json!([
                { "bus": "SCSI", "slot": "0", "model": "ScsiHardDisk",
                  "function": { "HardDisk": { "media": "hd.img" } } },
                { "bus": "SCSI", "slot": "3", "model": "ScsiCdrom",
                  "function": { "Cdrom": { "folder": "sub/cds" } } },
            ])
        );
        // The older fields are no longer written
        for old in ["disks", "scsi_targets", "ethernet_link_type"] {
            assert!(saved.get(old).is_none(), "{old} is written");
        }

        assert_eq!(Workspace::from_file(&path).unwrap().devices(), devices);
    }

    #[test]
    fn a_device_that_cannot_be_read_does_not_cost_the_others() {
        let dir = TestDir::new("unread");
        let mut workspace = dir.load(
            r#"{"devices":[
                {"bus":"SCSI","slot":"0","model":"NoSuchDevice","function":{"Cdrom":{}}},
                {"bus":"SCSI","slot":"3","model":"ScsiCdrom","function":{"Cdrom":{}}}]}"#,
        );
        assert_eq!(workspace.devices(), [cdrom("3", None)]);
        assert_eq!(workspace.take_unread().len(), 1);
    }
}
