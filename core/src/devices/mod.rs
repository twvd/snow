//! The device model: the I/O devices of a machine that the frontend shows, configures or acts on
//!
//! See docs/misc/device-model.md.

mod config;
mod function;

pub use config::*;
pub use function::*;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;

use crate::debuggable::DebuggableProperties;
use crate::mac::MacModel;
use crate::mac::compact;
use crate::mac::maclc;
use crate::mac::maclc::pds_iie;
use crate::mac::nubus;
use crate::mac::nubus::NubusSlot;
use crate::mac::pds::PdsSlot;
use crate::mac::portable;
use crate::mac::quadra;
use crate::mac::scsi;
use crate::mac::scsi::target::ScsiTarget;
use crate::mac::swim;
use crate::mac::swim::device::SwimBusDevice;

/// A type of bus. A device model fits one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusKind {
    Scsi,
    Swim,
    Nubus,
    /// The processor-direct slot of the LC family
    LcPds,
    /// The processor-direct slot of the SE/30
    Se30Pds,
    /// The disk port of an Apple IIe Card
    IieDisk,
    /// The devices on the logic board
    BuiltIn,
}

/// A bus of a machine: a list of slots
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bus {
    pub kind: BusKind,
    /// Names this bus: "SCSI". In the workspace files: must not change.
    pub id: String,
    /// In the menus: free to change
    pub title: String,
    pub slots: Vec<Slot>,
}

/// A place on a bus, which may hold a device
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// The slot's id: unique on its bus, and the only way to identify the slot outside its
    /// bus. It may read like a number ("3"), but nothing outside the bus treats it as one. In
    /// the workspace files: must not change.
    pub id: String,
    pub device: Option<Device>,
}

/// The device in a slot, as it is now
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub model: DeviceModel,
    /// Its ROM file, if it has one
    pub rom: Option<PathBuf>,
    /// The Config of its function, as it is now
    pub function: FunctionConfig,
    /// In the menus, if the device gives one
    pub name: Option<String>,
}

impl Bus {
    /// A bus with empty slots
    pub fn new(kind: BusKind, id: &str, title: &str, slots: &[&str]) -> Self {
        Self {
            kind,
            id: id.to_string(),
            title: title.to_string(),
            slots: slots
                .iter()
                .map(|id| Slot {
                    id: id.to_string(),
                    device: None,
                })
                .collect(),
        }
    }

    /// What a message to the user calls a slot of this bus: "SCSI ID #3" on a SCSI bus, as
    /// Snow always said, and the title of the bus and the id of the slot on the others
    pub fn slot_label(&self, slot: &str) -> String {
        if matches!(self.kind, BusKind::Scsi) {
            format!("SCSI ID #{}", slot)
        } else {
            format!("{} #{}", self.title, slot)
        }
    }
}

/// The machine that has these buses, as the list of device configs it can be made from again.
///
/// It leaves out a device that the Mac model has in its slot without being asked, and the
/// devices on a bus that a device adds: that device makes them.
pub fn device_configs(buses: &[Bus], mac: MacModel) -> Vec<DeviceConfig> {
    let defaults = mac.buses();
    let mut configs = vec![];
    for bus in buses {
        let Some(default_bus) = defaults.iter().find(|b| b.id == bus.id) else {
            continue;
        };
        for slot in &bus.slots {
            let Some(device) = &slot.device else {
                continue;
            };
            // A device only gives its name once it exists
            let unnamed = Device {
                name: None,
                ..device.clone()
            };
            let default_slot = default_bus.slots.iter().find(|s| s.id == slot.id);
            if default_slot.and_then(|s| s.device.as_ref()) == Some(&unnamed) {
                continue;
            }
            configs.push(DeviceConfig {
                bus: bus.id.clone(),
                slot: slot.id.clone(),
                model: Some(device.model),
                rom: device.rom.clone(),
                function: device.function.clone(),
            });
        }
    }
    configs
}

/// The device config of a built-in device in a list, by the id of its slot on the `Built-in`
/// bus. Called by the constructor of a machine, which makes its built-in devices.
pub(crate) fn built_in_config<'a>(
    devices: &'a [DeviceConfig],
    slot: &str,
) -> Option<&'a DeviceConfig> {
    devices
        .iter()
        .find(|config| config.bus == bus_id::BUILT_IN && config.slot == slot)
}

