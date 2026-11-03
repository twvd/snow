//! The machine as a frontend and a workspace see it
//!
//! A machine is a flat list of buses. A bus is a collection of slots, a slot may hold a device,
//! and a device is an instance of a device model, which provides one function. The function is
//! what the frontend deals with: the attributes every device model providing it has, such as a
//! disk image, and a status the device fills in. A device model's own attributes, such as a
//! card's ROM, travel as a dictionary so that a workspace can pass through code that doesn't
//! know them. Code that knows a device model uses its typed settings struct.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use snow_floppy::loaders::{Autodetect, FloppyImageLoader, TwoImg};
use snow_floppy::{Floppy as _, FloppyType};

use super::comm::EmulatorEvent;
use super::{Emulator, EmulatorConfig, MouseMode};
use crate::mac::maclc::pds_iie::disk::DskOrder;
use crate::mac::maclc::pds_iie::{IieCardSettings, IieDiskDevice, PdsIIeCard};
use crate::mac::scc::SccCh;
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::serial_bridge::serial_device;
use crate::mac::serial_bridge::{SerialBridgeConfig, SerialBridgeStatus};
use crate::mac::swim::MAX_DCDS;
use crate::mac::swim::dcd::{DCD_BLOCKSIZE, DcdDevice};
use crate::mac::swim::drive::{DriveType, FloppyDrive};
use crate::mac::{ExtraROMs, MacModel, MacMonitor};
use crate::renderer::DisplayBuffer;
use log::info;

/// The ids of the buses. They are in the workspace files: they must not change.
pub mod bus_id {
    pub const SWIM: &str = "SWIM";
    pub const SCSI: &str = "SCSI";
    pub const NUBUS: &str = "NuBus";
    pub const PDS_LC: &str = "PDS_LC";
    pub const PDS_SE30: &str = "PDS_SE30";
    /// The drives of the Apple IIe Card in the LC PDS slot
    pub const IIE_DISK: &str = "Floppy:PDS_LC.0";
    pub const SERIAL: &str = "Serial";
    pub const BUILT_IN: &str = "Built-in";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusKind {
    Swim,
    Scsi,
    Nubus,
    LcPds,
    Se30Pds,
    /// The Apple IIe Card's chain of drives
    IieDisk,
    Serial,
    /// Devices on the logic board with no real bus behind them
    BuiltIn,
}

/// What a device does, as far as the frontend is concerned. A new one needs frontend code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    HardDisk,
    Cdrom,
    Floppy,
    Ethernet,
    Printer,
    FileTransfer,
    Video,
    AppleIIe,
    SerialLink,
    LocalTalk,
}

/// Everything Snow can put in a slot. A new device model of a known function only touches the
/// core. A device model is what the guest sees: how the host backs it is a setting.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    strum::IntoStaticStr,
    strum::EnumString,
    strum::EnumIter,
)]
pub enum DeviceModel {
    ScsiHardDisk,
    Hd20,
    ScsiCdrom,
    #[cfg(feature = "ethernet")]
    DaynaPortScsiLink,
    #[cfg(feature = "printer")]
    LaserWriterIISC,
    BlueScsi,
    Mac400KDrive,
    Mac800KDrive,
    Mac800KPwmDrive,
    MacSuperDrive,
    UniDisk35,
    Apple525Drive,
    DisplayCard824,
    MacIIVideoCard,
    SE30Video,
    AppleIIePdsCard,
    LocalTalkAdapter,
    SerialBridge,
}

/// What a device model's file registers about it
pub struct ModelInfo {
    pub model: DeviceModel,
    /// In the menus: free to change
    pub name: &'static str,
    /// The bus type it fits, so the slot trait it implements
    pub bus: BusKind,
    /// The Mac models it fits, among those with that bus
    pub fits: fn(MacModel) -> bool,
    pub function: FunctionKind,
    /// Created with the machine, never added or removed while it runs
    pub fixed: bool,
    /// A second one on the same bus is refused, and greyed in the menus
    pub one_per_bus: bool,
    pub rom: Option<RomInfo>,
    /// The settings holding paths, which a workspace keeps relative to its file
    pub path_keys: &'static [&'static str],
    /// The buses a device of this model adds
    pub adds_buses: &'static [BusKind],
}

/// A ROM a device model needs
pub struct RomInfo {
    /// In the menus: "Display Card 8-24 ROM"
    pub label: &'static str,
    /// SHA-256 of each ROM known to work, in hex
    pub digests: &'static [&'static str],
}

impl ModelInfo {
    pub const fn fits_all(_mac: MacModel) -> bool {
        true
    }

    pub const fn needs_rom(&self) -> bool {
        self.rom.is_some()
    }

    /// Whether this is a ROM the device model is known to work with
    pub fn validate_rom(&self, rom: &[u8]) -> bool {
        let Some(info) = &self.rom else {
            return false;
        };
        let digest = hex::encode(Sha256::digest(rom));
        info.digests.contains(&digest.as_str())
    }
}

/// One line per device model, with its feature gate beside it. Rust has no initcall, so a
/// device model's file can't add itself.
pub static MODELS: &[&ModelInfo] = &[
    &crate::mac::scsi::disk::INFO,
    &crate::mac::swim::dcd::INFO,
    &crate::mac::scsi::cdrom::INFO,
    #[cfg(feature = "ethernet")]
    &crate::mac::scsi::ethernet::INFO,
    #[cfg(feature = "printer")]
    &crate::mac::scsi::printer::INFO,
    &crate::mac::scsi::toolbox::INFO,
    &crate::mac::swim::drive::INFO_400K,
    &crate::mac::swim::drive::INFO_800K,
    &crate::mac::swim::drive::INFO_800K_PWM,
    &crate::mac::swim::drive::INFO_SUPERDRIVE,
    &crate::mac::maclc::pds_iie::unidisk::INFO,
    &crate::mac::maclc::pds_iie::disk::INFO,
    &crate::mac::nubus::mdc12::INFO,
    &crate::mac::nubus::toby::INFO,
    &crate::mac::nubus::se30video::INFO,
    &crate::mac::maclc::pds_iie::INFO,
    &crate::mac::localtalk_bridge::INFO,
    &crate::mac::serial_bridge::INFO,
];

/// What a slot on this kind of bus can take, on this Mac model
pub fn models_for(bus: BusKind, mac: MacModel) -> impl Iterator<Item = &'static ModelInfo> {
    MODELS
        .iter()
        .copied()
        .filter(move |info| info.bus == bus && (info.fits)(mac))
}

impl DeviceModel {
    /// In the workspace files: must not change
    pub fn key(self) -> &'static str {
        self.into()
    }

    pub fn from_key(key: &str) -> Option<Self> {
        key.parse().ok()
    }

    /// What its file registered
    pub fn info(self) -> &'static ModelInfo {
        MODELS
            .iter()
            .find(|info| info.model == self)
            .expect("every device model is in the table")
    }

    /// In the menus: free to change
    pub fn name(self) -> &'static str {
        self.info().name
    }

    pub fn function(self) -> FunctionKind {
        self.info().function
    }

    /// Created with the machine, never added or removed while it runs
    pub fn fixed(self) -> bool {
        self.info().fixed
    }

    /// The settings holding paths, which a workspace keeps relative to its file
    pub fn path_keys(self) -> &'static [&'static str] {
        self.info().path_keys
    }

    /// The buses a device of this model adds
    pub fn buses(self) -> &'static [BusKind] {
        self.info().adds_buses
    }

    /// The drive of a machine
    pub const fn from_drive_type(drive: DriveType) -> Option<Self> {
        match drive {
            DriveType::None => None,
            DriveType::GCR400K => Some(Self::Mac400KDrive),
            DriveType::GCR800K => Some(Self::Mac800KDrive),
            DriveType::GCR800KPWM => Some(Self::Mac800KPwmDrive),
            DriveType::SuperDrive => Some(Self::MacSuperDrive),
        }
    }

    pub const fn drive_type(self) -> Option<DriveType> {
        match self {
            Self::Mac400KDrive => Some(DriveType::GCR400K),
            Self::Mac800KDrive => Some(DriveType::GCR800K),
            Self::Mac800KPwmDrive => Some(DriveType::GCR800KPWM),
            Self::MacSuperDrive => Some(DriveType::SuperDrive),
            _ => None,
        }
    }
}

/// The Floppy function. Mac and Apple IIe Card drives look the same to the frontend: what
/// differs is what each can do, which the status says.
pub trait Floppy {
    /// Fills in the status from the drive's state; true if anything changed
    fn update_status(&mut self, status: &mut FloppyStatus) -> bool;
    fn load(&mut self, media: &Path, read_only: bool) -> Result<()>;
    fn eject(&mut self);
}

/// The Ethernet function. Any device model providing it looks the same to the frontend.
#[cfg(feature = "ethernet")]
pub trait Ethernet {
    fn link(&self) -> crate::mac::scsi::ethernet::EthernetLinkType;
    fn set_link(&mut self, link: crate::mac::scsi::ethernet::EthernetLinkType) -> Result<()>;
    fn start_capture(&mut self, filename: &Path) -> Result<()>;
    fn stop_capture(&mut self) -> Option<(PathBuf, usize)>;
    fn capture_status(&self) -> Option<super::comm::EthernetCaptureStatus>;
}

/// The FileTransfer function: a host folder the guest copies files in and out of
pub trait FileTransfer {
    fn folder(&self) -> Option<&Path>;
    fn load(&mut self, folder: &Path) -> Result<()>;
    /// Stops sharing the folder
    fn eject(&mut self);
}

/// A place on a bus
#[derive(Debug, Clone)]
pub struct Slot {
    /// In the workspace files: must not change
    pub id: String,
    /// In the menus: "internal", "Modem"
    pub name: Option<String>,
    /// What the guest needs to use this slot, for the menus to say
    pub hint: Option<String>,
    pub device: Option<Device>,
}

impl Slot {
    fn empty(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            hint: None,
            device: None,
        }
    }

    fn named(id: impl Into<String>, name: &str) -> Self {
        Self {
            name: Some(name.to_owned()),
            ..Self::empty(id)
        }
    }

    /// A slot with what the machine has in it
    fn fixed(id: impl Into<String>, model: DeviceModel) -> Self {
        Self {
            device: Some(Device {
                model,
                settings: Map::new(),
            }),
            ..Self::empty(id)
        }
    }
}

/// A collection of slots
#[derive(Debug, Clone)]
pub struct Bus {
    pub kind: BusKind,
    /// In the workspace files: must not change
    pub id: String,
    /// In the menus: free to change
    pub title: String,
    pub slots: Vec<Slot>,
}

impl Bus {
    /// Whether a device of this model is on the bus
    pub fn has(&self, model: DeviceModel) -> bool {
        self.slots
            .iter()
            .any(|s| s.device.as_ref().is_some_and(|d| d.model == model))
    }

    /// Whether a device of this model can go in an empty slot of this bus right now
    pub fn can_take(&self, info: &ModelInfo) -> bool {
        !info.fixed && (!info.one_per_bus || !self.has(info.model))
    }
}

