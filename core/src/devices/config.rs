//! How a device is described, to make one

use std::path::{Path, PathBuf};

use rand::Rng;
use serde::{Deserialize, Serialize};

use super::{DeviceModel, FunctionConfig};

/// The ids of the buses of a machine. They are written in the files a frontend saves: they
/// must not change.
pub mod bus_id {
    pub const SCSI: &str = "SCSI";
    pub const SWIM: &str = "SWIM";
    pub const NUBUS: &str = "NuBus";
    /// The processor-direct slot of the LC family
    pub const PDS_LC: &str = "PDS_LC";
    /// The processor-direct slot of the SE/30
    pub const PDS_SE30: &str = "PDS_SE30";
    /// The disk port of the Apple IIe Card in the processor-direct slot of an LC
    pub const IIE_DISK: &str = "Floppy:PDS_LC.0";
}

/// One device, as described in a list of devices: where it is, which device it is, and what
/// was chosen for it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceConfig {
    /// The id of its bus
    pub bus: String,
    /// The id of its slot on that bus
    pub slot: String,
    /// Its device model. None: the device the machine has there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<DeviceModel>,
    /// Its ROM file, if its device model needs one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rom: Option<PathBuf>,
    /// The Config of its function
    pub function: FunctionConfig,
}

impl DeviceConfig {
    /// Replaces each path of this device config, its ROM file and those in the Config of its
    /// function, by what `convert` returns for it. A frontend that saves device configs uses
    /// it to make their paths relative to its file, and absolute again.
    pub fn convert_paths(&mut self, convert: impl Fn(&Path) -> PathBuf) {
        let in_function = match &mut self.function {
            FunctionConfig::Floppy => None,
            FunctionConfig::HardDisk(disk) => disk.media.as_mut(),
            FunctionConfig::Cdrom(cdrom) => cdrom.folder.as_mut(),
            FunctionConfig::Ethernet(_) => None,
            FunctionConfig::Printer(printer) => printer.output_dir.as_mut(),
            FunctionConfig::Display(_) | FunctionConfig::AppleIIeCard(_) => None,
        };
        for path in [self.rom.as_mut(), in_function].into_iter().flatten() {
            *path = convert(path);
        }
    }
}

/// A new address for an Ethernet adapter, with the maker's prefix `oui` (from the
/// `EthernetInfo` of its device model), different from the addresses in `taken`: those of the
/// other adapters of the machine
pub fn new_ethernet_address(oui: [u8; 3], taken: &[[u8; 6]]) -> [u8; 6] {
    let mut rand = rand::rng();
    loop {
        let address = [
            oui[0],
            oui[1],
            oui[2],
            rand.random(),
            rand.random(),
            rand.random(),
        ];
        if !taken.contains(&address) {
            return address;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{CdromConfig, HardDiskConfig};

    #[test]
    fn a_device_config_is_written_as_the_documents_show_it() {
        let disk = DeviceConfig {
            bus: bus_id::SCSI.into(),
            slot: "0".into(),
            model: Some(DeviceModel::ScsiHardDisk),
            rom: None,
            function: FunctionConfig::HardDisk(HardDiskConfig {
                media: Some("a.img".into()),
            }),
        };
        let json = r#"{"bus":"SCSI","slot":"0","model":"ScsiHardDisk","function":{"HardDisk":{"media":"a.img"}}}"#;
        assert_eq!(serde_json::to_string(&disk).unwrap(), json);
        assert_eq!(serde_json::from_str::<DeviceConfig>(json).unwrap(), disk);

        // What is None is left out
        let cdrom = DeviceConfig {
            bus: bus_id::SCSI.into(),
            slot: "3".into(),
            model: None,
            rom: None,
            function: FunctionConfig::Cdrom(CdromConfig::default()),
        };
        let json = r#"{"bus":"SCSI","slot":"3","function":{"Cdrom":{}}}"#;
        assert_eq!(serde_json::to_string(&cdrom).unwrap(), json);
        assert_eq!(serde_json::from_str::<DeviceConfig>(json).unwrap(), cdrom);
    }

    #[test]
    fn every_path_of_a_device_config_is_converted() {
        let mut disk = DeviceConfig {
            bus: bus_id::SCSI.into(),
            slot: "0".into(),
            model: Some(DeviceModel::ScsiHardDisk),
            rom: Some("disk.rom".into()),
            function: FunctionConfig::HardDisk(HardDiskConfig {
                media: Some("a.img".into()),
            }),
        };
        disk.convert_paths(|path| Path::new("/there").join(path));
        assert_eq!(disk.rom, Some("/there/disk.rom".into()));
        assert_eq!(
            disk.function,
            FunctionConfig::HardDisk(HardDiskConfig {
                media: Some("/there/a.img".into()),
            })
        );

        // A path that isn't there stays so
        let mut cdrom = DeviceConfig {
            bus: bus_id::SCSI.into(),
            slot: "3".into(),
            model: Some(DeviceModel::ScsiCdrom),
            rom: None,
            function: FunctionConfig::Cdrom(CdromConfig::default()),
        };
        let before = cdrom.clone();
        cdrom.convert_paths(|_| panic!("No path to convert"));
        assert_eq!(cdrom, before);
    }

    #[test]
    fn a_new_ethernet_address_is_not_one_that_is_taken() {
        let oui = [0x00, 0x80, 0x19];
        let first = new_ethernet_address(oui, &[]);
        assert_eq!(first[..3], oui);
        assert_ne!(new_ethernet_address(oui, &[first]), first);
    }
}
