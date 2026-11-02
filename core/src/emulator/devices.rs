//! The machine as a frontend and a workspace see it
//!
//! A machine is a flat list of buses. A bus is a collection of slots, a slot may hold a device,
//! and a device is a specific model that provides one function. A device's settings belong to its
//! model, and travel as a dictionary so that a workspace can pass through code that doesn't know
//! them. Code that knows a model uses its typed settings struct.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use snow_floppy::Floppy;

use super::Emulator;
use crate::mac::MacModel;
use crate::mac::scc::SccCh;
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::serial_bridge::{SerialBridgeConfig, SerialBridgeStatus};
use crate::mac::swim::drive::DriveType;

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
    FileSharing,
    Video,
    AppleIIe,
    SerialLink,
}

/// Everything Snow can put in a slot. A new model of a known function only touches the core.
/// A model is what the guest sees: how the host backs it is a setting.
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

impl DeviceModel {
    /// In the workspace files: must not change
    pub fn key(self) -> &'static str {
        self.into()
    }

    pub fn from_key(key: &str) -> Option<Self> {
        key.parse().ok()
    }

    /// In the menus: free to change
    pub const fn name(self) -> &'static str {
        match self {
            Self::ScsiHardDisk => "Hard disk",
            Self::ScsiCdrom => "CD-ROM drive",
            #[cfg(feature = "ethernet")]
            Self::DaynaPortScsiLink => "DaynaPORT SCSI/Link",
            #[cfg(feature = "printer")]
            Self::LaserWriterIISC => "LaserWriter IISC",
            Self::BlueScsi => "BlueSCSI file sharing",
            Self::Mac400KDrive => "400K drive",
            Self::Mac800KDrive => "800K drive",
            Self::Mac800KPwmDrive => "800K drive (PWM)",
            Self::MacSuperDrive => "SuperDrive",
            Self::UniDisk35 => "UniDisk 3.5",
            Self::Apple525Drive => "Apple 5.25 Drive",
            Self::DisplayCard824 => "Display Card 8-24",
            Self::MacIIVideoCard => "Macintosh II Video Card",
            Self::SE30Video => "SE/30 video",
            Self::AppleIIePdsCard => "Apple IIe Card",
            Self::LocalTalkAdapter => "LocalTalk adapter",
            Self::SerialBridge => "Serial bridge",
        }
    }

    pub const fn function(self) -> FunctionKind {
        match self {
            Self::ScsiHardDisk => FunctionKind::HardDisk,
            Self::ScsiCdrom => FunctionKind::Cdrom,
            #[cfg(feature = "ethernet")]
            Self::DaynaPortScsiLink => FunctionKind::Ethernet,
            #[cfg(feature = "printer")]
            Self::LaserWriterIISC => FunctionKind::Printer,
            Self::BlueScsi => FunctionKind::FileSharing,
            Self::Mac400KDrive
            | Self::Mac800KDrive
            | Self::Mac800KPwmDrive
            | Self::MacSuperDrive
            | Self::UniDisk35
            | Self::Apple525Drive => FunctionKind::Floppy,
            Self::DisplayCard824 | Self::MacIIVideoCard | Self::SE30Video => FunctionKind::Video,
            Self::AppleIIePdsCard => FunctionKind::AppleIIe,
            Self::LocalTalkAdapter | Self::SerialBridge => FunctionKind::SerialLink,
        }
    }

    /// Created with the machine, never added or removed while it runs
    pub const fn fixed(self) -> bool {
        matches!(
            self,
            Self::Mac400KDrive
                | Self::Mac800KDrive
                | Self::Mac800KPwmDrive
                | Self::MacSuperDrive
                | Self::UniDisk35
                | Self::Apple525Drive
                | Self::DisplayCard824
                | Self::MacIIVideoCard
                | Self::SE30Video
                | Self::AppleIIePdsCard
        )
    }

    /// The settings holding paths, which a workspace keeps relative to its file
    pub const fn path_keys(self) -> &'static [&'static str] {
        match self {
            Self::ScsiHardDisk
            | Self::ScsiCdrom
            | Self::Mac400KDrive
            | Self::Mac800KDrive
            | Self::Mac800KPwmDrive
            | Self::MacSuperDrive
            | Self::UniDisk35
            | Self::Apple525Drive => &["media"],
            Self::BlueScsi => &["shared_dir"],
            Self::DisplayCard824 | Self::MacIIVideoCard | Self::SE30Video => &["rom"],
            _ => &[],
        }
    }

    /// The buses a device of this model adds
    pub const fn buses(self) -> &'static [BusKind] {
        match self {
            Self::AppleIIePdsCard => &[BusKind::IieDisk],
            _ => &[],
        }
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