/// What a list of device configs has chosen for the built-in video: nothing, if the list
/// doesn't name it
pub(crate) fn built_in_display(devices: &[DeviceConfig]) -> DisplayConfig {
    match built_in_config(devices, built_in::VIDEO) {
        Some(DeviceConfig {
            function: FunctionConfig::Display(display),
            ..
        }) => *display,
        _ => DisplayConfig::default(),
    }
}

/// A device config that a machine couldn't use, or whose device was made without a part of
/// its Config
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedDevice {
    pub config: DeviceConfig,
    /// Why, for the user
    pub reason: String,
}

/// Why a device config can't be used on a Mac model, given the device configs kept before it.
/// A device config that names no device model gets the one the Mac model has in the slot.
fn check_device_config(
    mac: MacModel,
    buses: &[Bus],
    kept: &[DeviceConfig],
    config: &mut DeviceConfig,
) -> Result<()> {
    let bus = buses
        .iter()
        .find(|bus| bus.id == config.bus)
        .context("This Mac model has no such bus")?;
    let slot = bus
        .slots
        .iter()
        .find(|slot| slot.id == config.slot)
        .context("The bus has no such slot")?;
    if kept
        .iter()
        .any(|k| k.bus == config.bus && k.slot == config.slot)
    {
        bail!("Another device is in this slot already");
    }
    let model = match (config.model, &slot.device) {
        (Some(model), _) => model,
        (None, Some(device)) => device.model,
        (None, None) => bail!("No device model is given"),
    };
    let info = model.info();
    if !models_for(bus.kind, mac).any(|m| m.model == model) {
        bail!("{} doesn't fit this bus", info.name);
    }
    if !info.function.takes(&config.function) {
        bail!("The Config isn't that of the function of {}", info.name);
    }
    config.model = Some(model);
    Ok(())
}

/// Checks a list of device configs against what a Mac model has, and completes it: each slot
/// in which the Mac model has a device without being asked, and that the list doesn't name,
/// gets a device config for that device, and each Ethernet adapter with no address gets one
/// that no other adapter of the list has. Returns the list to make the machine from, and the
/// device configs that can't be used.
pub(crate) fn complete_device_configs(
    mac: MacModel,
    configs: Vec<DeviceConfig>,
) -> (Vec<DeviceConfig>, Vec<RefusedDevice>) {
    let buses = mac.buses();
    let mut kept = vec![];
    let mut refused = vec![];
    for mut config in configs {
        match check_device_config(mac, &buses, &kept, &mut config) {
            Ok(()) => kept.push(config),
            Err(e) => refused.push(RefusedDevice {
                config,
                reason: format!("{:#}", e),
            }),
        }
    }
    for bus in buses {
        for slot in bus.slots {
            let Some(device) = slot.device else {
                continue;
            };
            if kept.iter().any(|k| k.bus == bus.id && k.slot == slot.id) {
                continue;
            }
            kept.push(DeviceConfig {
                bus: bus.id.clone(),
                slot: slot.id,
                model: Some(device.model),
                rom: device.rom,
                function: device.function,
            });
        }
    }
    give_ethernet_addresses(&mut kept);
    (kept, refused)
}

/// Gives each Ethernet adapter of a list of device configs that has no address a random one,
/// with the maker's prefix of its device model, that no other adapter of the list has
fn give_ethernet_addresses(configs: &mut [DeviceConfig]) {
    let mut taken: Vec<[u8; 6]> = configs
        .iter()
        .filter_map(|config| match &config.function {
            FunctionConfig::Ethernet(ethernet) => ethernet.mac,
            _ => None,
        })
        .collect();
    for config in configs {
        let (Some(model), FunctionConfig::Ethernet(ethernet)) =
            (config.model, &mut config.function)
        else {
            continue;
        };
        if let FunctionInfo::Ethernet(info) = model.info().function
            && ethernet.mac.is_none()
        {
            let mac = new_ethernet_address(info.oui, &taken);
            taken.push(mac);
            ethernet.mac = Some(mac);
        }
    }
}

/// One kind of device. It fits one type of bus and provides one function.
///
/// The name of a variant is what workspaces store: it must not change once released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
pub enum DeviceModel {
    ScsiHardDisk,
    ScsiCdrom,
    #[cfg(feature = "ethernet")]
    DaynaPortScsiLink,
    #[cfg(feature = "printer")]
    LaserWriterIISC,
    Mac400KDrive,
    Mac800KDrive,
    Mac800KPwmDrive,
    MacSuperDrive,
    DisplayCard824,
    MacIIVideoCard,
    SE30Video,
    CompactVideo,
    PortableVideo,
    V8Video,
    SonoraVideo,
    DafbVideo,
    MemcJrVideo,
    DjMemcVideo,
    AppleIIePdsCard,
    UniDisk35,
    Apple525Drive,
}

