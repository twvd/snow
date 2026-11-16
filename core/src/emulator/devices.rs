//! The emulator's device code
//!
//! It runs the frontend's commands on the devices of the machine. A device is reached by the
//! ids of its bus and of its slot: through the bus operations of its bus, then through its own
//! device operations. See docs/misc/device-model.md.

use std::path::Path;
#[cfg(feature = "printer")]
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use log::*;

use super::Emulator;
use super::comm::{EmulatorEvent, FddStatus};
#[cfg(feature = "printer")]
use crate::devices::PrinterConfig;
use crate::devices::{
    Bus, CdromConfig, Device, DeviceConfig, DeviceModel, EthernetFunction, FloppyFunction,
    FunctionConfig, FunctionInfo, FunctionMut, FunctionStatus, HardDiskConfig, bus_id,
    device_configs, models_for, new_ethernet_address,
};
use crate::mac::scsi::disk_image::DiskImage;
use crate::mac::swim::drive::DriveType;

impl Emulator {
    /// The device in a slot, seen through its function
    pub(super) fn function_in(&mut self, bus: &str, slot: &str) -> Result<FunctionMut<'_>> {
        let device = self
            .config
            .bus_ops(bus)
            .context("This machine has no such bus")?
            .device(slot)
            .context("There is no device in this slot")?;
        Ok(device.function())
    }

    /// The buses of the machine, their slots and the devices in them, as they are now
    pub fn buses(&mut self) -> Vec<Bus> {
        let mut buses = self.model.buses();
        for bus in &mut buses {
            let Some(ops) = self.config.bus_ops(&bus.id) else {
                continue;
            };
            for slot in &mut bus.slots {
                slot.device = ops.device(&slot.id).map(|device| Device {
                    model: device.info().model,
                    rom: device.rom().map(Path::to_path_buf),
                    function: device.config(),
                    name: device.name(),
                });
            }
        }
        buses
    }

    /// The machine, as the list of device configs it can be made from again
    pub fn devices(&mut self) -> Vec<DeviceConfig> {
        device_configs(&self.buses(), self.model)
    }

    /// Sends a new list of buses, and forgets the statuses that were sent: they are those of
    /// the list before it
    pub(super) fn send_buses(&mut self, buses: Vec<Bus>) -> Result<()> {
        self.device_statuses = vec![None; buses.iter().map(|bus| bus.slots.len()).sum()];
        self.buses = Arc::new(buses);
        self.event_sender
            .send(EmulatorEvent::Buses(self.buses.clone()))?;
        Ok(())
    }

    /// Sends the list of buses if it changed, then the status of each device whose status
    /// isn't the one that was sent last. Called at each status update.
    pub(super) fn devices_update(&mut self) -> Result<()> {
        let buses = self.buses();
        if *self.buses != buses {
            self.send_buses(buses)?;
        }

        let buses = self.buses.clone();
        let slots = buses
            .iter()
            .flat_map(|bus| bus.slots.iter().map(move |slot| (bus, slot)));
        for (index, (bus, slot)) in slots.enumerate() {
            let status = match self.function_in(&bus.id, &slot.id) {
                Ok(function) => Some(function.status()),
                Err(_) => None,
            };
            if self.device_statuses[index] != status {
                self.device_statuses[index] = status.clone();
                self.event_sender.send(EmulatorEvent::DeviceStatus(
                    bus.id.clone(),
                    slot.id.clone(),
                    status,
                ))?;
            }
        }
        Ok(())
    }

    /// What a message to the user calls a slot (see `Bus::slot_label()`)
    pub(super) fn slot_label(&self, bus: &str, slot: &str) -> String {
        match self.buses.iter().find(|b| b.id == bus) {
            Some(bus) => bus.slot_label(slot),
            None => format!("{} #{}", bus, slot),
        }
    }

    pub(super) fn load_media(&mut self, bus: &str, slot: &str, path: &Path) -> Result<()> {
        match self.function_in(bus, slot)? {
            FunctionMut::Cdrom(cdrom) => cdrom.load(path),
            _ => bail!("This device takes no media"),
        }
    }

    pub(super) fn cdrom_set_folder(&mut self, bus: &str, slot: &str, folder: &Path) -> Result<()> {
        match self.function_in(bus, slot)? {
            FunctionMut::Cdrom(cdrom) => cdrom.set_folder(folder),
            _ => bail!("This device isn't a CD-ROM drive"),
        }
    }

    pub(super) fn branch_disk(&mut self, bus: &str, slot: &str, path: &Path) -> Result<()> {
        match self.function_in(bus, slot)? {
            FunctionMut::HardDisk(disk) => disk.branch(path),
            _ => bail!("This device isn't a hard disk"),
        }
    }

    /// The floppy drive on a connector of the floppy controller, by its number
    pub(super) fn floppy(&mut self, drive: usize) -> Result<&mut dyn FloppyFunction> {
        let device = self
            .config
            .swim_mut()
            .drive_at(drive)
            .with_context(|| format!("Cannot insert disk: Drive {} not present", drive))?;
        match device.function() {
            FunctionMut::Floppy(floppy) => Ok(floppy),
            _ => bail!("Not a floppy drive"),
        }
    }

    /// The state of the floppy drive on a connector, for `EmulatorStatus`
    pub(super) fn fdd_status(&mut self, drive: usize) -> FddStatus {
        let Some(device) = self.config.swim_mut().drive_at(drive) else {
            return FddStatus {
                present: false,
                ejected: true,
                image_title: String::new(),
                dirty: false,
                drive_type: DriveType::None,
                writeback_supported: false,
                writeback_enabled: false,
            };
        };
        let drive_type = match device.info().model {
            DeviceModel::Mac400KDrive => DriveType::GCR400K,
            DeviceModel::Mac800KDrive => DriveType::GCR800K,
            DeviceModel::Mac800KPwmDrive => DriveType::GCR800KPWM,
            DeviceModel::MacSuperDrive => DriveType::SuperDrive,
            _ => DriveType::None,
        };
        let FunctionStatus::Floppy(status) = device.function().status() else {
            unreachable!()
        };
        FddStatus {
            present: true,
            ejected: status.ejected,
            image_title: status.title,
            dirty: status.dirty,
            drive_type,
            writeback_supported: status.writeback_supported,
            writeback_enabled: status.writeback_enabled,
        }
    }

    /// The Ethernet adapter in a slot
    pub(super) fn ethernet_in(
        &mut self,
        bus: &str,
        slot: &str,
    ) -> Result<&mut dyn EthernetFunction> {
        match self.function_in(bus, slot)? {
            FunctionMut::Ethernet(ethernet) => Ok(ethernet),
            _ => bail!("This device isn't an Ethernet adapter"),
        }
    }

    /// Adds a device to the machine, in an empty slot. The device is made by the create
    /// function of its device model, attached, then given its Config with
    /// `configure_device()`. When that part of the Config can't be applied, the device stays and
    /// the error says why.
    pub fn add_device(&mut self, config: &DeviceConfig) -> Result<()> {
        let mac = self.model;
        let buses = mac.buses();
        let kind = buses
            .iter()
            .find(|bus| bus.id == config.bus)
            .context("This machine has no such bus")?
            .kind;
        let bus = self
            .config
            .bus_ops(&config.bus)
            .context("This machine has no such bus")?;
        if bus.device(&config.slot).is_some() {
            bail!("The slot has a device");
        }
        let model = config.model.context("No device model is given")?;
        let info = model.info();
        if !models_for(kind, mac).any(|m| m.model == model) {
            bail!("{} doesn't fit this bus", info.name);
        }
        let create = info
            .create
            .with_context(|| format!("{} can't be added to a machine", info.name))?;
        let config = &self.with_ethernet_address(config, info.function);
        let bus = self
            .config
            .bus_ops(&config.bus)
            .context("This machine has no such bus")?;
        bus.attach(&config.slot, create(config)?)?;

        // A CD-ROM drive plays audio CDs on the host's sound output
        let audio_provider = self.audio_provider.clone();
        let function = self.function_in(&config.bus, &config.slot)?;
        if let (FunctionMut::Cdrom(cdrom), Some(provider)) = (function, audio_provider)
            && let Err(e) = cdrom.set_audio_provider(&mut *provider.lock().unwrap())
        {
            error!("Failed setting CD audio provider: {}", e);
        }

        self.configure_device(config)
    }

    /// The Config to make a device from: an Ethernet adapter whose Config has no address gets a
    /// random one with its maker's prefix, which no other adapter of the machine has
    fn with_ethernet_address(
        &mut self,
        config: &DeviceConfig,
        function: FunctionInfo,
    ) -> DeviceConfig {
        let mut config = config.clone();
        if let (FunctionConfig::Ethernet(ethernet), FunctionInfo::Ethernet(info)) =
            (&mut config.function, function)
            && ethernet.mac.is_none()
        {
            let mut taken = vec![];
            for bus in self.buses() {
                for slot in &bus.slots {
                    if let Ok(FunctionMut::Ethernet(other)) = self.function_in(&bus.id, &slot.id) {
                        taken.push(other.status().mac);
                    }
                }
            }
            ethernet.mac = Some(new_ethernet_address(info.oui, &taken));
        }
        config
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
            self.function_in(&config.bus, &config.slot)?,
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
        match self.function_in(bus_id::SCSI, &scsi_id.to_string())? {
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

    pub fn insert_cdrom_image_at(
        &mut self,
        image: Box<dyn DiskImage>,
        scsi_id: usize,
    ) -> Result<()> {
        match self.function_in(bus_id::SCSI, &scsi_id.to_string())? {
            FunctionMut::Cdrom(cdrom) => cdrom.load_image(image),
            _ => bail!("No CD-ROM drive at SCSI ID {}", scsi_id),
        }
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
    #[cfg(feature = "ethernet")]
    use crate::devices::{EthernetConfig, EthernetLinkType};
    use crate::emulator::comm::EmulatorEvent;
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
        emu.function_in(bus_id::SCSI, slot).unwrap().status()
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

    /// A machine of each Mac model, with the ROMs it asks for
    fn machine(model: MacModel) -> Emulator {
        use crate::mac::ExtraROMs;

        let extra_roms: &[ExtraROMs] = match model {
            MacModel::SE30 => &[ExtraROMs::SE30Video(&[])],
            MacModel::MacII | MacModel::MacIIFDHD | MacModel::MacIIx | MacModel::MacIIcx => {
                &[ExtraROMs::MDC12(&[])]
            }
            _ => &[],
        };
        Emulator::new(&vec![0; 1024 * 1024], extra_roms, model)
            .unwrap()
            .0
    }

    /// The device events sent since the last call: lists of buses, and statuses by slot id
    fn device_events(emu: &Emulator) -> (usize, Vec<(String, Option<FunctionStatus>)>) {
        let mut lists = 0;
        let mut statuses = vec![];
        for event in emu.event_recv.try_iter() {
            match event {
                EmulatorEvent::Buses(_) => lists += 1,
                EmulatorEvent::DeviceStatus(_, slot, status) => statuses.push((slot, status)),
                _ => (),
            }
        }
        (lists, statuses)
    }

    #[test]
    fn every_bus_of_a_mac_model_is_one_of_its_machine() {
        use strum::IntoEnumIterator;

        for model in MacModel::iter() {
            let mut emu = machine(model);
            for bus in model.buses() {
                let ops = emu
                    .config
                    .bus_ops(&bus.id)
                    .unwrap_or_else(|| panic!("{model} has no {} bus", bus.id));
                for slot in &bus.slots {
                    // An empty slot of the bus, not one the bus doesn't know
                    assert!(ops.device(&slot.id).is_none());
                    assert!(ops.detach(&slot.id).is_err());
                }
            }
            assert_eq!(emu.buses(), model.buses());
            assert_eq!(emu.devices(), vec![]);
        }
    }

    #[test]
    fn a_status_is_sent_when_it_changes() {
        let dir = TestDir::new("status");
        let image = dir.file("disc.iso", 4096);
        let mut emu = plus();
        // A new machine sends its list, and no status: its slots are empty
        assert_eq!(device_events(&emu), (1, vec![]));

        // A new device: the list again, then its status
        emu.add_device(&cdrom("3", None)).unwrap();
        emu.status_update().unwrap();
        let empty = CdromStatus {
            image: None,
            folder: None,
        };
        let sent = |cdrom| {
            (
                1,
                vec![("3".to_string(), Some(FunctionStatus::Cdrom(cdrom)))],
            )
        };
        assert_eq!(device_events(&emu), sent(empty));

        // Nothing changed: nothing is sent
        emu.status_update().unwrap();
        assert_eq!(device_events(&emu), (0, vec![]));

        // A disc: its status, once, and no list
        emu.load_media(bus_id::SCSI, "3", &image).unwrap();
        emu.status_update().unwrap();
        emu.status_update().unwrap();
        let (lists, statuses) = sent(CdromStatus {
            image: Some(image),
            folder: None,
        });
        assert_eq!(device_events(&emu), (lists - 1, statuses.clone()));

        // Another device: the new list, and every status again
        emu.add_device(&cdrom("4", None)).unwrap();
        emu.status_update().unwrap();
        let (lists, again) = device_events(&emu);
        assert_eq!(lists, 1);
        assert_eq!(again.len(), 2);
        assert_eq!(again[0], statuses[0]);

        // A device removed: the new list, and the status of what is left
        emu.remove_device(bus_id::SCSI, "3").unwrap();
        emu.status_update().unwrap();
        let (lists, left) = device_events(&emu);
        assert_eq!((lists, left.len()), (1, 1));
        assert_eq!(left[0].0, "4");
    }

    #[test]
    fn the_machine_is_reported_as_the_list_it_is_made_from() {
        let dir = TestDir::new("report");
        let image = dir.file("disk.img", 1024 * 1024);
        dir.file("disc.iso", 4096);
        // An image can only be in one drive
        let other = TestDir::new("report_other");
        other.file("other.iso", 4096);
        let mut emu = plus();
        let disk = scsi(
            "0",
            Some(DeviceModel::ScsiHardDisk),
            FunctionConfig::HardDisk(HardDiskConfig { media: Some(image) }),
        );
        let configs = vec![disk, cdrom("3", Some(&dir.0)), cdrom("5", None)];
        for config in &configs {
            emu.add_device(config).unwrap();
        }
        assert_eq!(emu.devices(), configs);

        // The list shows each device in its slot, with its Config as it is now
        let buses = emu.buses();
        let slot = |id: &str| buses[0].slots.iter().find(|s| s.id == id).unwrap();
        assert_eq!(buses[0].id, bus_id::SCSI);
        assert_eq!(slot("1").device, None);
        let drive = slot("3").device.as_ref().unwrap();
        assert_eq!(drive.model, DeviceModel::ScsiCdrom);
        assert_eq!(drive.function, configs[1].function);
        assert_eq!(drive.name, None);

        // A change of a Config is a change of the list
        device_events(&emu);
        emu.cdrom_set_folder(bus_id::SCSI, "5", &other.0).unwrap();
        emu.status_update().unwrap();
        assert_eq!(device_events(&emu).0, 1);
        assert_eq!(emu.devices()[2], cdrom("5", Some(&other.0)));

        // A second machine made from the report reports the same
        let report = emu.devices();
        drop(emu);
        let mut second = plus();
        for config in &report {
            second.add_device(config).unwrap();
        }
        assert_eq!(second.devices(), report);
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
        assert!(emu.function_in(bus_id::SCSI, "0").is_err());
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
        // The adapter has a name for the menus, and its Config has its address and its link
        assert_eq!(
            emu.buses()[0].slots[4].device,
            Some(Device {
                model: DeviceModel::DaynaPortScsiLink,
                rom: None,
                function: adapter(EthernetLinkType::Down).function,
                name: Some("DaynaPORT SCSI/Link".to_string()),
            })
        );

        // No interface of the host has this index: the adapter stays, with its link down
        assert!(
            emu.configure_device(&adapter(EthernetLinkType::Bridge(u32::MAX)))
                .is_err()
        );
        let FunctionStatus::Ethernet(ethernet) = status(&mut emu, "4") else {
            panic!("An Ethernet adapter has the Ethernet function");
        };
        assert_eq!(ethernet.link, EthernetLinkType::Down);

        // One made with no address gets a random one, with the maker's prefix, which its
        // Config then has
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
        let device = emu.buses()[0].slots[6].device.clone().unwrap();
        let FunctionConfig::Ethernet(config) = device.function else {
            panic!("An Ethernet adapter has an Ethernet Config");
        };
        assert_eq!(config.mac, Some(random.mac));
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
    fn a_cdrom_drive_gets_the_sound_output() {
        use crate::renderer::{AudioProvider, AudioSink, null_audio_sink};
        use std::sync::Mutex;

        /// Counts the streams it is asked for
        struct Provider(usize);

        impl AudioProvider for Provider {
            fn create_stream(&mut self, _: i32, _: u8, _: u16) -> Result<Box<dyn AudioSink>> {
                self.0 += 1;
                Ok(null_audio_sink())
            }
        }

        // The machine's own sound asks for its streams
        let provider = Arc::new(Mutex::new(Provider(0)));
        plus().set_audio_provider(provider.clone()).unwrap();
        let machine = provider.lock().unwrap().0;

        // A drive that is there when the sound output is set, and one added after
        let provider = Arc::new(Mutex::new(Provider(0)));
        let mut emu = plus();
        emu.add_device(&cdrom("3", None)).unwrap();
        emu.set_audio_provider(provider.clone()).unwrap();
        assert_eq!(provider.lock().unwrap().0, machine + 1);
        emu.add_device(&cdrom("4", None)).unwrap();
        assert_eq!(provider.lock().unwrap().0, machine + 2);
    }

    #[test]
    fn a_message_names_a_scsi_slot_by_its_id() {
        let emu = plus();
        assert_eq!(emu.slot_label(bus_id::SCSI, "3"), "SCSI ID #3");
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
        assert!(emu.function_in(bus_id::SCSI, "0").is_err());

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