/// Where a slot is on the running machine. Never saved: it changes when a bus is added.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SlotAddr {
    pub bus: usize,
    pub slot: usize,
}

/// A device in a slot
#[derive(Debug, Clone)]
pub struct Device {
    pub model: DeviceModel,
    /// The device model's own attributes, such as a card's ROM. The function's are in the status.
    pub settings: Map<String, Value>,
}

/// What a function shows of a device, filled in by the device. Never saved, except for the
/// function's attributes in `settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionStatus {
    HardDisk(HardDiskStatus),
    Cdrom(CdromStatus),
    Floppy(FloppyStatus),
    #[cfg(feature = "ethernet")]
    Ethernet(EthernetStatus),
    Printer,
    FileTransfer(FileTransferStatus),
    Video,
    AppleIIe,
    SerialLink(Option<SerialBridgeStatus>),
    LocalTalk(Option<SerialBridgeStatus>),
}

impl FunctionStatus {
    /// The function's attributes, which a workspace saves
    pub fn settings(&self) -> Map<String, Value> {
        match self {
            Self::HardDisk(s) => settings_map(&s.settings),
            Self::Cdrom(s) => settings_map(&s.settings),
            Self::Floppy(s) => settings_map(&s.settings),
            Self::FileTransfer(s) => settings_map(&s.settings),
            #[cfg(feature = "ethernet")]
            Self::Ethernet(s) => settings_map(&serde_json::json!({ "link": s.link })),
            _ => Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardDiskStatus {
    pub settings: HardDiskSettings,
    pub capacity: Option<usize>,
    /// The image can be branched off to a new file
    pub can_branch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdromStatus {
    /// The image, or the folder of images of a Toolbox managed drive
    pub settings: CdromSettings,
    pub image: Option<PathBuf>,
    pub folder: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloppyStatus {
    pub settings: FloppySettings,
    pub title: String,
    pub ejected: bool,
    pub dirty: bool,
    /// The image can be saved to a file, and the drive remembers the last one ejected
    pub can_save: bool,
    /// The image can be written back to the file it came from
    pub writeback_supported: bool,
    pub writeback_enabled: bool,
    /// The blank disks the drive can take
    pub blank_formats: Vec<FloppyType>,
    /// For the file dialog: what the images are, and their extensions
    pub image_label: &'static str,
    pub image_types: Vec<&'static str>,
    pub rpm_control: bool,
}

impl Default for FloppyStatus {
    fn default() -> Self {
        Self {
            settings: FloppySettings::default(),
            title: String::new(),
            ejected: true,
            dirty: false,
            can_save: false,
            writeback_supported: false,
            writeback_enabled: false,
            blank_formats: vec![],
            image_label: "",
            image_types: vec![],
            rpm_control: false,
        }
    }
}

#[cfg(feature = "ethernet")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthernetStatus {
    pub link: crate::mac::scsi::ethernet::EthernetLinkType,
    pub capture: Option<super::comm::EthernetCaptureStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransferStatus {
    pub settings: FileTransferSettings,
}

/// The status of every device, shared with the frontend
pub struct StatusBlock {
    /// Bumped when a status changed, so the frontend only copies them then
    pub generation: AtomicU64,
    /// By `SlotAddr`, in the shape of the bus list
    pub statuses: Mutex<Vec<Vec<Option<FunctionStatus>>>>,
}

impl StatusBlock {
    pub(super) fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            statuses: Mutex::new(vec![]),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// One device, as a workspace stores it and as a machine is told about it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub bus: String,
    pub slot: String,
    /// None: the device the machine has in this slot
    #[serde(rename = "device", default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What the model understands. Other entries are ignored, and a model's struct says which.
    #[serde(flatten)]
    pub settings: Map<String, Value>,
}

impl DeviceConfig {
    /// A device of the model these settings are for
    pub fn new<S: DeviceSettings>(bus: &str, slot: &str, settings: &S) -> Result<Self> {
        Ok(Self {
            bus: bus.to_owned(),
            slot: slot.to_owned(),
            model: Some(S::MODEL.key().to_owned()),
            settings: to_map(settings)?,
        })
    }

    /// A model that needs nothing, or settings for it that aren't known at compile time
    pub fn of_model(
        bus: &str,
        slot: &str,
        model: DeviceModel,
        settings: &impl Serialize,
    ) -> Result<Self> {
        Ok(Self {
            bus: bus.to_owned(),
            slot: slot.to_owned(),
            model: Some(model.key().to_owned()),
            settings: to_map(settings)?,
        })
    }

    /// Settings for the device already in a slot
    pub fn existing(bus: &str, slot: &str, settings: &impl Serialize) -> Result<Self> {
        Ok(Self {
            bus: bus.to_owned(),
            slot: slot.to_owned(),
            model: None,
            settings: to_map(settings)?,
        })
    }

    /// How an entry is named in messages
    pub fn label(&self) -> String {
        format!("{} #{}", self.bus, self.slot)
    }

    /// The model, if this build knows it
    pub fn known_model(&self) -> Option<DeviceModel> {
        DeviceModel::from_key(self.model.as_deref()?)
    }

    /// The model, or the one this Mac has in the slot if the entry names none
    pub fn model_on(&self, mac: MacModel) -> Option<DeviceModel> {
        if self.model.is_some() {
            return self.known_model();
        }
        let buses = mac.buses();
        let bus = buses.iter().find(|b| b.id == self.bus)?;
        let slot = bus.slots.iter().find(|s| s.id == self.slot)?;
        Some(slot.device.as_ref()?.model)
    }

    /// The settings as what a model expects
    pub fn parse<S: DeserializeOwned>(&self) -> Result<S> {
        serde_json::from_value(Value::Object(self.settings.clone())).map_err(|e| {
            anyhow!(
                "{} #{}: bad settings for {}: {}",
                self.bus,
                self.slot,
                self.model.as_deref().unwrap_or("device"),
                e
            )
        })
    }

    /// The names of the settings holding paths. An entry with no model has what drives have.
    pub fn path_keys(&self) -> &'static [&'static str] {
        match (&self.model, self.known_model()) {
            (None, _) => &["media"],
            (Some(_), Some(model)) => model.path_keys(),
            (Some(_), None) => &[],
        }
    }
}

fn to_map(settings: &impl Serialize) -> Result<Map<String, Value>> {
    match serde_json::to_value(settings)? {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(Map::new()),
        v => Err(anyhow!("Settings are not a dictionary: {}", v)),
    }
}

/// The settings of a model. The struct names the model, so they can't be paired wrongly.
pub trait DeviceSettings: Serialize + DeserializeOwned {
    const MODEL: DeviceModel;
}

/// What the hard disk function needs, whichever the device model
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardDiskSettings {
    pub media: PathBuf,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScsiHardDiskSettings {
    #[serde(flatten)]
    pub disk: HardDiskSettings,
}
impl DeviceSettings for ScsiHardDiskSettings {
    const MODEL: DeviceModel = DeviceModel::ScsiHardDisk;
}

/// What the CD-ROM function needs, whichever the device model
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CdromSettings {
    /// An image, or a folder of images the guest swaps with the BlueSCSI Toolbox
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScsiCdromSettings {
    #[serde(flatten)]
    pub cdrom: CdromSettings,
}
impl DeviceSettings for ScsiCdromSettings {
    const MODEL: DeviceModel = DeviceModel::ScsiCdrom;
}

#[cfg(feature = "printer")]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaserWriterIISCSettings {
    /// Where the pages go. It's the host's business, so it isn't saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<PathBuf>,
}
#[cfg(feature = "printer")]
impl DeviceSettings for LaserWriterIISCSettings {
    const MODEL: DeviceModel = DeviceModel::LaserWriterIISC;
}

/// What the FileTransfer function needs, whichever the device model
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTransferSettings {
    /// The folder the guest copies files in and out of
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<PathBuf>,
}

/// What the floppy function needs, whichever the device model
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloppySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayCard824Settings {
    pub rom: PathBuf,
}
impl DeviceSettings for DisplayCard824Settings {
    const MODEL: DeviceModel = DeviceModel::DisplayCard824;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacIIVideoCardSettings {
    pub rom: PathBuf,
}
impl DeviceSettings for MacIIVideoCardSettings {
    const MODEL: DeviceModel = DeviceModel::MacIIVideoCard;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SE30VideoSettings {
    pub rom: PathBuf,
}
impl DeviceSettings for SE30VideoSettings {
    const MODEL: DeviceModel = DeviceModel::SE30Video;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppleIIePdsCardSettings {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub security_jumper: bool,
}
impl DeviceSettings for AppleIIePdsCardSettings {
    const MODEL: DeviceModel = DeviceModel::AppleIIePdsCard;
}

/// What the serial link function needs, whichever the device model
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialLinkSettings {
    /// None for a PTY
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_port: Option<u16>,
}

impl MacModel {
    /// The buses every machine of this model has, with its fixed devices in their slots. Cards
    /// add theirs.
    pub fn buses(self) -> Vec<Bus> {
        let mut buses = vec![];

        let drives: Vec<_> = self
            .fdd_drives()
            .iter()
            .enumerate()
            .filter_map(|(i, d)| Some((i, DeviceModel::from_drive_type(*d)?)))
            .collect();
        if !drives.is_empty() {
            let mut slots: Vec<_> = drives
                .iter()
                .map(|&(i, model)| Slot::fixed((i + 1).to_string(), model))
                .collect();
            // HD20s chain on the external drive port
            slots.extend((1..=MAX_DCDS).map(|i| Slot {
                name: Some("HD20".to_owned()),
                hint: Some(self.hd20_hint().to_owned()),
                ..Slot::empty(format!("{}{}", HD20_SLOT, i))
            }));
            buses.push(Bus {
                kind: BusKind::Swim,
                id: bus_id::SWIM.to_owned(),
                title: "Floppy".to_owned(),
                slots,
            });
        }

        if self.has_scsi() {
            buses.push(Bus {
                kind: BusKind::Scsi,
                id: bus_id::SCSI.to_owned(),
                title: "SCSI".to_owned(),
                slots: (0..7).map(|i| Slot::empty(i.to_string())).collect(),
            });
        }

        if self == Self::SE30 {
            buses.push(Bus {
                kind: BusKind::BuiltIn,
                id: bus_id::BUILT_IN.to_owned(),
                title: "Built-in".to_owned(),
                slots: vec![Slot::fixed("0", DeviceModel::SE30Video)],
            });
            buses.push(Bus {
                kind: BusKind::Se30Pds,
                id: bus_id::PDS_SE30.to_owned(),
                title: "SE/30 PDS".to_owned(),
                slots: vec![Slot::empty("0")],
            });
        } else if !self.nubus_slots().is_empty() {
            buses.push(Bus {
                kind: BusKind::Nubus,
                id: bus_id::NUBUS.to_owned(),
                title: "NuBus".to_owned(),
                slots: self
                    .nubus_slots()
                    .iter()
                    .map(|s| Slot::empty(format!("{:X}", s)))
                    .collect(),
            });
        }

        if self.has_lc_pds() {
            buses.push(Bus {
                kind: BusKind::LcPds,
                id: bus_id::PDS_LC.to_owned(),
                title: "LC PDS".to_owned(),
                slots: vec![Slot::empty("0")],
            });
        }

        buses.push(Bus {
            kind: BusKind::Serial,
            id: bus_id::SERIAL.to_owned(),
            title: "Serial".to_owned(),
            slots: vec![Slot::named("A", "Modem"), Slot::named("B", "Printer")],
        });

        buses
    }

    /// The bus the Apple IIe Card adds, with the drives on its chain
    pub fn iie_disk_bus(chain: &[DeviceModel]) -> Bus {
        Bus {
            kind: BusKind::IieDisk,
            id: bus_id::IIE_DISK.to_owned(),
            title: "Floppy (Apple IIe Card)".to_owned(),
            slots: chain
                .iter()
                .enumerate()
                .map(|(i, &model)| Slot::fixed((i + 1).to_string(), model))
                .collect(),
        }
    }

    pub const fn has_lc_pds(self) -> bool {
        matches!(self, Self::LC | Self::LCII | Self::LCIII | Self::LC475)
    }

    /// What the guest needs to see an HD20. Which ROMs have the driver is in docs/HD20.md.
    const fn hd20_hint(self) -> &'static str {
        match self {
            Self::Early128K => "This Mac doesn't have enough RAM to load the HD20 INIT.",
            Self::Early512K => {
                "This Mac needs the HD20 INIT from a boot floppy. It looks for HD20s when it \
                 starts up."
            }
            Self::MacII | Self::MacIIFDHD | Self::MacIIx | Self::MacIIcx | Self::SE30 => {
                "This Mac's ROM has no HD20 driver, it needs Apple's MacIIHD20 INIT. It looks \
                 for HD20s when it starts up."
            }
            _ => "The Mac looks for HD20s when it starts up.",
        }
    }
}

/// The ids of the HD20 slots on the floppy bus start with this, then their chain position
pub const HD20_SLOT: &str = "HD20.";

/// What a machine remembers of its devices, where the devices don't
#[derive(Default)]
pub(super) struct DeviceBook {
    /// The cards the machine was built with. They can't change, so they report what they got.
    pub(super) cards: Vec<DeviceConfig>,
    /// The Apple IIe Card's drives, in chain order
    pub(super) iie_chain: Vec<DeviceModel>,
    pub(super) serial: [Option<SerialBridgeConfig>; 2],
    /// The images a restored machine's SCSI devices came from: it runs on copies
    pub(super) restored_media: [Option<PathBuf>; 7],
    /// The same for its HD20s
    pub(super) restored_hd20: [Option<PathBuf>; MAX_DCDS],
}

impl DeviceBook {
    pub(super) fn set_iie_chain(&mut self, settings: &IieCardSettings) {
        self.iie_chain = settings
            .disk_chain
            .iter()
            .filter_map(|d| match d {
                IieDiskDevice::None => None,
                IieDiskDevice::Apple525 => Some(DeviceModel::Apple525Drive),
                IieDiskDevice::UniDisk35 => Some(DeviceModel::UniDisk35),
            })
            .collect();
    }
}

/// What a machine is, apart from its devices
pub struct MachineConfig<'a> {
    pub rom: &'a [u8],
    pub model: MacModel,
    pub extension_rom: Option<&'a [u8]>,
    pub monitor: Option<MacMonitor>,
    pub mouse_mode: MouseMode,
    pub ram_size: Option<usize>,
    pub pmmu_enabled: bool,
    pub fpu_enabled: bool,
    pub vram_size: Option<usize>,
}

/// A machine, where its frames arrive, and the devices it couldn't use
pub type NewMachine = (
    Emulator,
    Arc<Mutex<Option<DisplayBuffer>>>,
    Vec<RefusedDevice>,
);

/// An entry a machine couldn't use. It stays in the workspace.
#[derive(Debug, Clone)]
pub struct RefusedDevice {
    pub config: DeviceConfig,
    pub reason: String,
}

/// What has to be decided before a machine can be built
#[derive(Default)]
struct BuildPlan {
    video: Option<(u8, DeviceModel, Vec<u8>)>,
    iie_card: Option<IieCardSettings>,
    override_drive: Option<DriveType>,
    cards: Vec<DeviceConfig>,
    /// What to hand to the machine once it's built
    rest: Vec<DeviceConfig>,
    refused: Vec<RefusedDevice>,
}

impl BuildPlan {
    fn new(model: MacModel, devices: Vec<DeviceConfig>) -> Self {
        let mut plan = Self::default();
        // The chain's order matters and the file's doesn't
        let mut chain_entries = vec![];
        let layout = model.buses();
        for config in devices {
            // An entry that names no device means what this machine has in that slot
            let device = config.known_model().or_else(|| {
                let bus = layout.iter().find(|b| b.id == config.bus)?;
                let slot = bus.slots.iter().find(|s| s.id == config.slot)?;
                let device = slot.device.as_ref().filter(|_| config.model.is_none())?;
                Some(device.model)
            });
            match device.filter(|m| m.fixed()) {
                Some(DeviceModel::UniDisk35 | DeviceModel::Apple525Drive) => {
                    chain_entries.push(config);
                }
                Some(device) => {
                    let result = plan.take_fixed(model, device, &config);
                    plan.settle(config, result);
                }
                None => plan.rest.push(config),
            }
        }
        chain_entries.sort_by(|a, b| a.slot.cmp(&b.slot));
        plan.take_iie_chain(chain_entries);
        plan
    }

    fn settle(&mut self, config: DeviceConfig, result: Result<bool>) {
        match result {
            // The machine still has to get its settings
            Ok(true) => self.rest.push(config),
            Ok(false) => (),
            Err(e) => self.refused.push(RefusedDevice {
                config,
                reason: format!("{:#}", e),
            }),
        }
    }

    /// Returns whether there's something left to do once the machine exists
    fn take_fixed(
        &mut self,
        model: MacModel,
        device: DeviceModel,
        config: &DeviceConfig,
    ) -> Result<bool> {
        let fits = |kind| models_for(kind, model).any(|i| i.model == device);
        match device {
            DeviceModel::DisplayCard824 | DeviceModel::MacIIVideoCard => {
                if config.bus != bus_id::NUBUS || !fits(BusKind::Nubus) {
                    bail!("This machine can't take a {}", device.name());
                }
                let slot = u8::from_str_radix(&config.slot, 16)
                    .ok()
                    .filter(|s| model.nubus_slots().contains(s))
                    .context("No such NuBus slot on this machine")?;
                let rom: DisplayCard824Settings = config.parse()?;
                self.take_video(slot, device, &rom.rom, config)?;
                Ok(false)
            }
            DeviceModel::SE30Video => {
                if config.bus != bus_id::BUILT_IN || !fits(BusKind::BuiltIn) {
                    bail!("This machine can't take a {}", device.name());
                }
                let rom: SE30VideoSettings = config.parse()?;
                self.take_video(0xE, device, &rom.rom, config)?;
                Ok(false)
            }
            DeviceModel::AppleIIePdsCard => {
                if config.bus != bus_id::PDS_LC || !fits(BusKind::LcPds) {
                    bail!("This machine can't take a {}", device.name());
                }
                let settings: AppleIIePdsCardSettings = config.parse()?;
                self.iie_card = Some(IieCardSettings {
                    security_jumper: settings.security_jumper,
                    disk_chain: [IieDiskDevice::None, IieDiskDevice::None],
                });
                self.cards.push(config.clone());
                Ok(false)
            }
            _ => {
                // A drive other than the machine's own
                if config.bus != bus_id::SWIM {
                    bail!("{} doesn't go on {}", device.name(), config.bus);
                }
                let own = model.fdd_drives().first().copied();
                let wanted = device.drive_type();
                if wanted != own {
                    if !fits(BusKind::Swim) {
                        bail!("This machine can't take a {}", device.name());
                    }
                    // A machine has one kind of drive for now
                    self.override_drive = wanted;
                }
                Ok(true)
            }
        }
    }

    fn take_video(
        &mut self,
        slot: u8,
        device: DeviceModel,
        rom: &Path,
        config: &DeviceConfig,
    ) -> Result<()> {
        if self.video.is_some() {
            bail!("A machine can only have one video card for now");
        }
        let rom = std::fs::read(rom).with_context(|| format!("Cannot read {}", rom.display()))?;
        self.video = Some((slot, device, rom));
        self.cards.push(DeviceConfig {
            model: Some(device.key().to_owned()),
            ..config.clone()
        });
        Ok(())
    }

    /// The card builds its chain from these, so it has to get them first
    fn take_iie_chain(&mut self, entries: Vec<DeviceConfig>) {
        for config in entries {
            let result = self.take_iie_drive(&config);
            self.settle(config, result);
        }
    }

    fn take_iie_drive(&mut self, config: &DeviceConfig) -> Result<bool> {
        let Some(card) = &mut self.iie_card else {
            bail!("This machine has no Apple IIe Card");
        };
        if config.bus != bus_id::IIE_DISK {
            bail!("These drives only go on the Apple IIe Card");
        }
        let position = match config.slot.as_str() {
            "1" => 0,
            "2" => 1,
            _ => bail!("The Apple IIe Card's chain has two drives"),
        };
        if position == 1 && card.disk_chain[0] == IieDiskDevice::None {
            bail!("The second drive plugs into the first");
        }
        card.disk_chain[position] = match config.known_model() {
            Some(DeviceModel::UniDisk35) if position == 0 => IieDiskDevice::UniDisk35,
            Some(DeviceModel::UniDisk35) => bail!("A UniDisk 3.5 must come first"),
            _ => IieDiskDevice::Apple525,
        };
        Ok(true)
    }
}

/// What is behind a slot. Only this file knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotTarget {
    Drive(usize),
    /// A position on the HD20 chain
    Hd20(usize),
    Scsi(usize),
    Card,
    IieDrive(usize),
    Serial(SccCh),
}

fn slot_target(bus: &Bus, slot: &Slot) -> SlotTarget {
    let number = |base: usize| slot.id.parse::<usize>().unwrap_or(base) - base;
    match bus.kind {
        BusKind::Swim => match slot.id.strip_prefix(HD20_SLOT) {
            Some(position) => SlotTarget::Hd20(position.parse::<usize>().unwrap_or(1) - 1),
            None => SlotTarget::Drive(number(1)),
        },
        BusKind::Scsi => SlotTarget::Scsi(number(0)),
        BusKind::IieDisk => SlotTarget::IieDrive(number(1)),
        BusKind::Serial if slot.id == "A" => SlotTarget::Serial(SccCh::A),
        BusKind::Serial => SlotTarget::Serial(SccCh::B),
        BusKind::Nubus | BusKind::LcPds | BusKind::Se30Pds | BusKind::BuiltIn => SlotTarget::Card,
    }
}

fn settings_map(settings: &impl Serialize) -> Map<String, Value> {
    to_map(settings).unwrap_or_default()
}

/// The list a machine with these buses can be created from. `statuses` has the function's
/// attributes, such as `media`, in the shape of `buses`.
pub fn device_configs(
    buses: &[Bus],
    statuses: &[Vec<Option<FunctionStatus>>],
    mac: MacModel,
) -> Vec<DeviceConfig> {
    // What the machine has without being told, so need not be saved
    let layout = mac.buses();
    let default_model = |bus: &Bus, slot: &Slot| {
        let bus = layout.iter().find(|b| b.id == bus.id)?;
        let slot = bus.slots.iter().find(|s| s.id == slot.id)?;
        Some(slot.device.as_ref()?.model)
    };
    let mut configs = vec![];
    for (bus_idx, bus) in buses.iter().enumerate() {
        for (slot_idx, slot) in bus.slots.iter().enumerate() {
            let Some(device) = &slot.device else {
                continue;
            };
            let mut settings = device.settings.clone();
            if let Some(Some(status)) = statuses.get(bus_idx).and_then(|b| b.get(slot_idx)) {
                settings.extend(status.settings());
            }
            let is_default = default_model(bus, slot) == Some(device.model);
            if is_default && settings.is_empty() {
                continue;
            }
            configs.push(DeviceConfig {
                bus: bus.id.clone(),
                slot: slot.id.clone(),
                model: (!is_default).then(|| device.model.key().to_owned()),
                settings,
            });
        }
    }
    configs
}

/// A drive on the Apple IIe Card's chain, as the Floppy function sees it
struct IieDrive<'a> {
    card: &'a mut PdsIIeCard,
    model: DeviceModel,
    /// Its position on the chain
    position: usize,
    /// The card numbers the 5.25" drives among themselves
    number: usize,
}

impl IieDrive<'_> {
    const DISK_TYPES: &'static [&'static str] = &["dsk", "do", "po", "2mg", "woz"];
    const UNIDISK_TYPES: &'static [&'static str] = &["po", "2mg"];
}

impl Floppy for IieDrive<'_> {
    fn update_status(&mut self, status: &mut FloppyStatus) -> bool {
        let media = self.card.chain_media[self.position].clone();
        let (image_label, image_types) = if self.model == DeviceModel::UniDisk35 {
            ("Apple II 3.5\" disks", Self::UNIDISK_TYPES)
        } else {
            ("Apple II 5.25\" disks", Self::DISK_TYPES)
        };
        let new = FloppyStatus {
            title: media
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
            ejected: media.is_none(),
            settings: FloppySettings {
                read_only: media.is_some() && self.card.chain_read_only[self.position],
                media,
            },
            image_label,
            image_types: image_types.to_vec(),
            ..FloppyStatus::default()
        };
        if *status == new {
            return false;
        }
        *status = new;
        true
    }