impl DeviceModel {
    pub const fn info(self) -> &'static ModelInfo {
        match self {
            Self::ScsiHardDisk => &scsi::disk::INFO,
            Self::ScsiCdrom => &scsi::cdrom::INFO,
            #[cfg(feature = "ethernet")]
            Self::DaynaPortScsiLink => &scsi::ethernet::INFO,
            #[cfg(feature = "printer")]
            Self::LaserWriterIISC => &scsi::printer::INFO,
            Self::Mac400KDrive => &swim::drive::INFO_400K,
            Self::Mac800KDrive => &swim::drive::INFO_800K,
            Self::Mac800KPwmDrive => &swim::drive::INFO_800K_PWM,
            Self::MacSuperDrive => &swim::drive::INFO_SUPERDRIVE,
            Self::DisplayCard824 => &nubus::mdc12::INFO,
            Self::MacIIVideoCard => &nubus::toby::INFO,
            Self::SE30Video => &nubus::se30video::INFO,
            Self::CompactVideo => &compact::video::INFO,
            Self::PortableVideo => &portable::video::INFO,
            Self::V8Video => &maclc::video::INFO,
            Self::SonoraVideo => &maclc::video::INFO_SONORA,
            Self::DafbVideo => &quadra::dafb::INFO,
            Self::MemcJrVideo => &quadra::dafb::INFO_MEMCJR,
            Self::DjMemcVideo => &quadra::dafb::INFO_DJMEMC,
            Self::AppleIIePdsCard => &pds_iie::INFO,
            Self::UniDisk35 => &pds_iie::unidisk::INFO,
            Self::Apple525Drive => &pds_iie::disk::INFO,
        }
    }
}

/// What the core knows about one device model, before any device of it exists
pub struct ModelInfo {
    pub model: DeviceModel,
    /// In the menus: free to change
    pub name: &'static str,
    /// The type of bus it fits
    pub bus: BusKind,
    /// The Mac models it fits, among those with that bus
    pub fits: fn(MacModel) -> bool,
    /// Never added or removed once the machine has run
    pub fixed: bool,
    /// The ROM it needs, if any
    pub rom: Option<RomInfo>,
    /// Its function, and what that function supports
    pub function: FunctionInfo,
    /// How a device of this device model is made. None: it is on the logic board.
    pub(crate) create: Option<fn(&DeviceConfig) -> Result<MadeDevice>>,
}

/// A new device, tagged with the type of bus it goes on
pub(crate) enum MadeDevice {
    /// A device for a SCSI bus
    Scsi(Box<dyn ScsiTarget>),
    /// A device for a drive connector of the floppy controller
    Swim(Box<dyn SwimBusDevice>),
    /// A NuBus card
    Nubus(Box<dyn NubusSlot>),
    /// A card for a processor-direct slot
    Pds(Box<dyn PdsSlot>),
}

/// The ROM file a device model needs, which the user provides
pub struct RomInfo {
    /// In the dialog: "Display Card 8-24 ROM"
    pub label: &'static str,
    /// SHA-256 of each ROM known to work, in hex
    pub digests: &'static [&'static str],
    /// The device works without the file
    pub optional: bool,
}

impl ModelInfo {
    /// A `ModelInfo` with what most device models have: they fit every Mac with the bus, are
    /// added and removed while the machine runs and need no ROM
    pub const fn base(
        model: DeviceModel,
        name: &'static str,
        bus: BusKind,
        function: FunctionInfo,
    ) -> Self {
        Self {
            model,
            name,
            bus,
            fits: Self::fits_all,
            fixed: false,
            rom: None,
            function,
            create: None,
        }
    }

    const fn fits_all(_mac: MacModel) -> bool {
        true
    }
}

/// What a slot on this type of bus can take, on this Mac model
pub fn models_for(bus: BusKind, mac: MacModel) -> impl Iterator<Item = &'static ModelInfo> {
    let has_bus = mac.buses().iter().any(|b| b.kind == bus);
    DeviceModel::iter()
        .map(DeviceModel::info)
        .filter(move |info| has_bus && info.bus == bus && (info.fits)(mac))
}