/// Something that can go in a slot
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub model: DeviceModel,
    /// Unavailable right now, so shown greyed
    pub blocked: bool,
}

impl Offer {
    const fn free(model: DeviceModel) -> Self {
        Self {
            model,
            blocked: false,
        }
    }
}

/// A place on a bus
#[derive(Debug, Clone)]
pub struct Slot {
    /// In the workspace files: must not change
    pub id: String,
    /// In the menus: "internal", "Modem"
    pub name: Option<String>,
    pub blocked: bool,
    pub hidden_by_default: bool,
    pub hint: Option<String>,
    /// What this machine has here unless told otherwise
    pub default_model: Option<DeviceModel>,
    pub device: Option<Device>,
}

impl Slot {
    fn empty(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            blocked: false,
            hidden_by_default: false,
            hint: None,
            default_model: None,
            device: None,
        }
    }

    fn named(id: impl Into<String>, name: &str) -> Self {
        Self {
            name: Some(name.to_owned()),
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
    pub offers: Vec<Offer>,
    pub slots: Vec<Slot>,
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
    /// What gets saved, and what can create this device again
    pub settings: Map<String, Value>,
    pub state: FunctionState,
}

/// What the menus show of a device, with what changes as it runs. Never saved.
#[derive(Debug, Clone)]
pub enum FunctionState {
    HardDisk(HardDiskState),
    Cdrom(CdromState),
    Floppy(FloppyState),
    IieFloppy(IieFloppyState),
    #[cfg(feature = "ethernet")]
    Ethernet(EthernetState),
    Printer,
    FileSharing(FileSharingState),
    Video,
    AppleIIe,
    SerialLink(Option<SerialBridgeStatus>),
}

#[derive(Debug, Clone)]
pub struct HardDiskState {
    pub media: Option<PathBuf>,
    pub capacity: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct CdromState {
    pub media: Option<PathBuf>,
    /// The folder of images of a Toolbox managed drive
    pub folder: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct FloppyState {
    pub media: Option<PathBuf>,
    pub ejected: bool,
    pub motor: bool,
    pub writing: bool,
    pub track: usize,
    pub image_title: String,
    pub dirty: bool,
    pub drive_type: DriveType,
    /// The image can be written back to the file it came from
    pub writeback_supported: bool,
    pub writeback_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct IieFloppyState {
    pub media: Option<PathBuf>,
}

#[cfg(feature = "ethernet")]
#[derive(Debug, Clone)]
pub struct EthernetState {
    pub link: crate::mac::scsi::ethernet::EthernetLinkType,
    pub capture: Option<super::comm::EthernetCaptureStatus>,
}

#[derive(Debug, Clone)]
pub struct FileSharingState {
    pub shared_dir: Option<PathBuf>,
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

    /// The model, if this build knows it
    pub fn known_model(&self) -> Option<DeviceModel> {
        DeviceModel::from_key(self.model.as_deref()?)
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

/// What the hard disk function needs, whichever the model
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

/// What the CD-ROM function needs, whichever the model
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlueScsiSettings {
    pub shared_dir: PathBuf,
}
impl DeviceSettings for BlueScsiSettings {
    const MODEL: DeviceModel = DeviceModel::BlueScsi;
}

/// What the floppy function needs, whichever the model
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialBridgeSettings {
    /// None for a PTY
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_port: Option<u16>,
}
impl DeviceSettings for SerialBridgeSettings {
    const MODEL: DeviceModel = DeviceModel::SerialBridge;
}

impl MacModel {
    /// The buses every machine of this model has, with nothing plugged in. Cards add theirs.
    pub fn buses(self) -> Vec<Bus> {
        let mut buses = vec![];

        let drives: Vec<_> = self
            .fdd_drives()
            .iter()
            .enumerate()
            .filter_map(|(i, d)| Some((i, DeviceModel::from_drive_type(*d)?)))
            .collect();
        if !drives.is_empty() {
            buses.push(Bus {
                kind: BusKind::Swim,
                id: bus_id::SWIM.to_owned(),
                title: "Floppy".to_owned(),
                offers: self
                    .drive_options()
                    .iter()
                    .copied()
                    .map(Offer::free)
                    .collect(),
                slots: drives
                    .iter()
                    .map(|&(i, model)| Slot {
                        default_model: Some(model),
                        ..Slot::empty((i + 1).to_string())
                    })
                    .collect(),
            });
        }

        if self.has_scsi() {
            let mut offers = vec![
                Offer::free(DeviceModel::ScsiHardDisk),
                Offer::free(DeviceModel::ScsiCdrom),
            ];
            #[cfg(feature = "ethernet")]
            offers.push(Offer::free(DeviceModel::DaynaPortScsiLink));
            #[cfg(feature = "printer")]
            offers.push(Offer::free(DeviceModel::LaserWriterIISC));
            offers.push(Offer::free(DeviceModel::BlueScsi));
            buses.push(Bus {
                kind: BusKind::Scsi,
                id: bus_id::SCSI.to_owned(),
                title: "SCSI".to_owned(),
                offers,
                slots: (0..7).map(|i| Slot::empty(i.to_string())).collect(),
            });
        }

        if self == Self::SE30 {
            buses.push(Bus {
                kind: BusKind::BuiltIn,
                id: bus_id::BUILT_IN.to_owned(),
                title: "Built-in".to_owned(),
                offers: vec![],
                slots: vec![Slot {
                    default_model: Some(DeviceModel::SE30Video),
                    ..Slot::empty("0")
                }],
            });
            buses.push(Bus {
                kind: BusKind::Se30Pds,
                id: bus_id::PDS_SE30.to_owned(),
                title: "SE/30 PDS".to_owned(),
                offers: vec![],
                slots: vec![Slot::empty("0")],
            });
        } else if !self.nubus_slots().is_empty() {
            buses.push(Bus {
                kind: BusKind::Nubus,
                id: bus_id::NUBUS.to_owned(),
                title: "NuBus".to_owned(),
                offers: self
                    .nubus_cards()
                    .iter()
                    .copied()
                    .map(Offer::free)
                    .collect(),
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
                offers: vec![Offer::free(DeviceModel::AppleIIePdsCard)],
                slots: vec![Slot::empty("0")],
            });
        }

        buses.push(Bus {
            kind: BusKind::Serial,
            id: bus_id::SERIAL.to_owned(),
            title: "Serial".to_owned(),
            offers: vec![
                Offer::free(DeviceModel::SerialBridge),
                Offer::free(DeviceModel::LocalTalkAdapter),
            ],
            slots: vec![Slot::named("A", "Modem"), Slot::named("B", "Printer")],
        });

        buses
    }

    /// The bus the Apple IIe Card adds, with the drives on its chain
    pub fn iie_disk_bus(chain: usize) -> Bus {
        Bus {
            kind: BusKind::IieDisk,
            id: bus_id::IIE_DISK.to_owned(),
            title: "Floppy (Apple IIe Card)".to_owned(),
            offers: vec![
                Offer::free(DeviceModel::UniDisk35),
                Offer::free(DeviceModel::Apple525Drive),
            ],
            slots: (1..=chain).map(|i| Slot::empty(i.to_string())).collect(),
        }
    }

    /// The drives this model's machines can be built with, in place of its own
    pub const fn drive_options(self) -> &'static [DeviceModel] {
        match self {
            Self::Early128K | Self::Early512K => {
                &[DeviceModel::Mac400KDrive, DeviceModel::Mac800KPwmDrive]
            }
            _ => &[],
        }
    }

    /// The cards this model's NuBus slots can take
    pub const fn nubus_cards(self) -> &'static [DeviceModel] {
        match self {
            Self::MacII | Self::MacIIFDHD | Self::MacIIx | Self::MacIIcx => {
                &[DeviceModel::DisplayCard824, DeviceModel::MacIIVideoCard]
            }
            _ => &[],
        }
    }

    pub const fn has_lc_pds(self) -> bool {
        matches!(self, Self::LC | Self::LCII | Self::LCIII | Self::LC475)
    }
}

/// What a machine remembers of its devices, where the devices don't
#[derive(Default)]
pub(super) struct DeviceBook {
    /// The cards the machine was built with. They can't change, so they report what they got.
    pub(super) cards: Vec<DeviceConfig>,
    /// The Apple IIe Card's drives, in chain order
    pub(super) iie_chain: Vec<DeviceModel>,
    /// The image in each of those drives
    pub(super) iie_media: Vec<Option<PathBuf>>,
    pub(super) serial: [Option<SerialBridgeConfig>; 2],
    /// The SCSI ID the shared folder sits at, and the folder
    pub(super) blue_scsi: Option<(usize, PathBuf)>,
}

/// What is behind a slot. Only this file knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotTarget {
    Drive(usize),
    Scsi(usize),
    Card,
    IieDrive(usize),
    Serial(SccCh),
}

fn slot_target(bus: &Bus, slot: &Slot) -> SlotTarget {
    let number = |base: usize| slot.id.parse::<usize>().unwrap_or(base) - base;
    match bus.kind {
        BusKind::Swim => SlotTarget::Drive(number(1)),
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

/// The list a machine with these buses can be created from
pub fn device_configs(buses: &[Bus]) -> Vec<DeviceConfig> {
    let mut configs = vec![];
    for bus in buses {
        for slot in &bus.slots {
            let Some(device) = &slot.device else {
                continue;
            };
            let is_default = slot.default_model == Some(device.model);
            if is_default && device.settings.is_empty() {
                continue;
            }
            configs.push(DeviceConfig {
                bus: bus.id.clone(),
                slot: slot.id.clone(),
                model: (!is_default).then(|| device.model.key().to_owned()),
                settings: device.settings.clone(),
            });
        }
    }
    configs
}

impl Emulator {
    /// The machine as it is now
    pub fn buses(&self) -> Vec<Bus> {
        let mut buses = self.model.buses();
        if !self.device_book.iie_chain.is_empty()
            && let Some(pds) = buses.iter().position(|b| b.kind == BusKind::LcPds)
        {
            buses.insert(
                pds + 1,
                MacModel::iie_disk_bus(self.device_book.iie_chain.len()),
            );
        }
        for bus in &mut buses {
            let mut slots = std::mem::take(&mut bus.slots);
            for slot in &mut slots {
                slot.device = self.device_in(bus, slot);
            }
            bus.slots = slots;
            self.block_offers(bus);
        }
        buses
    }

    /// The list a machine like this one can be created from
    pub fn devices(&self) -> Vec<DeviceConfig> {
        device_configs(&self.buses())
    }

    /// Finds a slot from what a workspace knows of it
    pub fn find_slot(&self, bus: &str, slot: &str) -> Option<SlotAddr> {
        let buses = self.buses();
        let bus_idx = buses.iter().position(|b| b.id == bus)?;
        let slot = buses[bus_idx].slots.iter().position(|s| s.id == slot)?;
        Some(SlotAddr { bus: bus_idx, slot })
    }

    /// One of each on a bus: the frontend has offered no more than that so far
    fn block_offers(&self, bus: &mut Bus) {
        if bus.kind != BusKind::Scsi {
            return;
        }
        let has = |model| {
            bus.slots
                .iter()
                .any(|s| s.device.as_ref().is_some_and(|d| d.model == model))
        };
        let mut offers = std::mem::take(&mut bus.offers);
        for offer in &mut offers {
            offer.blocked = match offer.model {
                #[cfg(feature = "ethernet")]
                DeviceModel::DaynaPortScsiLink => has(offer.model),
                #[cfg(feature = "printer")]
                DeviceModel::LaserWriterIISC => has(offer.model),
                DeviceModel::BlueScsi => has(offer.model),
                _ => false,
            };
        }
        bus.offers = offers;
    }

    fn device_in(&self, bus: &Bus, slot: &Slot) -> Option<Device> {
        match slot_target(bus, slot) {
            SlotTarget::Drive(i) => self.drive_device(i),
            SlotTarget::Scsi(i) => self.scsi_device(i),
            SlotTarget::Card => {
                let config = self
                    .device_book
                    .cards
                    .iter()
                    .find(|c| c.bus == bus.id && c.slot == slot.id)?;
                let model = config.known_model()?;
                Some(Device {
                    model,
                    settings: config.settings.clone(),
                    state: match model.function() {
                        FunctionKind::AppleIIe => FunctionState::AppleIIe,
                        _ => FunctionState::Video,
                    },
                })
            }
            SlotTarget::IieDrive(i) => {
                let media = self.device_book.iie_media.get(i).cloned().flatten();
                Some(Device {
                    model: *self.device_book.iie_chain.get(i)?,
                    settings: settings_map(&FloppySettings {
                        media: media.clone(),
                        read_only: false,
                    }),
                    state: FunctionState::IieFloppy(IieFloppyState { media }),
                })
            }
            SlotTarget::Serial(ch) => {
                let (model, settings) = match self.device_book.serial[ch as usize].as_ref()? {
                    SerialBridgeConfig::Pty => (
                        DeviceModel::SerialBridge,
                        settings_map(&SerialBridgeSettings::default()),
                    ),
                    SerialBridgeConfig::Tcp(port) => (
                        DeviceModel::SerialBridge,
                        settings_map(&SerialBridgeSettings {
                            tcp_port: Some(*port),
                        }),
                    ),
                    SerialBridgeConfig::LocalTalk => (DeviceModel::LocalTalkAdapter, Map::new()),
                };
                Some(Device {
                    model,
                    settings,
                    state: FunctionState::SerialLink(
                        self.serial_bridges[ch as usize]
                            .as_ref()
                            .map(|b| b.status()),
                    ),
                })
            }
        }
    }

    fn drive_device(&self, i: usize) -> Option<Device> {
        let swim = self.config.swim();
        let drive = swim.drives.get(i)?;
        let model = DeviceModel::from_drive_type(drive.drive_type)?;
        let media = drive
            .floppy_inserted
            .then(|| drive.media_path.clone())
            .flatten();
        Some(Device {
            model,
            settings: settings_map(&FloppySettings {
                media: media.clone(),
                read_only: media.is_some() && drive.media_read_only,
            }),
            state: FunctionState::Floppy(FloppyState {
                media,
                ejected: !drive.floppy_inserted,
                motor: drive.motor,
                writing: drive.motor && swim.is_writing(),
                track: drive.track,
                image_title: drive.floppy.get_title().to_owned(),
                dirty: drive.floppy.is_dirty(),
                drive_type: drive.drive_type,
                writeback_supported: drive.image_path.is_some()
                    && drive.floppy.supports_writeback(),
                writeback_enabled: drive.writeback_enabled,
            }),
        })
    }

    fn scsi_device(&self, id: usize) -> Option<Device> {
        if let Some((blue_id, dir)) = &self.device_book.blue_scsi
            && *blue_id == id
        {
            return Some(Device {
                model: DeviceModel::BlueScsi,
                settings: settings_map(&BlueScsiSettings {
                    shared_dir: dir.clone(),
                }),
                state: FunctionState::FileSharing(FileSharingState {
                    shared_dir: Some(dir.clone()),
                }),
            });
        }
        let scsi = self.config.scsi();
        let image = scsi.get_disk_imagefn(id).map(|p| p.to_path_buf());
        match scsi.get_target_type(id)? {
            ScsiTargetType::Disk => Some(Device {
                model: DeviceModel::ScsiHardDisk,
                settings: settings_map(&HardDiskSettings {
                    media: image.clone()?,
                    read_only: false,
                }),
                state: FunctionState::HardDisk(HardDiskState {
                    media: image,
                    capacity: scsi.get_disk_capacity(id),
                }),
            }),
            ScsiTargetType::Cdrom => {
                let folder = scsi.get_cdrom_folder(id).map(|p| p.to_path_buf());
                Some(Device {
                    model: DeviceModel::ScsiCdrom,
                    settings: settings_map(&CdromSettings {
                        media: folder.clone().or_else(|| image.clone()),
                    }),
                    state: FunctionState::Cdrom(CdromState {
                        media: image,
                        folder,
                    }),
                })
            }
            #[cfg(feature = "ethernet")]
            ScsiTargetType::Ethernet => {
                let target = scsi.targets[id].as_ref()?;
                let link = target.eth_link()?;
                Some(Device {
                    model: DeviceModel::DaynaPortScsiLink,
                    settings: settings_map(&serde_json::json!({ "link": link })),
                    state: FunctionState::Ethernet(EthernetState {
                        link,
                        capture: target.eth_capture_status(),
                    }),
                })
            }
            #[cfg(feature = "printer")]
            ScsiTargetType::Printer => Some(Device {
                model: DeviceModel::LaserWriterIISC,
                settings: Map::new(),
                state: FunctionState::Printer,
            }),
        }
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
        assert_eq!(slots(MacModel::Plus, bus_id::SWIM), ["1", "2"]);
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
        assert_eq!(swim.slots.len(), 2);
        for slot in &swim.slots {
            let device = slot.device.as_ref().unwrap();
            assert_eq!(device.model, DeviceModel::Mac800KDrive);
            assert!(matches!(&device.state, FunctionState::Floppy(f) if f.ejected));
        }
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
}
