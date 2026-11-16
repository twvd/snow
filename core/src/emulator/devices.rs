//! The emulator's device code
//!
//! It runs the frontend's commands on the devices of the machine. A device is reached by the
//! ids of its bus and of its slot: through the bus operations of its bus, then through its own
//! device operations. See docs/misc/device-model.md.

use std::path::Path;
#[cfg(feature = "printer")]
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use log::*;

use super::Emulator;
#[cfg(feature = "printer")]
use crate::devices::PrinterConfig;
use crate::devices::{
    CdromConfig, DeviceConfig, DeviceModel, FunctionConfig, FunctionMut, HardDiskConfig, bus_id,
    models_for,
};
#[cfg(feature = "ethernet")]
use crate::devices::{EthernetConfig, EthernetLinkType, FunctionInfo, new_ethernet_address};
#[cfg(feature = "ethernet")]
use crate::mac::scsi::bus::ScsiBus;
use crate::mac::scsi::disk_image::DiskImage;

impl Emulator {
    /// The device in a slot, seen through its function
    pub(super) fn device_function(&mut self, bus: &str, slot: &str) -> Result<FunctionMut<'_>> {
        let device = self
            .config
            .bus_ops(bus)
            .context("This machine has no such bus")?
            .device(slot)
            .context("There is no device in this slot")?;
        Ok(device.function())
    }

    /// Adds a device to the machine, in an empty slot. The device is made by the create
    /// function of its device model, attached, then given its Config with
    /// `configure_device()`. When that part of the Config can't be applied, the device stays and
    /// the error says why.
    pub fn add_device(&mut self, config: &DeviceConfig) -> Result<()> {
        let mac = self.model;
        let bus = self
            .config
            .bus_ops(&config.bus)
            .context("This machine has no such bus")?;
        if bus.device(&config.slot).is_some() {
            bail!("The slot has a device");
        }
        let model = config.model.context("No device model is given")?;
        let info = model.info();
        if !models_for(info.bus, mac).any(|m| m.model == model) {
            bail!("{} doesn't fit this machine", info.name);
        }
        let create = info
            .create
            .with_context(|| format!("{} can't be added to a machine", info.name))?;
        bus.attach(&config.slot, create(config)?)?;

        // A CD-ROM drive plays audio CDs on the host's sound output
        let audio_provider = self.audio_provider.clone();
        let function = self.device_function(&config.bus, &config.slot)?;
        if let (FunctionMut::Cdrom(cdrom), Some(provider)) = (function, audio_provider)
            && let Err(e) = cdrom.set_audio_provider(&mut *provider.lock().unwrap())
        {
            error!("Failed setting CD audio provider: {}", e);
        }

        self.configure_device(config)
    }

    /// Gives the device in a slot what a Config can change on a device that exists: the link
    /// of an Ethernet adapter and the folder of a CD-ROM drive. The slot must hold a device of
    /// the Config's device model, if it names one. When that part of the Config can't be
    /// applied, the device stays and the error says why.
    fn configure_device(&mut self, config: &DeviceConfig) -> Result<()> {
        let device = self
            .config
            .bus_ops(&config.bus)
            .context("This machine has no such bus")?
            .device(&config.slot)
            .context("There is no device in this slot")?;
        if config.model.is_some_and(|m| m != device.info().model) {
            bail!("The slot has another device");
        }
        match (
            self.device_function(&config.bus, &config.slot)?,
            &config.function,
        ) {
            (FunctionMut::Ethernet(ethernet), FunctionConfig::Ethernet(config))
                if ethernet.status().link != config.link =>
            {
                ethernet.set_link(config.link.clone())
            }
            (
                FunctionMut::Cdrom(cdrom),
                FunctionConfig::Cdrom(CdromConfig {
                    folder: Some(folder),
                }),
            ) if cdrom.status().folder.as_ref() != Some(folder) => cdrom.set_folder(folder),
            _ => Ok(()),
        }
    }

    /// Removes the device in a slot from the machine
    pub fn remove_device(&mut self, bus: &str, slot: &str) -> Result<()> {
        self.config
            .bus_ops(bus)
            .context("This machine has no such bus")?
            .detach(slot)
    }

    // The helpers by SCSI ID: the id of a SCSI slot is the ID, in decimal

    fn add_scsi_device(
        &mut self,
        scsi_id: usize,
        model: DeviceModel,
        function: FunctionConfig,
    ) -> Result<()> {
        self.add_device(&DeviceConfig {
            bus: bus_id::SCSI.into(),
            slot: scsi_id.to_string(),
            model: Some(model),
            rom: None,
            function,
        })
    }

    pub fn load_hdd_image(&mut self, filename: &Path, scsi_id: usize) -> Result<()> {
        self.add_scsi_device(
            scsi_id,
            DeviceModel::ScsiHardDisk,
            FunctionConfig::HardDisk(HardDiskConfig {
                media: Some(filename.to_path_buf()),
            }),
        )
    }

    /// Attaches a hard disk whose image isn't a file: a disk with no image file, which is then
    /// given `image` as its storage
    pub fn attach_disk_image_at(
        &mut self,
        image: Box<dyn DiskImage>,
        scsi_id: usize,
    ) -> Result<()> {
        self.add_scsi_device(
            scsi_id,
            DeviceModel::ScsiHardDisk,
            FunctionConfig::HardDisk(HardDiskConfig::default()),
        )?;
        match self.device_function(bus_id::SCSI, &scsi_id.to_string())? {
            FunctionMut::HardDisk(disk) => disk.load_image(image),
            _ => bail!("No hard disk at SCSI ID {}", scsi_id),
        }
    }

    pub fn attach_cdrom(&mut self, id: usize) -> Result<()> {
        self.add_scsi_device(
            id,
            DeviceModel::ScsiCdrom,
            FunctionConfig::Cdrom(CdromConfig::default()),
        )?;
        info!("SCSI ID #{}: CD-ROM drive attached", id);
        Ok(())
    }

    /// Gives a CD-ROM drive a folder of images: the drive at `id`, or a new one if the SCSI ID
    /// is free
    pub fn attach_cdrom_folder(&mut self, id: usize, dir: &Path) -> Result<()> {
        let config = DeviceConfig {
            bus: bus_id::SCSI.into(),
            slot: id.to_string(),
            model: Some(DeviceModel::ScsiCdrom),
            rom: None,
            function: FunctionConfig::Cdrom(CdromConfig {
                folder: Some(dir.to_path_buf()),
            }),
        };
        if self.device_function(bus_id::SCSI, &config.slot).is_ok() {
            self.configure_device(&config)?;
        } else {
            self.add_device(&config)?;
        }
        info!("SCSI ID #{}: Toolbox managed CD-ROM folder attached", id);
        Ok(())
    }

    pub fn insert_cdrom_image_at(
        &mut self,
        image: Box<dyn DiskImage>,
        scsi_id: usize,
    ) -> Result<()> {
        match self.device_function(bus_id::SCSI, &scsi_id.to_string())? {
            FunctionMut::Cdrom(cdrom) => cdrom.load_image(image),
            _ => bail!("No CD-ROM drive at SCSI ID {}", scsi_id),
        }
    }

    /// Attaches an Ethernet adapter, with a new Ethernet address
    #[cfg(feature = "ethernet")]
    pub fn attach_ethernet(&mut self, id: usize) -> Result<()> {
        let mut taken = vec![];
        for other in 0..ScsiBus::MAX_TARGETS {
            if let Ok(FunctionMut::Ethernet(ethernet)) =
                self.device_function(bus_id::SCSI, &other.to_string())
            {
                taken.push(ethernet.status().mac);
            }
        }
        let model = DeviceModel::DaynaPortScsiLink;
        let FunctionInfo::Ethernet(ethernet) = model.info().function else {
            bail!("{} is not an Ethernet adapter", model.info().name);
        };
        self.add_scsi_device(
            id,
            model,
            FunctionConfig::Ethernet(EthernetConfig {
                mac: Some(new_ethernet_address(ethernet.oui, &taken)),
                link: EthernetLinkType::default(),
            }),
        )?;
        info!("SCSI ID #{}: Ethernet controller attached", id);
        Ok(())
    }

    #[cfg(feature = "printer")]
    pub fn attach_printer(&mut self, id: usize, output_dir: PathBuf) -> Result<()> {
        self.add_scsi_device(
            id,
            DeviceModel::LaserWriterIISC,
            FunctionConfig::Printer(PrinterConfig {
                output_dir: Some(output_dir),
            }),
        )?;
        info!("SCSI ID #{}: LaserWriter IISC printer attached", id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::devices::{CdromStatus, FunctionStatus};
    use crate::mac::MacModel;

    /// A Macintosh Plus, which has a SCSI bus
    fn plus() -> Emulator {
        Emulator::new(&vec![0; 128 * 1024], &[], MacModel::Plus)
            .unwrap()
            .0
    }

    fn scsi(slot: &str, model: Option<DeviceModel>, function: FunctionConfig) -> DeviceConfig {
        DeviceConfig {
            bus: bus_id::SCSI.into(),
            slot: slot.into(),
            model,
            rom: None,
            function,
        }
    }

    fn cdrom(slot: &str, folder: Option<&Path>) -> DeviceConfig {
        scsi(
            slot,
            Some(DeviceModel::ScsiCdrom),
            FunctionConfig::Cdrom(CdromConfig {
                folder: folder.map(Path::to_path_buf),
            }),
        )
    }

    fn status(emu: &mut Emulator, slot: &str) -> FunctionStatus {
        emu.device_function(bus_id::SCSI, slot).unwrap().status()
    }

    /// A folder of its own for a test, removed when the test is done
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("snow_devices_{}_{}", name, std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, size: usize) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, vec![0; size]).unwrap();
            path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_hard_disk_and_a_cdrom_drive_are_added_and_removed() {
        let dir = TestDir::new("add");
        let image = dir.file("disk.img", 1024 * 1024);
        let mut emu = plus();

        let disk = scsi(
            "0",
            Some(DeviceModel::ScsiHardDisk),
            FunctionConfig::HardDisk(HardDiskConfig {
                media: Some(image.clone()),
            }),
        );
        emu.add_device(&disk).unwrap();
        let FunctionStatus::HardDisk(disk) = status(&mut emu, "0") else {
            panic!("A hard disk has the HardDisk function");
        };
        assert_eq!(disk.media, Some(image));
        assert_eq!(disk.capacity, 1024 * 1024);

        emu.add_device(&cdrom("3", None)).unwrap();
        assert!(matches!(status(&mut emu, "3"), FunctionStatus::Cdrom(_)));

        emu.remove_device(bus_id::SCSI, "0").unwrap();
        assert!(emu.device_function(bus_id::SCSI, "0").is_err());
        // An empty slot has nothing to remove
        assert!(emu.remove_device(bus_id::SCSI, "0").is_err());
    }

    #[cfg(feature = "ethernet")]
    #[test]
    fn an_ethernet_adapter_gets_its_address_and_its_link() {
        let mut emu = plus();
        let mac = [0x00, 0x80, 0x19, 1, 2, 3];
        let adapter = |link| {
            scsi(
                "4",
                Some(DeviceModel::DaynaPortScsiLink),
                FunctionConfig::Ethernet(EthernetConfig {
                    mac: Some(mac),
                    link,
                }),
            )
        };
        emu.add_device(&adapter(EthernetLinkType::Down)).unwrap();
        let FunctionStatus::Ethernet(ethernet) = status(&mut emu, "4") else {
            panic!("An Ethernet adapter has the Ethernet function");
        };
        assert_eq!((ethernet.mac, ethernet.link), (mac, EthernetLinkType::Down));

        // No interface of the host has this index: the adapter stays, with its link down
        assert!(
            emu.configure_device(&adapter(EthernetLinkType::Bridge(u32::MAX)))
                .is_err()
        );
        let FunctionStatus::Ethernet(ethernet) = status(&mut emu, "4") else {
            panic!("An Ethernet adapter has the Ethernet function");
        };
        assert_eq!(ethernet.link, EthernetLinkType::Down);

        // Another adapter gets another address
        emu.attach_ethernet(5).unwrap();
        let FunctionStatus::Ethernet(other) = status(&mut emu, "5") else {
            panic!("An Ethernet adapter has the Ethernet function");
        };
        assert_ne!(other.mac, mac);

        // One made with no address gets a random one, with the maker's prefix
        emu.add_device(&scsi(
            "6",
            Some(DeviceModel::DaynaPortScsiLink),
            FunctionConfig::Ethernet(EthernetConfig {
                mac: None,
                link: EthernetLinkType::Down,
            }),
        ))
        .unwrap();
        let FunctionStatus::Ethernet(random) = status(&mut emu, "6") else {
            panic!("An Ethernet adapter has the Ethernet function");
        };
        assert_eq!(random.mac[..3], [0x00, 0x80, 0x19]);
    }

    #[cfg(feature = "printer")]
    #[test]
    fn a_printer_is_added() {
        let mut emu = plus();
        emu.attach_printer(6, PathBuf::from("pages")).unwrap();
        let FunctionStatus::Printer(printer) = status(&mut emu, "6") else {
            panic!("A printer has the Printer function");
        };
        assert_eq!(printer.output_dir, PathBuf::from("pages"));
    }

    #[test]
    fn a_taken_slot_keeps_its_device() {
        let dir = TestDir::new("taken");
        dir.file("disc.iso", 4096);
        let mut emu = plus();
        emu.add_device(&cdrom("3", None)).unwrap();

        // No device is added to a slot that has one
        let disk = scsi(
            "3",
            Some(DeviceModel::ScsiHardDisk),
            FunctionConfig::HardDisk(HardDiskConfig::default()),
        );
        assert!(emu.add_device(&disk).is_err());
        assert!(emu.add_device(&cdrom("3", Some(&dir.0))).is_err());
        assert!(emu.configure_device(&disk).is_err());
        assert!(matches!(status(&mut emu, "3"), FunctionStatus::Cdrom(_)));

        // The Config of the same device model, or of none, gives the drive its folder
        emu.configure_device(&cdrom("3", Some(&dir.0))).unwrap();
        let with_folder = FunctionStatus::Cdrom(CdromStatus {
            image: Some(dir.0.join("disc.iso")),
            folder: Some(dir.0.clone()),
        });
        assert_eq!(status(&mut emu, "3"), with_folder);
        let no_model = scsi("3", None, FunctionConfig::Cdrom(CdromConfig::default()));
        emu.configure_device(&no_model).unwrap();
        assert_eq!(status(&mut emu, "3"), with_folder);
        // An empty slot has nothing to configure
        assert!(emu.configure_device(&cdrom("4", None)).is_err());
    }

    #[test]
    fn a_folder_that_cannot_be_read_does_not_cost_the_drive() {
        let dir = TestDir::new("missing");
        let missing = dir.0.join("missing");
        let mut emu = plus();
        assert!(emu.add_device(&cdrom("3", Some(&missing))).is_err());
        assert_eq!(
            status(&mut emu, "3"),
            FunctionStatus::Cdrom(CdromStatus {
                image: None,
                folder: Some(missing),
            })
        );
    }

    #[test]
    fn what_the_machine_does_not_have_is_refused() {
        let mut emu = plus();
        let mut config = cdrom("3", None);
        config.bus = "NuBus".into();
        assert!(emu.add_device(&config).is_err());
        assert!(emu.add_device(&cdrom("7", None)).is_err());
        assert!(emu.add_device(&cdrom("03", None)).is_err());
        // An empty slot needs a device model
        let no_model = scsi("3", None, FunctionConfig::Cdrom(CdromConfig::default()));
        assert!(emu.add_device(&no_model).is_err());
        // A hard disk whose image can't be read isn't made
        let disk = scsi(
            "0",
            Some(DeviceModel::ScsiHardDisk),
            FunctionConfig::HardDisk(HardDiskConfig {
                media: Some("no such file".into()),
            }),
        );
        assert!(emu.add_device(&disk).is_err());
        assert!(emu.device_function(bus_id::SCSI, "0").is_err());

        // The first Macs have no SCSI bus
        let mut emu = Emulator::new(&vec![0; 64 * 1024], &[], MacModel::Early512K)
            .unwrap()
            .0;
        assert!(emu.add_device(&cdrom("3", None)).is_err());
    }

    #[test]
    fn a_disk_image_that_is_not_a_file_is_the_storage_of_a_disk() {
        struct Image(Vec<u8>);

        impl DiskImage for Image {
            fn byte_len(&self) -> usize {
                self.0.len()
            }

            fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8> {
                self.0[offset..offset + length].to_vec()
            }

            fn write_bytes(&mut self, offset: usize, data: &[u8]) {
                self.0[offset..offset + data.len()].copy_from_slice(data);
            }

            fn media_bytes(&self) -> Option<&[u8]> {
                Some(&self.0)
            }

            fn image_path(&self) -> Option<&Path> {
                None
            }
        }

        let mut emu = plus();
        emu.attach_disk_image_at(Box::new(Image(vec![0; 4096])), 0)
            .unwrap();
        let FunctionStatus::HardDisk(disk) = status(&mut emu, "0") else {
            panic!("A hard disk has the HardDisk function");
        };
        assert_eq!((disk.media, disk.capacity), (None, 4096));
        // The disk has its storage: another image is refused
        assert!(
            emu.attach_disk_image_at(Box::new(Image(vec![])), 0)
                .is_err()
        );
    }
}