/// What the emulator can ask of a bus about its slots
pub(crate) trait BusOps {
    /// The device in a slot. Called by the emulator to reach a device, with the id of its slot.
    fn device(&mut self, slot: &str) -> Option<&mut dyn DeviceOps>;
    /// Puts a new device in an empty slot. Called by the emulator when a device is added.
    /// Refused by default.
    fn attach(&mut self, _slot: &str, _device: MadeDevice) -> Result<()> {
        bail!("This bus takes no device")
    }
    /// Removes the device in a slot. Called by the emulator when a device is removed. Refused
    /// by default.
    fn detach(&mut self, _slot: &str) -> Result<()> {
        bail!("No device of this bus can be removed")
    }
}

/// What the emulator can ask of any device, whichever bus it is on
pub(crate) trait DeviceOps {
    /// Which device model it is
    fn info(&self) -> &'static ModelInfo;

    /// Its ROM file. Called when the list of buses is built.
    fn rom(&self) -> Option<&Path> {
        None
    }

    /// The Config of its function, as it is now: the one the device was made with, with the
    /// changes the user asked for since. Called when the list of buses is built.
    fn config(&self) -> FunctionConfig;

    /// Its name for the user, if it has one. Called when the list of buses is built.
    fn name(&self) -> Option<String> {
        None
    }

    /// The device, seen through its function. Called by the emulator to run a command of the
    /// frontend on the device, or to read its status.
    fn function(&mut self) -> FunctionMut<'_>;

    /// What the debugger shows of the device. Called when the debugger asks for the state of
    /// the machine.
    fn debug_properties(&self) -> DebuggableProperties {
        vec![]
    }

    /// The buses this device adds to the machine, as the Apple IIe Card adds its disk port:
    /// each with its description, its slots empty, and the object that implements it. Called
    /// to reach a bus whose id isn't one of the Mac model's, and when the list of buses is
    /// built. None by default.
    fn extra_buses(&mut self) -> Vec<(Bus, &mut dyn BusOps)> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_of_a_video_are_in_its_lists() {
        for model in DeviceModel::iter() {
            let FunctionInfo::Display(display) = model.info().function else {
                continue;
            };
            assert!(
                display.monitors.is_empty() || display.default_monitor().is_some(),
                "{model:?}"
            );
            assert!(
                display.vram_sizes.is_empty() || display.default_vram_size().is_some(),
                "{model:?}"
            );
            assert!(display.vram_sizes.is_sorted(), "{model:?}");
        }
    }

    #[test]
    fn every_device_model_has_its_info() {
        for model in DeviceModel::iter() {
            assert_eq!(model.info().model, model);
        }
    }

    #[test]
    fn models_fit_the_macs_with_their_bus() {
        let models = |mac| models_for(BusKind::Scsi, mac).map(|info| info.model);
        let scsi = DeviceModel::iter().filter(|model| model.info().bus == BusKind::Scsi);
        assert!(models(MacModel::Plus).eq(scsi));
        // No SCSI on the first Macs
        assert_eq!(models(MacModel::Early512K).count(), 0);
    }

    fn config(bus: &str, slot: &str, model: Option<DeviceModel>) -> DeviceConfig {
        let function = match bus {
            bus_id::SCSI => FunctionConfig::Cdrom(CdromConfig::default()),
            bus_id::SWIM => FunctionConfig::Floppy,
            _ => FunctionConfig::Display(DisplayConfig::default()),
        };
        DeviceConfig {
            bus: bus.into(),
            slot: slot.into(),
            model,
            rom: None,
            function,
        }
    }

    #[test]
    fn a_list_is_completed_with_what_the_mac_model_has() {
        // Nothing asked: the two drives of an LC and its video
        let (kept, refused) = complete_device_configs(MacModel::LC, vec![]);
        assert_eq!(refused, []);
        assert_eq!(
            kept,
            [
                config(bus_id::SWIM, "1", Some(DeviceModel::MacSuperDrive)),
                config(bus_id::SWIM, "2", Some(DeviceModel::MacSuperDrive)),
                config(
                    bus_id::BUILT_IN,
                    built_in::VIDEO,
                    Some(DeviceModel::V8Video)
                ),
            ]
        );

        // A slot that the list names keeps what the list says, and a device config with no
        // device model gets the one of the Mac model
        let mut video = config(bus_id::BUILT_IN, built_in::VIDEO, None);
        video.function = FunctionConfig::Display(DisplayConfig {
            monitor: Some(crate::mac::MacMonitor::RGB12),
            vram_size: None,
        });
        let cdrom = config(bus_id::SCSI, "3", Some(DeviceModel::ScsiCdrom));
        let asked = vec![cdrom.clone(), video.clone()];
        let (kept, refused) = complete_device_configs(MacModel::LC, asked);
        assert_eq!(refused, []);
        video.model = Some(DeviceModel::V8Video);
        assert_eq!(
            kept,
            [
                cdrom,
                video,
                config(bus_id::SWIM, "1", Some(DeviceModel::MacSuperDrive)),
                config(bus_id::SWIM, "2", Some(DeviceModel::MacSuperDrive)),
            ]
        );

        // A drive of another type in the place of one of a Macintosh 128K
        let drive = config(bus_id::SWIM, "2", Some(DeviceModel::Mac800KPwmDrive));
        let (kept, refused) = complete_device_configs(MacModel::Early128K, vec![drive.clone()]);
        assert_eq!(refused, []);
        assert_eq!(
            kept,
            [
                drive,
                config(bus_id::SWIM, "1", Some(DeviceModel::Mac400KDrive)),
                config(
                    bus_id::BUILT_IN,
                    built_in::VIDEO,
                    Some(DeviceModel::CompactVideo)
                ),
            ]
        );
    }