    fn load(&mut self, media: &Path, read_only: bool) -> Result<()> {
        let bytes = std::fs::read(media)?;
        if self.model == DeviceModel::UniDisk35 {
            // A 2IMG header carries the lock flag; a raw image is the blocks alone
            let (data, locked) = if TwoImg::detect(&bytes) {
                let img = TwoImg::parse(&bytes)?;
                (img.data.to_vec(), img.locked)
            } else {
                (bytes, false)
            };
            let locked = locked || read_only;
            self.card.insert_unidisk(data, locked)?;
            self.card.chain_read_only[self.position] = locked;
        } else {
            // A 2IMG header names the sector order, else the extension does
            let (bytes, order) = if TwoImg::detect(&bytes) {
                let img = TwoImg::parse(&bytes)?;
                let order = if img.format == TwoImg::FORMAT_PRODOS {
                    DskOrder::Prodos
                } else {
                    DskOrder::Dos
                };
                (img.data, order)
            } else if media
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("po"))
            {
                (&bytes[..], DskOrder::Prodos)
            } else {
                (&bytes[..], DskOrder::Dos)
            };
            self.card.insert_disk(self.number, bytes, order)?;
            // The card's 5.25" drives don't write yet
            self.card.chain_read_only[self.position] = true;
        }
        self.card.chain_media[self.position] = Some(media.to_path_buf());
        Ok(())
    }

    fn eject(&mut self) {
        if self.model == DeviceModel::UniDisk35 {
            self.card.eject_unidisk();
        } else {
            self.card.eject_disk(self.number);
        }
        self.card.chain_media[self.position] = None;
        self.card.chain_read_only[self.position] = false;
    }
}

