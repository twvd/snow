//! The device model: the I/O devices of a machine that the frontend shows, configures or acts on
//!
//! See docs/misc/device-model.md.

mod config;
mod function;

pub use config::*;
pub use function::*;

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;

use crate::debuggable::DebuggableProperties;
use crate::mac::MacModel;
use crate::mac::maclc::pds_iie;
use crate::mac::scsi;
use crate::mac::scsi::target::ScsiTarget;
use crate::mac::swim;
use crate::mac::swim::device::SwimBusDevice;

/// A type of bus. A device model fits one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusKind {
    Scsi,
    Swim,
    /// The disk port of an Apple IIe Card
    IieDisk,
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
/// It leaves out a device that the Mac model has in its slot without being asked.
pub fn device_configs(buses: &[Bus], mac: MacModel) -> Vec<DeviceConfig> {
    let defaults = mac.buses();
    let default_device = |bus: &Bus, slot: &Slot| {
        let bus = defaults.iter().find(|b| b.id == bus.id)?;
        bus.slots.iter().find(|s| s.id == slot.id)?.device.clone()
    };
    let mut configs = vec![];
    for bus in buses {
        for slot in &bus.slots {
            let Some(device) = &slot.device else {
                continue;
            };
            // A device only gives its name once it exists
            let unnamed = Device {
                name: None,
                ..device.clone()
            };
            if default_device(bus, slot) == Some(unnamed) {
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
    fn attach(&mut self, slot: &str, device: MadeDevice) -> Result<()>;
    /// Removes the device in a slot. Called by the emulator when a device is removed.
    fn detach(&mut self, slot: &str) -> Result<()>;
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