    #[test]
    fn a_device_config_the_mac_model_cannot_use_is_refused() {
        let cdrom = config(bus_id::SCSI, "3", Some(DeviceModel::ScsiCdrom));
        let cases = [
            // The Mac model has no such bus
            (
                config(bus_id::NUBUS, "9", Some(DeviceModel::DisplayCard824)),
                "This Mac model has no such bus",
            ),
            // The bus has no such slot
            (
                config(bus_id::SCSI, "7", Some(DeviceModel::ScsiCdrom)),
                "The bus has no such slot",
            ),
            // An earlier device config names the slot
            (
                config(bus_id::SCSI, "3", Some(DeviceModel::ScsiHardDisk)),
                "Another device is in this slot already",
            ),
            // The device model doesn't fit the bus
            (
                config(bus_id::SCSI, "4", Some(DeviceModel::MacSuperDrive)),
                "SuperDrive doesn't fit this bus",
            ),
            // Nor this Mac model
            (
                config(
                    bus_id::BUILT_IN,
                    built_in::VIDEO,
                    Some(DeviceModel::SonoraVideo),
                ),
                "Built-in video doesn't fit this bus",
            ),
            // No device model, in a slot where the Mac model has no device
            (config(bus_id::SCSI, "5", None), "No device model is given"),
            // The Config of another function
            (
                config(bus_id::SWIM, "1", Some(DeviceModel::MacSuperDrive)),
                "",
            ),
        ];
        let mut asked = vec![cdrom.clone()];
        asked.extend(cases.iter().map(|(config, _)| config.clone()));
        // The last case: a floppy drive with the Config of a CD-ROM drive
        asked.last_mut().unwrap().function = FunctionConfig::Cdrom(CdromConfig::default());

        let (kept, refused) = complete_device_configs(MacModel::LC, asked);
        // The CD-ROM drive, and what the LC has
        assert_eq!(kept.len(), 4);
        assert_eq!(kept[0], cdrom);
        assert_eq!(refused.len(), cases.len());
        for (refused, (_, reason)) in refused.iter().zip(&cases[..cases.len() - 1]) {
            assert_eq!(refused.reason, *reason);
        }
        assert_eq!(
            refused.last().unwrap().reason,
            "The Config isn't that of the function of SuperDrive"
        );
    }

    #[cfg(feature = "ethernet")]
    #[test]
    fn the_ethernet_adapters_of_a_list_get_the_addresses_they_lack() {
        use crate::devices::{EthernetConfig, EthernetLinkType};

        let adapter = |slot: &str, mac| DeviceConfig {
            bus: bus_id::SCSI.into(),
            slot: slot.into(),
            model: Some(DeviceModel::DaynaPortScsiLink),
            rom: None,
            function: FunctionConfig::Ethernet(EthernetConfig {
                mac,
                link: EthernetLinkType::Down,
            }),
        };
        let given = [0x00, 0x80, 0x19, 1, 2, 3];
        let mut configs = [
            adapter("4", Some(given)),
            adapter("5", None),
            adapter("6", None),
        ];
        give_ethernet_addresses(&mut configs);
        let macs: Vec<_> = configs
            .iter()
            .map(|config| match &config.function {
                FunctionConfig::Ethernet(ethernet) => ethernet.mac.unwrap(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(macs[0], given);
        assert_eq!(macs[1][..3], [0x00, 0x80, 0x19]);
        assert_ne!(macs[1], macs[0]);
        assert_ne!(macs[2], macs[0]);
        assert_ne!(macs[2], macs[1]);
    }
}
