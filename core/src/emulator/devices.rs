//! The machine as a frontend and a workspace see it
//!
//! A machine is a flat list of buses. A bus is a collection of slots, a slot may hold a device,
//! and a device is a specific model that provides one function. A device's settings belong to its
//! model, and travel as a dictionary so that a workspace can pass through code that doesn't know
//! them. Code that knows a model uses its typed settings struct.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use snow_floppy::Floppy;
use snow_floppy::loaders::{Autodetect, FloppyImageLoader, TwoImg};

use super::comm::EmulatorEvent;
use super::{Emulator, EmulatorConfig, MouseMode};
use crate::mac::maclc::pds_iie::disk::DskOrder;
use crate::mac::maclc::pds_iie::{IieCardSettings, IieDiskDevice, PdsIIeCard};
use crate::mac::scc::SccCh;
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::serial_bridge::SccBridge;
use crate::mac::serial_bridge::{SerialBridgeConfig, SerialBridgeStatus};
use crate::mac::swim::drive::DriveType;
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
        bus.slots.iter().find(|s| s.id == self.slot)?.default_model
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
        self.iie_media = vec![None; self.iie_chain.len()];
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
                slot.default_model.filter(|_| config.model.is_none())
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
        match device {
            DeviceModel::DisplayCard824 | DeviceModel::MacIIVideoCard => {
                if config.bus != bus_id::NUBUS || !model.nubus_cards().contains(&device) {
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
                if model != MacModel::SE30 || config.bus != bus_id::BUILT_IN {
                    bail!("This machine can't take a {}", device.name());
                }
                let rom: SE30VideoSettings = config.parse()?;
                self.take_video(0xE, device, &rom.rom, config)?;
                Ok(false)
            }
            DeviceModel::AppleIIePdsCard => {
                if config.bus != bus_id::PDS_LC || !model.has_lc_pds() {
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
                    if !model.drive_options().contains(&device) {
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
            None,
            plan.iie_card,
            video_slot,
        )?;
        emu.device_book.cards = plan.cards;

        let mut refused = plan.refused;
        for config in plan.rest {
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
            None => self.create_device(bus, slot, config),
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
            SlotTarget::Scsi(id) if device.model == DeviceModel::BlueScsi => {
                debug_assert_eq!(self.device_book.blue_scsi.as_ref().map(|b| b.0), Some(id));
                self.device_book.blue_scsi = None;
                self.config.scsi_mut().set_shared_dir(None);
            }
            SlotTarget::Scsi(id) => self.config.scsi_mut().detach_target(id),
            SlotTarget::Serial(ch) => self.serial_bridge_disable(ch)?,
            t => bail!("{:?} doesn't take removable devices", t),
        }
        info!("{} #{}: device removed", bus.id, slot.id);
        Ok(())
    }

    fn create_device(&mut self, bus: &Bus, slot: &Slot, config: &DeviceConfig) -> Result<()> {
        let key = config
            .model
            .as_deref()
            .context("This slot is empty, and the entry doesn't say what to put in it")?;
        let model =
            DeviceModel::from_key(key).with_context(|| format!("Unknown device {}", key))?;
        if model.fixed() {
            bail!("A {} can only be chosen with the machine", model.name());
        }
        let offer = bus
            .offers
            .iter()
            .find(|o| o.model == model)
            .with_context(|| format!("{} doesn't take a {}", bus.id, model.name()))?;
        if offer.blocked || slot.blocked {
            bail!("A {} can't go here right now", model.name());
        }
        match (slot_target(bus, slot), model) {
            (SlotTarget::Scsi(id), DeviceModel::ScsiHardDisk) => {
                let settings: ScsiHardDiskSettings = config.parse()?;
                if settings.disk.read_only {
                    bail!("SCSI disks can't be read-only yet");
                }
                self.load_hdd_image(&settings.disk.media, id)
            }
            (SlotTarget::Scsi(id), DeviceModel::ScsiCdrom) => {
                let settings: ScsiCdromSettings = config.parse()?;
                match settings.cdrom.media {
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
                let settings: BlueScsiSettings = config.parse()?;
                if !settings.shared_dir.is_dir() {
                    bail!("{} is not a folder", settings.shared_dir.display());
                }
                self.config
                    .scsi_mut()
                    .set_shared_dir(Some(settings.shared_dir.clone()));
                self.device_book.blue_scsi = Some((id, settings.shared_dir));
                Ok(())
            }
            (SlotTarget::Serial(ch), DeviceModel::SerialBridge) => {
                let settings: SerialBridgeSettings = config.parse()?;
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
            SlotTarget::Drive(i) => {
                let settings: FloppySettings = config.parse()?;
                match settings.media {
                    Some(media) => self.insert_floppy_file(i, &media, settings.read_only),
                    None => Ok(()),
                }
            }
            SlotTarget::IieDrive(i) => {
                let settings: FloppySettings = config.parse()?;
                match settings.media {
                    Some(media) => self.insert_iie_media(i, &media),
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
                        .context("No target attached")?
                        .eth_set_link(link)?;
                }
                Ok(())
            }
            SlotTarget::Scsi(id) if model == DeviceModel::ScsiCdrom => {
                let settings: ScsiCdromSettings = config.parse()?;
                match settings.cdrom.media {
                    Some(media) => self.scsi_load_media(id, &media),
                    None => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    fn target_at(&self, addr: SlotAddr) -> Result<SlotTarget> {
        let buses = self.buses();
        let bus = buses.get(addr.bus).context("No such bus")?;
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
            .branch_media(path)
    }

    /// Puts an image in the drive in a slot
    pub(super) fn load_media(&mut self, addr: SlotAddr, path: &Path) -> Result<()> {
        match self.target_at(addr)? {
            SlotTarget::Scsi(id) => self.scsi_load_media(id, path),
            SlotTarget::IieDrive(i) => self.insert_iie_media(i, path),
            t => bail!("{:?} doesn't take media", t),
        }
    }

    /// Takes the image out of the drive in a slot
    pub(super) fn eject_media(&mut self, addr: SlotAddr) -> Result<()> {
        let SlotTarget::IieDrive(i) = self.target_at(addr)? else {
            bail!("This drive can't eject from here");
        };
        let model = *self.device_book.iie_chain.get(i).context("No such drive")?;
        let drive = self.device_book.iie_chain[..i]
            .iter()
            .filter(|m| **m == DeviceModel::Apple525Drive)
            .count();
        let card = self.iie_card_mut().context("No Apple IIe Card installed")?;
        if model == DeviceModel::UniDisk35 {
            card.eject_unidisk();
        } else {
            card.eject_disk(drive);
        }
        self.device_book.iie_media[i] = None;
        Ok(())
    }

    fn scsi_load_media(&mut self, id: usize, path: &Path) -> Result<()> {
        self.config.scsi_mut().targets[id]
            .as_mut()
            .context("No target attached")?
            .load_media(path)
    }

    fn insert_floppy_file(&mut self, drive: usize, path: &Path, read_only: bool) -> Result<()> {
        let filename = path.to_string_lossy();
        let mut image = Autodetect::load_file(&filename)
            .with_context(|| format!("Cannot load {}", path.display()))?;
        if read_only {
            image.set_force_wp();
        }
        let writeback_path = image.supports_writeback().then(|| path.to_path_buf());
        self.config.swim_mut().disk_insert(drive, image)?;
        let drv = &mut self.config.swim_mut().drives[drive];
        drv.media_path = Some(path.to_path_buf());
        drv.media_read_only = read_only;
        drv.image_path = writeback_path;
        drv.writeback_enabled = false;
        drv.pending_writeback = false;
        Ok(())
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

    /// Puts an image in the drive at this position of the card's chain
    fn insert_iie_media(&mut self, position: usize, path: &Path) -> Result<()> {
        let chain = &self.device_book.iie_chain;
        match chain.get(position) {
            Some(DeviceModel::UniDisk35) => self.insert_iie_unidisk(path),
            Some(_) => {
                // The IIe numbers the 5.25" drives among themselves
                let drive = chain[..position]
                    .iter()
                    .filter(|m| **m == DeviceModel::Apple525Drive)
                    .count();
                self.insert_iie_disk(drive, path)
            }
            None => bail!("No drive at this position of the chain"),
        }
    }

    /// Inserts an image in one of the card's 5.25" drives
    fn insert_iie_disk(&mut self, drive: usize, path: &Path) -> Result<()> {
        let bytes = std::fs::read(path)?;
        // A 2IMG header names the sector order, else the extension does
        let (bytes, order) = if TwoImg::detect(&bytes) {
            let img = TwoImg::parse(&bytes)?;
            let order = if img.format == TwoImg::FORMAT_PRODOS {
                DskOrder::Prodos
            } else {
                DskOrder::Dos
            };
            (img.data, order)
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("po"))
        {
            (&bytes[..], DskOrder::Prodos)
        } else {
            (&bytes[..], DskOrder::Dos)
        };
        self.iie_card_mut()
            .context("No Apple IIe Card installed")?
            .insert_disk(drive, bytes, order)?;
        // Back from the card's numbering to the position in the chain
        let position = self
            .device_book
            .iie_chain
            .iter()
            .enumerate()
            .filter(|(_, m)| **m == DeviceModel::Apple525Drive)
            .nth(drive)
            .map(|(i, _)| i);
        if let Some(position) = position {
            self.device_book.iie_media[position] = Some(path.to_path_buf());
        }
        Ok(())
    }

    /// Inserts an image in the card's UniDisk 3.5
    fn insert_iie_unidisk(&mut self, path: &Path) -> Result<()> {
        let bytes = std::fs::read(path)?;
        // A 2IMG header carries the lock flag; a raw image is the blocks alone
        let (data, locked) = if TwoImg::detect(&bytes) {
            let img = TwoImg::parse(&bytes)?;
            (img.data.to_vec(), img.locked)
        } else {
            (bytes, false)
        };
        self.iie_card_mut()
            .context("No Apple IIe Card installed")?
            .insert_unidisk(data, locked)?;
        let position = self
            .device_book
            .iie_chain
            .iter()
            .position(|m| *m == DeviceModel::UniDisk35);
        if let Some(position) = position {
            self.device_book.iie_media[position] = Some(path.to_path_buf());
        }
        Ok(())
    }

    /// Plugs a host link in a serial port
    fn serial_bridge_enable(&mut self, ch: SccCh, config: SerialBridgeConfig) -> Result<()> {
        let bridge = SccBridge::new(&config)?;
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
        self.event_sender
            .send(EmulatorEvent::SerialBridgeStatus(ch, Some(status)))?;
        Ok(())
    }

    /// Unplugs it
    fn serial_bridge_disable(&mut self, ch: SccCh) -> Result<()> {
        self.device_book.serial[ch as usize] = None;
        if self.serial_bridges[ch as usize].take().is_some() {
            info!("Serial bridge disabled on channel {:?}", ch);
            self.event_sender
                .send(EmulatorEvent::SerialBridgeStatus(ch, None))?;
        }
        Ok(())
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
        let (emu, refused) = build(model, devices);
        assert!(refused.is_empty(), "{:?}", refused);
        let reported = emu.devices();
        drop(emu);
        let (again, refused) = build(model, reported.clone());
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
    fn blue_scsi_holds_its_slot() {
        let files = TestFiles::new("blue");
        let blue = DeviceConfig::new(
            bus_id::SCSI,
            "1",
            &BlueScsiSettings {
                shared_dir: files.dir("shared"),
            },
        )
        .unwrap();
        assert_eq!(
            assert_round_trip(MacModel::Plus, vec![blue.clone()]),
            vec![blue.clone()]
        );

        // Its slot is taken, and there's one to a bus
        let (emu, refused) = build(
            MacModel::Plus,
            vec![
                blue.clone(),
                scsi_cdrom("1", None),
                DeviceConfig {
                    slot: "2".into(),
                    ..blue
                },
            ],
        );
        assert_eq!(refused.len(), 2);
        assert_eq!(emu.devices().len(), 1);
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
            let (emu, refused) = if model == MacModel::MacIIx {
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
        let (emu, refused) = build(
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
                &FloppySettings {
                    media: Some(files.file("b.dsk", 143_360)),
                    read_only: false,
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
        let (emu, refused) = build(MacModel::Plus, devices);
        assert_eq!(refused.len(), cases.len(), "{:#?}", refused);
        assert_eq!(emu.devices(), [good]);
        // The drive is still there
        let buses = emu.buses();
        assert!(buses[0].slots[0].device.is_some());
    }

    #[test]
    fn a_serial_link_round_trips() {
        let bridge = DeviceConfig::new(
            bus_id::SERIAL,
            "A",
            &SerialBridgeSettings { tcp_port: Some(0) },
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
        assert_eq!(emu.devices(), [disk.clone()]);

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
