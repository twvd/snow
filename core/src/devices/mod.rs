//! The device model: the I/O devices of a machine that the frontend shows, configures or acts on
//!
//! See docs/misc/device-model.md.

mod config;
mod function;

pub use config::*;
pub use function::*;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;

use crate::mac::MacModel;
use crate::mac::scsi;
use crate::mac::scsi::target::ScsiTarget;

/// A type of bus. A device model fits one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusKind {
    Scsi,
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
    let has_bus = match bus {
        BusKind::Scsi => mac.has_scsi(),
    };
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

    /// The device, seen through its function. Called by the emulator to run a command of the
    /// frontend on the device, or to read its status.
    fn function(&mut self) -> FunctionMut<'_>;
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
        assert!(models(MacModel::Plus).eq(DeviceModel::iter()));
        // No SCSI on the first Macs
        assert_eq!(models(MacModel::Early512K).count(), 0);
    }
}
