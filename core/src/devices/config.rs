//! How a device is described, to make one

use std::path::PathBuf;

use rand::Rng;
use serde::{Deserialize, Serialize};

use super::{DeviceModel, FunctionConfig};

/// The ids of the buses of a machine. They are written in the files a frontend saves: they
/// must not change.
pub mod bus_id {
    pub const SCSI: &str = "SCSI";
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
    fn a_new_ethernet_address_is_not_one_that_is_taken() {
        let oui = [0x00, 0x80, 0x19];
        let first = new_ethernet_address(oui, &[]);
        assert_eq!(first[..3], oui);
        assert_ne!(new_ethernet_address(oui, &[first]), first);
    }
}