/// The floppy status in a slot's status, made one if it wasn't
fn floppy_status(status: &mut Option<FunctionStatus>) -> (&mut FloppyStatus, bool) {
    let fresh = !matches!(status, Some(FunctionStatus::Floppy(_)));
    if fresh {
        *status = Some(FunctionStatus::Floppy(FloppyStatus::default()));
    }
    let Some(FunctionStatus::Floppy(floppy)) = status else {
        unreachable!()
    };
    (floppy, fresh)
}

impl Emulator {
    /// The machine as it is now. It only changes when a device is added or removed.
    pub fn buses(&self) -> Arc<Vec<Bus>> {
        Arc::clone(&self.bus_list)
    }

    /// The status of every device, which the frontend reads
    pub fn status_block(&self) -> Arc<StatusBlock> {
        Arc::clone(&self.status_block)
    }

    /// The list a machine like this one can be created from
    pub fn devices(&mut self) -> Vec<DeviceConfig> {
        let buses = self.buses();
        let statuses: Vec<Vec<_>> = buses
            .iter()
            .map(|bus| {
                bus.slots
                    .iter()
                    .map(|slot| {
                        let mut status = None;
                        self.update_slot_status(bus, slot, &mut status);
                        status
                    })
                    .collect()
            })
            .collect();
        device_configs(&buses, &statuses, self.model)
    }

    /// Finds a slot from what a workspace knows of it
    pub fn find_slot(&self, bus: &str, slot: &str) -> Option<SlotAddr> {
        let bus_idx = self.bus_list.iter().position(|b| b.id == bus)?;
        let slot = self.bus_list[bus_idx]
            .slots
            .iter()
            .position(|s| s.id == slot)?;
        Some(SlotAddr { bus: bus_idx, slot })
    }

    /// Builds the bus list again, after a device came or went, and tells the frontend
    pub(super) fn rebuild_bus_list(&mut self) {
        let mut buses = self.model.buses();
        if !self.device_book.iie_chain.is_empty()
            && let Some(pds) = buses.iter().position(|b| b.kind == BusKind::LcPds)
        {
            buses.insert(pds + 1, MacModel::iie_disk_bus(&self.device_book.iie_chain));
        }
        for bus in &mut buses {
            let mut slots = std::mem::take(&mut bus.slots);
            for slot in &mut slots {
                slot.device = self.device_in(bus, slot);
            }
            bus.slots = slots;
        }
        self.bus_list = Arc::new(buses);
        let _ = self
            .event_sender
            .send(EmulatorEvent::Buses(Arc::clone(&self.bus_list)));
        // A new device's first status shows, whatever the devices say
        self.status_block.generation.fetch_add(1, Ordering::Release);
    }

    /// After a state is restored: the state has the devices but not where they came from, so
    /// this takes it from the list saved with it
    pub(super) fn reinstate_devices(&mut self, devices: Vec<DeviceConfig>) {
        let layout = self.model.buses();
        let mut chain = vec![];
        for config in devices {
            let in_layout = || {
                let bus = layout.iter().find(|b| b.id == config.bus)?;
                bus.slots
                    .iter()
                    .find(|s| s.id == config.slot)?
                    .device
                    .clone()
            };
            let Some(model) = config
                .known_model()
                .or_else(|| in_layout().map(|d| d.model))
            else {
                continue;
            };
            let media = config.parse::<FloppySettings>().ok();
            match model.info().bus {
                BusKind::Nubus | BusKind::BuiltIn | BusKind::LcPds | BusKind::Se30Pds => {
                    self.device_book.cards.push(DeviceConfig {
                        model: Some(model.key().to_owned()),
                        ..config
                    });
                }
                BusKind::IieDisk => {
                    chain.push((config.slot.clone(), model, media));
                }
                BusKind::Swim if model == DeviceModel::Hd20 => {
                    let position = config
                        .slot
                        .strip_prefix(HD20_SLOT)
                        .and_then(|p| p.parse::<usize>().ok());
                    if let (Some(position), Some(media)) = (position, media.and_then(|m| m.media))
                        && (1..=MAX_DCDS).contains(&position)
                    {
                        self.device_book.restored_hd20[position - 1] = Some(media);
                    }
                }
                BusKind::Swim => {
                    let Ok(drive) = config.slot.parse::<usize>() else {
                        continue;
                    };
                    let media = media.unwrap_or_default();
                    if let Some(drive) = self.config.swim_mut().drives.get_mut(drive - 1)
                        && drive.floppy_inserted
                    {
                        drive.media_read_only = media.read_only;
                        drive.media_path = media.media;
                    }
                }
                BusKind::Scsi => {
                    let Ok(id) = config.slot.parse::<usize>() else {
                        continue;
                    };
                    // A BlueSCSI's folder is its own, not a copy in the state
                    if model != DeviceModel::BlueScsi
                        && let Some(media) = media.and_then(|m| m.media)
                    {
                        self.device_book.restored_media[id] = Some(media);
                    }
                }
                BusKind::Serial => {
                    // The host's side of a link isn't in the state: it starts again
                    self.rebuild_bus_list();
                    if let Err(e) = self.add_device(&config) {
                        log::warn!("{}: {:#}", config.label(), e);
                    }
                }
            }
        }
        chain.sort_by(|a, b| a.0.cmp(&b.0));
        self.device_book.iie_chain = chain.iter().map(|(_, model, _)| *model).collect();
        if let Some(card) = self.iie_card_mut() {
            for (position, (_, _, media)) in chain.into_iter().enumerate().take(2) {
                let media = media.unwrap_or_default();
                card.chain_read_only[position] = media.read_only;
                card.chain_media[position] = media.media;
            }
        }
        self.rebuild_bus_list();
    }

    /// Gives every device its turn to update its status. The frontend may be copying the block:
    /// the next pass catches up then.
    pub(super) fn publish_status(&mut self) {
        let block = Arc::clone(&self.status_block);
        let Ok(mut statuses) = block.statuses.try_lock() else {
            return;
        };
        let buses = self.buses();
        let mut changed = false;
        let same_shape = statuses.len() == buses.len()
            && statuses
                .iter()
                .zip(buses.iter())
                .all(|(s, b)| s.len() == b.slots.len());
        if !same_shape {
            *statuses = buses.iter().map(|b| vec![None; b.slots.len()]).collect();
            changed = true;
        }
        for (bus_idx, bus) in buses.iter().enumerate() {
            for (slot_idx, slot) in bus.slots.iter().enumerate() {
                changed |= self.update_slot_status(bus, slot, &mut statuses[bus_idx][slot_idx]);
            }
        }
        if changed {
            block.generation.fetch_add(1, Ordering::Release);
        }
    }

    /// Slot -> real object -> function -> its status. True if it changed.
    fn update_slot_status(
        &mut self,
        bus: &Bus,
        slot: &Slot,
        status: &mut Option<FunctionStatus>,
    ) -> bool {
        let Some(device) = &slot.device else {
            return status.take().is_some();
        };
        match slot_target(bus, slot) {
            SlotTarget::Drive(i) => {
                let (floppy, fresh) = floppy_status(status);
                let Some(drive) = self.config.swim_mut().drives.get_mut(i) else {
                    return fresh;
                };
                drive.update_status(floppy) | fresh
            }
            SlotTarget::IieDrive(i) => {
                let (floppy, fresh) = floppy_status(status);
                let Some(mut drive) = self.iie_drive(i) else {
                    return fresh;
                };
                drive.update_status(floppy) | fresh
            }
            target => {
                let new = self.computed_status(target, device);
                if *status == new {
                    return false;
                }
                *status = new;
                true
            }
        }
    }

    /// The status of the devices that don't implement their function's trait yet
    fn computed_status(&self, target: SlotTarget, device: &Device) -> Option<FunctionStatus> {
        match target {
            SlotTarget::Scsi(id) => self.scsi_status(id),
            SlotTarget::Hd20(i) => {
                let dcd = self.config.swim().dcds.get(i)?;
                // A restored machine runs on copies: what matters is where they came from
                let media = self.device_book.restored_hd20[i]
                    .clone()
                    .or_else(|| dcd.image_path().map(Path::to_path_buf))?;
                Some(FunctionStatus::HardDisk(HardDiskStatus {
                    settings: HardDiskSettings {
                        media,
                        read_only: !dcd.writable(),
                    },
                    capacity: Some(dcd.blocks() * DCD_BLOCKSIZE),
                    can_branch: false,
                }))
            }
            SlotTarget::Card => Some(match device.model.function() {
                FunctionKind::AppleIIe => FunctionStatus::AppleIIe,
                _ => FunctionStatus::Video,
            }),
            SlotTarget::Serial(ch) => {
                let bridge = self.serial_bridges[ch as usize]
                    .as_ref()
                    .map(|b| b.status());
                Some(match device.model.function() {
                    FunctionKind::LocalTalk => FunctionStatus::LocalTalk(bridge),
                    _ => FunctionStatus::SerialLink(bridge),
                })
            }
            SlotTarget::Drive(_) | SlotTarget::IieDrive(_) => None,
        }
    }

    /// The drive at this position of the Apple IIe Card's chain
    fn iie_drive(&mut self, position: usize) -> Option<IieDrive<'_>> {
        let model = *self.device_book.iie_chain.get(position)?;
        let number = self.device_book.iie_chain[..position]
            .iter()
            .filter(|m| **m == DeviceModel::Apple525Drive)
            .count();
        Some(IieDrive {
            card: self.iie_card_mut()?,
            model,
            position,
            number,
        })
    }

    /// Builds a machine with these devices: the ones it can't use are returned
    pub fn new_with_devices(
        machine: &MachineConfig,
        devices: Vec<DeviceConfig>,
    ) -> Result<NewMachine> {
        let plan = BuildPlan::new(machine.model, devices);
        let mut extra_roms = vec![];
        let mut video_slot = 0x9;
        if let Some((slot, device, rom)) = &plan.video {
            video_slot = *slot;
            extra_roms.push(match device {
                DeviceModel::MacIIVideoCard => ExtraROMs::Toby(rom),
                DeviceModel::SE30Video => ExtraROMs::SE30Video(rom),
                _ => ExtraROMs::MDC12(rom),
            });
        }
        if let Some(rom) = machine.extension_rom {
            extra_roms.push(ExtraROMs::ExtensionROM(rom));
        }
        let (mut emu, frame_recv) = Self::new_with_extra(
            machine.rom,
            &extra_roms,
            machine.model,
            machine.monitor,
            machine.mouse_mode,
            machine.ram_size,
            plan.override_drive,
            machine.pmmu_enabled,
            machine.fpu_enabled,
            machine.vram_size,
            plan.iie_card,
            video_slot,
        )?;
        emu.device_book.cards = plan.cards;
        emu.rebuild_bus_list();

        let mut refused = plan.refused;
        let mut rest = plan.rest;
        // HD20s attach at the next free position, so they go last, in slot order
        rest.sort_by_key(|c| {
            let hd20 = c.bus == bus_id::SWIM && c.slot.starts_with(HD20_SLOT);
            (hd20, if hd20 { c.slot.clone() } else { String::new() })
        });
        for config in rest {
            if let Err(e) = emu.add_device(&config) {
                refused.push(RefusedDevice {
                    config,
                    reason: format!("{:#}", e),
                });
            }
        }
        emu.status_update()?;
        Ok((emu, frame_recv, refused))
    }

    /// If the slot has a device, hands it the settings. If not, creates one.
    pub fn add_device(&mut self, config: &DeviceConfig) -> Result<()> {
        let buses = self.buses();
        let bus = buses
            .iter()
            .find(|b| b.id == config.bus)
            .context("No such bus on this machine")?;
        let slot = bus
            .slots
            .iter()
            .find(|s| s.id == config.slot)
            .context("No such slot on this machine")?;
        match &slot.device {
            Some(device) => {
                if let Some(model) = &config.model
                    && model != device.model.key()
                {
                    bail!("This slot holds a {}", device.model.name());
                }
                self.configure_device(slot_target(bus, slot), device.model, config)
            }
            None => {
                self.create_device(bus, slot, config)?;
                self.rebuild_bus_list();
                Ok(())
            }
        }
    }

    /// Takes a device out of its slot, if it can be
    pub fn remove_device(&mut self, addr: SlotAddr) -> Result<()> {
        let buses = self.buses();
        let bus = buses.get(addr.bus).context("No such bus")?;
        let slot = bus.slots.get(addr.slot).context("No such slot")?;
        let Some(device) = &slot.device else {
            bail!("There is no device in this slot");
        };
        if device.model.fixed() {
            bail!("A {} can't be removed", device.model.name());
        }
        match slot_target(bus, slot) {
            SlotTarget::Scsi(id) => {
                self.config.scsi_mut().detach_target(id);
                self.device_book.restored_media[id] = None;
            }
            SlotTarget::Serial(ch) => self.serial_bridge_disable(ch)?,
            SlotTarget::Hd20(i) => {
                // The ones behind it move up
                self.config.swim_mut().detach_dcd(i)?;
                let restored = &mut self.device_book.restored_hd20;
                restored[i..].rotate_left(1);
                restored[MAX_DCDS - 1] = None;
            }
            t => bail!("{:?} doesn't take removable devices", t),
        }
        info!("{} #{}: device removed", bus.id, slot.id);
        self.rebuild_bus_list();
        Ok(())
    }

    fn create_device(&mut self, bus: &Bus, slot: &Slot, config: &DeviceConfig) -> Result<()> {
        let key = config
            .model
            .as_deref()
            .context("This slot is empty, and the entry doesn't say what to put in it")?;
        let model =
            DeviceModel::from_key(key).with_context(|| format!("Unknown device {}", key))?;
        let info = models_for(bus.kind, self.model)
            .find(|i| i.model == model)
            .with_context(|| format!("{} doesn't take a {}", bus.id, model.name()))?;
        if info.fixed {
            bail!("A {} can only be chosen with the machine", model.name());
        }
        if !bus.can_take(info) {
            bail!("A {} can't go here right now", model.name());
        }
        match (slot_target(bus, slot), model) {
            (SlotTarget::Scsi(id), DeviceModel::ScsiHardDisk) => {
                let settings: HardDiskSettings = config.parse()?;
                if settings.read_only {
                    bail!("SCSI disks can't be read-only yet");
                }
                self.load_hdd_image(&settings.media, id)
            }
            (SlotTarget::Hd20(_), DeviceModel::Hd20) => {
                // It takes the next free position: the slot id only orders the entries, so
                // one whose image is missing doesn't stop the ones behind it
                let settings: HardDiskSettings = config.parse()?;
                let dcd = DcdDevice::open(&settings.media, !settings.read_only)?;
                self.config.swim_mut().attach_dcd(dcd)?;
                Ok(())
            }
            (SlotTarget::Scsi(id), DeviceModel::ScsiCdrom) => {
                let settings: CdromSettings = config.parse()?;
                match settings.media {
                    Some(dir) if dir.is_dir() => self.attach_cdrom_folder(id, dir),
                    Some(image) => {
                        self.attach_cdrom(id);
                        self.scsi_load_media(id, &image)
                    }
                    None => {
                        self.attach_cdrom(id);
                        Ok(())
                    }
                }
            }
            #[cfg(feature = "ethernet")]
            (SlotTarget::Scsi(id), DeviceModel::DaynaPortScsiLink) => {
                self.attach_ethernet(id);
                self.configure_device(SlotTarget::Scsi(id), model, config)
            }
            #[cfg(feature = "printer")]
            (SlotTarget::Scsi(id), DeviceModel::LaserWriterIISC) => {
                let settings: LaserWriterIISCSettings = config.parse()?;
                self.attach_printer(id, settings.output_dir.unwrap_or_else(std::env::temp_dir));
                Ok(())
            }
            (SlotTarget::Scsi(id), DeviceModel::BlueScsi) => {
                let settings: FileTransferSettings = config.parse()?;
                if let Some(folder) = &settings.media
                    && !folder.is_dir()
                {
                    bail!("{} is not a folder", folder.display());
                }
                self.config
                    .scsi_mut()
                    .attach_bluescsi_at(id, settings.media)
            }
            (SlotTarget::Serial(ch), DeviceModel::SerialBridge) => {
                let settings: SerialLinkSettings = config.parse()?;
                self.serial_bridge_enable(
                    ch,
                    match settings.tcp_port {
                        Some(port) => SerialBridgeConfig::Tcp(port),
                        None => SerialBridgeConfig::Pty,
                    },
                )
            }
            (SlotTarget::Serial(ch), DeviceModel::LocalTalkAdapter) => {
                self.serial_bridge_enable(ch, SerialBridgeConfig::LocalTalk)
            }
            (t, model) => bail!("{:?} can't take a {}", t, model.name()),
        }
    }

    /// Hands a device that's already there its settings
    fn configure_device(
        &mut self,
        target: SlotTarget,
        model: DeviceModel,
        config: &DeviceConfig,
    ) -> Result<()> {
        match target {
            SlotTarget::Drive(_) | SlotTarget::IieDrive(_) => {
                let settings: FloppySettings = config.parse()?;
                match settings.media {
                    Some(media) => self.load_target(target, &media, settings.read_only),
                    None => Ok(()),
                }
            }
            #[cfg(feature = "ethernet")]
            SlotTarget::Scsi(id) if model == DeviceModel::DaynaPortScsiLink => {
                // A link this build doesn't have shouldn't cost the device
                if let Some(link) = config.settings.get("link") {
                    let link = serde_json::from_value(link.clone())
                        .map_err(|_| anyhow!("Unknown Ethernet link {}", link))?;
                    self.config.scsi_mut().targets[id]
                        .as_mut()
                        .and_then(|t| t.ethernet_mut())
                        .context("No Ethernet device attached")?
                        .set_link(link)?;
                }
                Ok(())
            }
            SlotTarget::Scsi(id) if model == DeviceModel::ScsiCdrom => {
                let settings: CdromSettings = config.parse()?;
                match settings.media {
                    Some(media) => self.scsi_load_media(id, &media),
                    None => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    fn target_at(&self, addr: SlotAddr) -> Result<SlotTarget> {
        let bus = self.bus_list.get(addr.bus).context("No such bus")?;
        let slot = bus.slots.get(addr.slot).context("No such slot")?;
        Ok(slot_target(bus, slot))
    }

    /// The floppy drive in a slot
    pub(super) fn drive_at(&self, addr: SlotAddr) -> Result<usize> {
        match self.target_at(addr)? {
            SlotTarget::Drive(i) => Ok(i),
            t => bail!("{:?} is not a floppy drive", t),
        }
    }

    /// The SCSI ID of a slot
    pub(super) fn scsi_id_at(&self, addr: SlotAddr) -> Result<usize> {
        match self.target_at(addr)? {
            SlotTarget::Scsi(id) => Ok(id),
            t => bail!("{:?} is not on SCSI", t),
        }
    }

    pub(super) fn branch_disk(&mut self, addr: SlotAddr, path: &Path) -> Result<()> {
        let id = self.scsi_id_at(addr)?;
        self.config.scsi_mut().targets[id]
            .as_mut()
            .context("No target attached")?
            .branch_media(path)?;
        self.device_book.restored_media[id] = None;
        Ok(())
    }

    /// Puts an image in the drive in a slot
    pub(super) fn load_media(
        &mut self,
        addr: SlotAddr,
        path: &Path,
        read_only: bool,
    ) -> Result<()> {
        let target = self.target_at(addr)?;
        self.load_target(target, path, read_only)
    }

    fn load_target(&mut self, target: SlotTarget, path: &Path, read_only: bool) -> Result<()> {
        match target {
            SlotTarget::Drive(i) => {
                let drive = self
                    .config
                    .swim_mut()
                    .drives
                    .get_mut(i)
                    .context("No such drive")?;
                Floppy::load(drive, path, read_only)
            }
            SlotTarget::IieDrive(i) => self
                .iie_drive(i)
                .context("No such drive")?
                .load(path, read_only),
            SlotTarget::Scsi(id) => {
                self.scsi_load_media(id, path)?;
                self.device_book.restored_media[id] = None;
                Ok(())
            }
            t => bail!("{:?} doesn't take media", t),
        }
    }

    /// Takes the image out of the drive in a slot
    pub(super) fn eject_media(&mut self, addr: SlotAddr) -> Result<()> {
        match self.target_at(addr)? {
            SlotTarget::Drive(i) => {
                // The writes since the last save must not be lost
                self.try_writeback(i);
                Floppy::eject(&mut self.config.swim_mut().drives[i]);
                Ok(())
            }
            SlotTarget::IieDrive(i) => {
                self.iie_drive(i).context("No such drive")?.eject();
                Ok(())
            }
            SlotTarget::Scsi(id) => {
                let target = self.config.scsi_mut().targets[id]
                    .as_mut()
                    .context("No target attached")?;
                let Some(transfer) = target.file_transfer_mut() else {
                    bail!("This SCSI device has nothing to eject from here");
                };
                transfer.eject();
                Ok(())
            }
            t => bail!("{:?} can't eject from here", t),
        }
    }

    fn scsi_load_media(&mut self, id: usize, path: &Path) -> Result<()> {
        self.config.scsi_mut().targets[id]
            .as_mut()
            .context("No target attached")?
            .load_media(path)
    }

    pub(super) fn iie_card_mut(&mut self) -> Option<&mut PdsIIeCard> {
        match &mut self.config {
            EmulatorConfig::MacLC(inner) => inner.bus.iie_card_mut(),
            EmulatorConfig::MacLCII(inner) => inner.bus.iie_card_mut(),
            EmulatorConfig::MacLCIII(inner) => inner.bus.iie_card_mut(),
            EmulatorConfig::LC475(inner) => inner.bus.iie_card_mut(),
            _ => None,
        }
    }

    /// Plugs a host link in a serial port
    fn serial_bridge_enable(&mut self, ch: SccCh, config: SerialBridgeConfig) -> Result<()> {
        let bridge = serial_device(&config)?;
        let status = bridge.status();
        info!("SCC bridge enabled on channel {:?}: {}", ch, status);
        match &status {
            SerialBridgeStatus::Pty(path) => {
                self.user_notice(&format!("Serial bridge PTY: {}", path.display()));
            }
            SerialBridgeStatus::LocalTalk(_) => {
                self.user_notice("LocalTalk bridge enabled");
            }
            _ => {}
        }
        self.serial_bridges[ch as usize] = Some(bridge);
        self.device_book.serial[ch as usize] = Some(config);
        Ok(())
    }

    /// Unplugs it
    fn serial_bridge_disable(&mut self, ch: SccCh) -> Result<()> {
        self.device_book.serial[ch as usize] = None;
        if self.serial_bridges[ch as usize].take().is_some() {
            info!("Serial bridge disabled on channel {:?}", ch);
        }
        Ok(())
    }

    /// What is in a slot, with the device model's own attributes
    fn device_in(&self, bus: &Bus, slot: &Slot) -> Option<Device> {
        let device = |model, settings| Some(Device { model, settings });
        match slot_target(bus, slot) {
            SlotTarget::Drive(i) => {
                let drive = self.config.swim().drives.get(i)?;
                device(DeviceModel::from_drive_type(drive.drive_type)?, Map::new())
            }
            SlotTarget::Hd20(i) => {
                self.config.swim().dcds.get(i)?;
                device(DeviceModel::Hd20, Map::new())
            }
            SlotTarget::Scsi(id) => {
                let model = match self.config.scsi().get_target_type(id)? {
                    ScsiTargetType::Disk => DeviceModel::ScsiHardDisk,
                    ScsiTargetType::Cdrom => DeviceModel::ScsiCdrom,
                    ScsiTargetType::BlueScsi => DeviceModel::BlueScsi,
                    #[cfg(feature = "ethernet")]
                    ScsiTargetType::Ethernet => DeviceModel::DaynaPortScsiLink,
                    #[cfg(feature = "printer")]
                    ScsiTargetType::Printer => DeviceModel::LaserWriterIISC,
                };
                device(model, Map::new())
            }
            SlotTarget::Card => {
                let config = self
                    .device_book
                    .cards
                    .iter()
                    .find(|c| c.bus == bus.id && c.slot == slot.id)?;
                device(config.known_model()?, config.settings.clone())
            }
            SlotTarget::IieDrive(i) => device(*self.device_book.iie_chain.get(i)?, Map::new()),
            SlotTarget::Serial(ch) => match self.device_book.serial[ch as usize].as_ref()? {
                SerialBridgeConfig::Pty => device(DeviceModel::SerialBridge, Map::new()),
                SerialBridgeConfig::Tcp(port) => device(
                    DeviceModel::SerialBridge,
                    settings_map(&SerialLinkSettings {
                        tcp_port: Some(*port),
                    }),
                ),
                SerialBridgeConfig::LocalTalk => device(DeviceModel::LocalTalkAdapter, Map::new()),
            },
        }
    }

    fn scsi_status(&self, id: usize) -> Option<FunctionStatus> {
        let scsi = self.config.scsi();
        // A restored machine runs on copies: what matters is where they came from
        let image = self.device_book.restored_media[id]
            .clone()
            .or_else(|| scsi.get_disk_imagefn(id).map(|p| p.to_path_buf()));
        match scsi.get_target_type(id)? {
            ScsiTargetType::Disk => Some(FunctionStatus::HardDisk(HardDiskStatus {
                settings: HardDiskSettings {
                    media: image?,
                    read_only: false,
                },
                capacity: scsi.get_disk_capacity(id),
                can_branch: true,
            })),
            ScsiTargetType::Cdrom => {
                let folder = scsi.get_cdrom_folder(id).map(|p| p.to_path_buf());
                Some(FunctionStatus::Cdrom(CdromStatus {
                    settings: CdromSettings {
                        media: folder.clone().or_else(|| image.clone()),
                    },
                    image,
                    folder,
                }))
            }
            #[cfg(feature = "ethernet")]
            ScsiTargetType::Ethernet => {
                let ethernet = scsi.targets[id].as_ref()?.ethernet()?;
                Some(FunctionStatus::Ethernet(EthernetStatus {
                    link: ethernet.link(),
                    capture: ethernet.capture_status(),
                }))
            }
            #[cfg(feature = "printer")]
            ScsiTargetType::Printer => Some(FunctionStatus::Printer),
            ScsiTargetType::BlueScsi => {
                let transfer = scsi.targets[id].as_ref()?.file_transfer()?;
                Some(FunctionStatus::FileTransfer(FileTransferStatus {
                    settings: FileTransferSettings {
                        media: transfer.folder().map(Path::to_path_buf),
                    },
                }))
            }
        }
    }
}

/// The Floppy function of a Mac floppy drive
impl Floppy for FloppyDrive {
    fn update_status(&mut self, status: &mut FloppyStatus) -> bool {
        let media = self
            .floppy_inserted
            .then(|| self.media_path.clone())
            .flatten();
        let mut blank_formats = vec![FloppyType::Mac800K];
        if self.drive_type == DriveType::SuperDrive {
            blank_formats.push(FloppyType::Mfm144M);
        }
        let new = FloppyStatus {
            settings: FloppySettings {
                read_only: media.is_some() && self.media_read_only,
                media,
            },
            title: self.floppy.get_title().to_owned(),
            ejected: !self.floppy_inserted,
            dirty: self.floppy.is_dirty(),
            can_save: true,
            writeback_supported: self.image_path.is_some() && self.floppy.supports_writeback(),
            writeback_enabled: self.writeback_enabled,
            blank_formats,
            image_label: "Floppy images",
            image_types: snow_floppy::loaders::ImageType::all_extensions(),
            rpm_control: self.drive_type.has_pwm_control(),
        };
        if *status == new {
            return false;
        }
        *status = new;
        true
    }

    fn load(&mut self, media: &Path, read_only: bool) -> Result<()> {
        if !self.is_present() {
            bail!("No drive here");
        }
        let mut image = Autodetect::load_file(&media.to_string_lossy())
            .with_context(|| format!("Cannot load {}", media.display()))?;
        if read_only {
            image.set_force_wp();
        }
        let writeback_path = image.supports_writeback().then(|| media.to_path_buf());
        self.disk_insert(image)?;
        self.media_path = Some(media.to_path_buf());
        self.media_read_only = read_only;
        self.image_path = writeback_path;
        self.writeback_enabled = false;
        self.pending_writeback = false;
        Ok(())
    }

    fn eject(&mut self) {
        Self::eject(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    /// A machine that does nothing: enough to look at its devices
    fn machine(model: MacModel) -> Emulator {
        let rom = vec![0; 128 * 1024];
        Emulator::new(&rom, &[], model).unwrap().0
    }

    fn slots(model: MacModel, bus: &str) -> Vec<String> {
        model
            .buses()
            .into_iter()
            .find(|b| b.id == bus)
            .map(|b| b.slots.into_iter().map(|s| s.id).collect())
            .unwrap_or_default()
    }

    #[test]
    fn layouts_come_from_the_model() {
        assert_eq!(
            slots(MacModel::MacII, bus_id::NUBUS),
            ["9", "A", "B", "C", "D", "E"]
        );
        assert_eq!(slots(MacModel::MacIIcx, bus_id::NUBUS), ["9", "A", "B"]);
        assert_eq!(
            slots(MacModel::Plus, bus_id::SWIM),
            ["1", "2", "HD20.1", "HD20.2", "HD20.3", "HD20.4"]
        );
        assert_eq!(slots(MacModel::Plus, bus_id::SCSI).len(), 7);
        assert!(slots(MacModel::Early128K, bus_id::SCSI).is_empty());
        assert_eq!(slots(MacModel::LC, bus_id::PDS_LC), ["0"]);
        assert_eq!(slots(MacModel::SE30, bus_id::PDS_SE30), ["0"]);
        assert!(slots(MacModel::SE30, bus_id::NUBUS).is_empty());
        assert!(slots(MacModel::SE30, bus_id::PDS_LC).is_empty());
    }

    #[test]
    fn every_key_comes_back_as_its_model() {
        for model in DeviceModel::iter() {
            assert_eq!(DeviceModel::from_key(model.key()), Some(model));
        }
        assert_eq!(DeviceModel::from_key("SomeFutureCard"), None);
    }

    #[test]
    fn a_config_is_the_file_entry() {
        let cfg = DeviceConfig::new(
            bus_id::SCSI,
            "0",
            &ScsiHardDiskSettings {
                disk: HardDiskSettings {
                    media: "a.img".into(),
                    read_only: false,
                },
            },
        )
        .unwrap();
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(
            json,
            r#"{"bus":"SCSI","slot":"0","device":"ScsiHardDisk","media":"a.img"}"#
        );
        let back: DeviceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.known_model(), Some(DeviceModel::ScsiHardDisk));
    }

    #[test]
    fn a_config_carries_what_this_build_does_not_know() {
        let json =
            r#"{"bus":"NuBus","slot":"A","device":"SomeFutureCard","rom":"x.rom","fast":true}"#;
        let cfg: DeviceConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.known_model(), None);
        assert!(cfg.path_keys().is_empty());
        let back: Value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(back, serde_json::from_str::<Value>(json).unwrap());
    }

    #[test]
    fn a_config_without_a_model_is_for_the_device_in_the_slot() {
        let cfg: DeviceConfig =
            serde_json::from_str(r#"{"bus":"SWIM","slot":"1","media":"a.img"}"#).unwrap();
        assert_eq!(cfg.model, None);
        assert_eq!(cfg.path_keys(), ["media"]);
        let settings: FloppySettings = cfg.parse().unwrap();
        assert_eq!(settings.media, Some("a.img".into()));
        assert!(!settings.read_only);
    }

    #[test]
    fn settings_ignore_what_they_do_not_know() {
        let cfg: DeviceConfig = serde_json::from_str(
            r#"{"bus":"SCSI","slot":"0","device":"ScsiHardDisk","media":"a.img","future":1}"#,
        )
        .unwrap();
        let settings: ScsiHardDiskSettings = cfg.parse().unwrap();
        assert_eq!(settings.disk.media, PathBuf::from("a.img"));
    }

    #[test]
    fn a_new_machine_has_its_drives_and_nothing_to_save() {
        let emu = machine(MacModel::Plus);
        let buses = emu.buses();
        let swim = buses.iter().find(|b| b.id == bus_id::SWIM).unwrap();
        // Two drives, and an HD20 chain with nothing on it
        assert_eq!(swim.slots.len(), 6);
        for slot in &swim.slots[..2] {
            let device = slot.device.as_ref().unwrap();
            assert_eq!(device.model, DeviceModel::Mac800KDrive);
        }
        assert!(swim.slots[2..].iter().all(|s| s.device.is_none()));
        let mut emu = emu;
        emu.status_update().unwrap();
        let block = emu.status_block();
        let statuses = block.statuses.lock().unwrap();
        let swim_idx = buses.iter().position(|b| b.id == bus_id::SWIM).unwrap();
        for status in &statuses[swim_idx][..2] {
            assert!(matches!(status, Some(FunctionStatus::Floppy(f)) if f.ejected));
        }
        drop(statuses);
        let scsi = buses.iter().find(|b| b.id == bus_id::SCSI).unwrap();
        assert!(scsi.slots.iter().all(|s| s.device.is_none()));
        assert_eq!(emu.devices(), vec![]);
    }

    #[test]
    fn slots_are_found_by_what_a_workspace_knows() {
        let emu = machine(MacModel::Plus);
        assert!(emu.find_slot(bus_id::SCSI, "3").is_some());
        assert!(emu.find_slot(bus_id::SCSI, "7").is_none());
        assert!(emu.find_slot(bus_id::NUBUS, "9").is_none());
    }

    struct TestFiles(PathBuf);

    impl TestFiles {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("snow_devices_{}_{}", name, std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(std::fs::canonicalize(dir).unwrap())
        }

        fn file(&self, name: &str, size: usize) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, vec![0u8; size]).unwrap();
            path
        }

        fn dir(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for TestFiles {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn build(model: MacModel, devices: Vec<DeviceConfig>) -> (Emulator, Vec<RefusedDevice>) {
        let rom = vec![0; 1024 * 1024];
        let (emu, _, refused) = Emulator::new_with_devices(
            &MachineConfig {
                rom: &rom,
                model,
                extension_rom: None,
                monitor: None,
                mouse_mode: MouseMode::default(),
                ram_size: None,
                pmmu_enabled: false,
                fpu_enabled: false,
                vram_size: None,
            },
            devices,
        )
        .unwrap();
        (emu, refused)
    }

    /// What a machine reports must create the same machine
    fn assert_round_trip(model: MacModel, devices: Vec<DeviceConfig>) -> Vec<DeviceConfig> {
        let (mut emu, refused) = build(model, devices);
        assert!(refused.is_empty(), "{:?}", refused);
        let reported = emu.devices();
        drop(emu);
        let (mut again, refused) = build(model, reported.clone());
        assert!(refused.is_empty(), "{:?}", refused);
        assert_eq!(again.devices(), reported);
        reported
    }

    fn scsi_disk(id: &str, media: PathBuf) -> DeviceConfig {
        DeviceConfig::new(
            bus_id::SCSI,
            id,
            &ScsiHardDiskSettings {
                disk: HardDiskSettings {
                    media,
                    read_only: false,
                },
            },
        )
        .unwrap()
    }

    fn scsi_cdrom(id: &str, media: Option<PathBuf>) -> DeviceConfig {
        DeviceConfig::new(
            bus_id::SCSI,
            id,
            &ScsiCdromSettings {
                cdrom: CdromSettings { media },
            },
        )
        .unwrap()
    }

    #[test]
    fn scsi_devices_round_trip() {
        let files = TestFiles::new("scsi");
        let devices = vec![
            scsi_disk("0", files.file("a.img", 1024 * 1024)),
            scsi_cdrom("2", Some(files.file("a.iso", 2048 * 512))),
            scsi_cdrom("3", None),
            scsi_cdrom("4", Some(files.dir("cds"))),
        ];
        let reported = assert_round_trip(MacModel::Plus, devices.clone());
        assert_eq!(reported, devices);
    }

    #[test]
    #[cfg(feature = "ethernet")]
    fn ethernet_round_trips() {
        use crate::mac::scsi::ethernet::EthernetLinkType;
        let config = DeviceConfig::of_model(
            bus_id::SCSI,
            "5",
            DeviceModel::DaynaPortScsiLink,
            &serde_json::json!({ "link": EthernetLinkType::Down }),
        )
        .unwrap();
        assert_eq!(
            assert_round_trip(MacModel::Plus, vec![config.clone()]),
            [config]
        );
    }

    #[test]
    #[cfg(feature = "printer")]
    fn the_printer_round_trips_without_its_host_folder() {
        let files = TestFiles::new("printer");
        let config = DeviceConfig::new(
            bus_id::SCSI,
            "6",
            &LaserWriterIISCSettings {
                output_dir: Some(files.dir("pages")),
            },
        )
        .unwrap();
        let reported = assert_round_trip(MacModel::Plus, vec![config]);
        assert_eq!(reported.len(), 1);
        assert!(reported[0].settings.is_empty());
    }

    #[test]
    fn bluescsis_each_share_their_own_folder() {
        let files = TestFiles::new("blue");
        let blue = |slot: &str, folder: Option<PathBuf>| {
            DeviceConfig::of_model(
                bus_id::SCSI,
                slot,
                DeviceModel::BlueScsi,
                &FileTransferSettings { media: folder },
            )
            .unwrap()
        };
        let one = blue("1", Some(files.dir("one")));
        let two = blue("2", Some(files.dir("two")));
        let none = blue("3", None);
        assert_eq!(
            assert_round_trip(MacModel::Plus, vec![one.clone(), two.clone(), none.clone()]),
            vec![one.clone(), two.clone(), none.clone()]
        );

        // Its slot is taken, and a folder must be a folder
        let (mut emu, refused) = build(
            MacModel::Plus,
            vec![
                one.clone(),
                scsi_cdrom("1", None),
                blue("4", Some(files.file("not-a-folder", 1))),
            ],
        );
        assert_eq!(refused.len(), 2);
        assert_eq!(emu.devices().len(), 1);

        // The folder changes and goes like a disk's image
        let addr = emu.find_slot(bus_id::SCSI, "1").unwrap();
        emu.load_media(addr, &files.dir("two"), false).unwrap();
        assert_eq!(emu.devices(), vec![blue("1", Some(files.dir("two")))]);
        emu.eject_media(addr).unwrap();
        assert_eq!(emu.devices(), vec![blue("1", None)]);
    }

    #[test]
    fn a_disk_goes_in_the_drive_of_the_machine_whichever_it_is() {
        let files = TestFiles::new("floppy");
        let media = files.file("a.img", 800 * 1024);
        let config = DeviceConfig::existing(
            bus_id::SWIM,
            "1",
            &FloppySettings {
                media: Some(media),
                read_only: true,
            },
        )
        .unwrap();
        for model in [MacModel::Plus, MacModel::SE, MacModel::MacIIx] {
            let (mut emu, refused) = if model == MacModel::MacIIx {
                let rom = files.file("card.rom", 32 * 1024);
                build(
                    model,
                    vec![
                        DeviceConfig::new(bus_id::NUBUS, "9", &DisplayCard824Settings { rom })
                            .unwrap(),
                        config.clone(),
                    ],
                )
            } else {
                build(model, vec![config.clone()])
            };
            assert!(refused.is_empty(), "{:?}", refused);
            let devices = emu.devices();
            let drive = devices.iter().find(|d| d.bus == bus_id::SWIM).unwrap();
            assert_eq!(drive, &config);
        }
    }

    #[test]
    fn another_drive_can_be_chosen_with_the_machine() {
        let config = DeviceConfig::of_model(
            bus_id::SWIM,
            "1",
            DeviceModel::Mac800KPwmDrive,
            &FloppySettings::default(),
        )
        .unwrap();
        let reported = assert_round_trip(MacModel::Early128K, vec![config.clone()]);
        assert!(reported.contains(&config));
        let (emu, _) = build(MacModel::Early128K, vec![config.clone()]);
        let buses = emu.buses();
        let swim = buses.iter().find(|b| b.id == bus_id::SWIM).unwrap();
        assert_eq!(
            swim.slots[0].device.as_ref().unwrap().model,
            DeviceModel::Mac800KPwmDrive
        );

        // Not on a machine that has no such option
        let (_, refused) = build(MacModel::Plus, vec![config]);
        assert_eq!(refused.len(), 1);
    }

    #[test]
    fn a_video_card_goes_in_the_slot_it_says() {
        let files = TestFiles::new("nubus");
        let card = DeviceConfig::new(
            bus_id::NUBUS,
            "A",
            &DisplayCard824Settings {
                rom: files.file("card.rom", 32 * 1024),
            },
        )
        .unwrap();
        assert_eq!(
            assert_round_trip(MacModel::MacIIx, vec![card.clone()]),
            vec![card.clone()]
        );

        // There is no slot E on a IIcx, and a machine has one card for now
        let (mut emu, refused) = build(
            MacModel::MacIIcx,
            vec![
                card.clone(),
                DeviceConfig {
                    slot: "E".into(),
                    ..card.clone()
                },
                DeviceConfig {
                    slot: "B".into(),
                    ..card.clone()
                },
            ],
        );
        assert_eq!(refused.len(), 2, "{:?}", refused);
        assert_eq!(emu.devices(), [card]);
    }

    #[test]
    fn the_se30_video_is_fixed() {
        let files = TestFiles::new("se30");
        let video = DeviceConfig::new(
            bus_id::BUILT_IN,
            "0",
            &SE30VideoSettings {
                rom: files.file("video.rom", 32 * 1024),
            },
        )
        .unwrap();
        // It's what this machine has there, so it needn't be named
        let reported = assert_round_trip(MacModel::SE30, vec![video.clone()]);
        assert_eq!(
            reported,
            [DeviceConfig {
                model: None,
                ..video
            }]
        );
    }

    #[test]
    fn the_iie_card_brings_a_bus_with_its_drives() {
        let files = TestFiles::new("iie");
        let devices = vec![
            DeviceConfig::new(
                bus_id::PDS_LC,
                "0",
                &AppleIIePdsCardSettings {
                    security_jumper: true,
                },
            )
            .unwrap(),
            DeviceConfig::of_model(
                bus_id::IIE_DISK,
                "1",
                DeviceModel::UniDisk35,
                &FloppySettings {
                    media: Some(files.file("a.po", 800 * 1024)),
                    read_only: false,
                },
            )
            .unwrap(),
            DeviceConfig::of_model(
                bus_id::IIE_DISK,
                "2",
                DeviceModel::Apple525Drive,
                // The card's 5.25" drives don't write yet, so they say so
                &FloppySettings {
                    media: Some(files.file("b.dsk", 143_360)),
                    read_only: true,
                },
            )
            .unwrap(),
        ];
        let (emu, refused) = build(MacModel::LC, devices.clone());
        assert!(refused.is_empty(), "{:?}", refused);
        let buses = emu.buses();
        let at = |id| buses.iter().position(|b: &Bus| b.id == id).unwrap();
        assert_eq!(at(bus_id::IIE_DISK), at(bus_id::PDS_LC) + 1);
        drop(emu);
        assert_eq!(assert_round_trip(MacModel::LC, devices.clone()), devices);
    }

    fn hd20(position: usize, media: PathBuf) -> DeviceConfig {
        DeviceConfig::of_model(
            bus_id::SWIM,
            &format!("{}{}", HD20_SLOT, position),
            DeviceModel::Hd20,
            &HardDiskSettings {
                media,
                read_only: false,
            },
        )
        .unwrap()
    }

    #[test]
    fn hd20s_take_the_next_free_position() {
        let files = TestFiles::new("hd20");
        // The file's order doesn't matter, the slot ids do
        let devices = vec![
            hd20(2, files.file("b.img", 1024 * 1024)),
            hd20(1, files.file("a.img", 1024 * 1024)),
        ];
        let (mut emu, refused) = build(MacModel::Plus, devices.clone());
        assert!(refused.is_empty(), "{:?}", refused);
        let reported = emu.devices();
        assert_eq!(reported, [devices[1].clone(), devices[0].clone()]);

        // The images can't be opened twice
        drop(emu);

        // A missing image doesn't stop the one behind it
        let devices = vec![
            hd20(1, files.0.join("gone.img")),
            hd20(2, files.file("c.img", 1024 * 1024)),
        ];
        let (mut emu, refused) = build(MacModel::Plus, devices);
        assert_eq!(refused.len(), 1);
        assert_eq!(emu.devices(), [hd20(1, files.0.join("c.img"))]);
        drop(emu);

        // Removing one moves the ones behind it up
        let (mut emu, _) = build(MacModel::Plus, reported);
        let first = emu.find_slot(bus_id::SWIM, "HD20.1").unwrap();
        emu.remove_device(first).unwrap();
        assert_eq!(emu.devices(), [hd20(1, files.0.join("b.img"))]);
    }

    #[cfg(feature = "savestates")]
    #[test]
    fn a_restored_machine_reports_its_devices() {
        let files = TestFiles::new("state");
        let devices = vec![
            DeviceConfig::new(bus_id::PDS_LC, "0", &AppleIIePdsCardSettings::default()).unwrap(),
            DeviceConfig::of_model(
                bus_id::IIE_DISK,
                "1",
                DeviceModel::UniDisk35,
                &FloppySettings {
                    media: Some(files.file("a.po", 800 * 1024)),
                    read_only: false,
                },
            )
            .unwrap(),
            scsi_disk("2", files.file("hd.img", 1024 * 1024)),
            DeviceConfig::of_model(
                bus_id::SCSI,
                "5",
                DeviceModel::BlueScsi,
                &FileTransferSettings {
                    media: Some(files.dir("shared")),
                },
            )
            .unwrap(),
        ];
        let (mut emu, refused) = build(MacModel::LC, devices);
        assert!(refused.is_empty(), "{:?}", refused);
        let reported = emu.devices();
        assert_eq!(reported.len(), 4);
        let state = files.0.join("machine.snows");
        emu.save_state(&state, None).unwrap();
        drop(emu);

        // It runs on a copy of the disk, and still says where it came from
        let (mut restored, _) = Emulator::load_state(&state, files.dir("tmp")).unwrap();
        assert_eq!(restored.devices(), reported);
    }

    #[test]
    fn the_iie_chain_is_checked() {
        let drive = |slot, model| {
            DeviceConfig::of_model(bus_id::IIE_DISK, slot, model, &FloppySettings::default())
                .unwrap()
        };
        let card =
            DeviceConfig::new(bus_id::PDS_LC, "0", &AppleIIePdsCardSettings::default()).unwrap();

        // A UniDisk must come first
        let (_, refused) = build(
            MacModel::LC,
            vec![
                card.clone(),
                drive("1", DeviceModel::Apple525Drive),
                drive("2", DeviceModel::UniDisk35),
            ],
        );
        assert_eq!(refused.len(), 1);

        // Nothing to plug the second drive into
        let (_, refused) = build(
            MacModel::LC,
            vec![card.clone(), drive("2", DeviceModel::Apple525Drive)],
        );
        assert_eq!(refused.len(), 1);

        // And no card
        let (_, refused) = build(MacModel::LC, vec![drive("1", DeviceModel::Apple525Drive)]);
        assert_eq!(refused.len(), 1);

        // The IIe doesn't go in an SE/30
        let files = TestFiles::new("iie_se30");
        let (_, refused) = build(
            MacModel::SE30,
            vec![
                DeviceConfig::new(
                    bus_id::BUILT_IN,
                    "0",
                    &SE30VideoSettings {
                        rom: files.file("v.rom", 32 * 1024),
                    },
                )
                .unwrap(),
                card,
            ],
        );
        assert_eq!(refused.len(), 1);
    }

    #[test]
    fn what_cant_be_used_is_refused_and_the_rest_works() {
        let files = TestFiles::new("refused");
        let good = scsi_disk("0", files.file("a.img", 1024 * 1024));
        let cases = [
            serde_json::json!({"bus":"SCSI","slot":"1","device":"SomeFutureDisk","media":"x"}),
            serde_json::json!({"bus":"SCSI","slot":"2"}),
            serde_json::json!({"bus":"SCSI","slot":"9","device":"ScsiCdrom"}),
            serde_json::json!({"bus":"NuBus","slot":"9","device":"ScsiCdrom"}),
            serde_json::json!({"bus":"Serial","slot":"A","device":"ScsiCdrom"}),
            serde_json::json!({"bus":"SCSI","slot":"3","device":"ScsiHardDisk","media":"/nonexistent/a.img"}),
            serde_json::json!({"bus":"SCSI","slot":"4","device":"ScsiHardDisk"}),
            serde_json::json!({"bus":"SCSI","slot":"5","device":"ScsiHardDisk","media":good.settings["media"],"read_only":true}),
            serde_json::json!({"bus":"SWIM","slot":"1","media":"/nonexistent/a.img"}),
            serde_json::json!({"bus":"SWIM","slot":"1","device":"ScsiCdrom"}),
            serde_json::json!({"bus":"SWIM","slot":"1","device":"MacSuperDrive"}),
        ];
        let mut devices = vec![good.clone()];
        devices.extend(
            cases
                .iter()
                .map(|c| serde_json::from_value(c.clone()).unwrap()),
        );
        let (mut emu, refused) = build(MacModel::Plus, devices);
        assert_eq!(refused.len(), cases.len(), "{:#?}", refused);
        assert_eq!(emu.devices(), [good]);
        // The drive is still there
        let buses = emu.buses();
        assert!(buses[0].slots[0].device.is_some());
    }

    #[test]
    fn a_serial_link_round_trips() {
        let bridge = DeviceConfig::of_model(
            bus_id::SERIAL,
            "A",
            DeviceModel::SerialBridge,
            &SerialLinkSettings { tcp_port: Some(0) },
        )
        .unwrap();
        assert_eq!(
            assert_round_trip(MacModel::Plus, vec![bridge.clone()]),
            vec![bridge]
        );
    }

    #[test]
    fn devices_come_and_go_while_the_machine_runs() {
        let files = TestFiles::new("hotplug");
        let (mut emu, _) = build(MacModel::Plus, vec![]);
        let disk = scsi_disk("3", files.file("a.img", 1024 * 1024));
        emu.add_device(&disk).unwrap();
        assert_eq!(emu.devices(), std::slice::from_ref(&disk));

        // An occupied slot only takes settings for what's in it
        assert!(emu.add_device(&scsi_cdrom("3", None)).is_err());

        let addr = emu.find_slot(bus_id::SCSI, "3").unwrap();
        emu.remove_device(addr).unwrap();
        assert_eq!(emu.devices(), []);
        assert!(emu.remove_device(addr).is_err());

        // A fixed device stays
        let drive = emu.find_slot(bus_id::SWIM, "1").unwrap();
        assert!(emu.remove_device(drive).is_err());
    }
}
