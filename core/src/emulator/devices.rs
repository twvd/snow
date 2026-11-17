//! The emulator's device code
//!
//! It runs the frontend's commands on the devices of the machine. A device is reached by the
//! ids of its bus and of its slot: through the bus operations of its bus, then through its own
//! device operations. See docs/misc/device-model.md.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use log::*;

use super::Emulator;
use super::comm::EmulatorEvent;
#[cfg(feature = "printer")]
use crate::devices::PrinterConfig;
use crate::devices::{
    Bus, BusKind, BusOps, CdromConfig, Device, DeviceConfig, DeviceModel, DisplayHandle,
    EthernetFunction, FloppyFunction, FunctionConfig, FunctionInfo, FunctionMut, HardDiskConfig,
    Slot, bus_id, device_configs, models_for, new_ethernet_address,
};
use crate::mac::scsi::disk_image::DiskImage;

/// The slots of a list of buses that hold a floppy drive
fn floppy_slots(buses: &[Bus]) -> impl Iterator<Item = (&Bus, &Slot)> {
    buses
        .iter()
        .flat_map(|bus| bus.slots.iter().map(move |slot| (bus, slot)))
        .filter(|(_, slot)| {
            slot.device
                .as_ref()
                .is_some_and(|d| matches!(d.model.info().function, FunctionInfo::Floppy(_)))
        })
}

impl Emulator {
    /// The bus with this id: one of the Mac model's, which the machine gives, or one that a
    /// device of the machine adds, which that device gives
    pub(super) fn bus_ops(&mut self, id: &str) -> Option<&mut dyn BusOps> {
        if self.model.buses().iter().any(|bus| bus.id == id) {
            return self.config.bus_ops(id);
        }
        let (_, bus, slot) = self
            .added_buses()
            .into_iter()
            .find(|(added, _, _)| added.id == id)?;
        let device = self.config.bus_ops(&bus)?.device(&slot)?;
        device
            .extra_buses()
            .into_iter()
            .find(|(added, _)| added.id == id)
            .map(|(_, ops)| ops)
    }

    /// The buses that devices in the slots of the Mac model's buses add, as the Apple IIe Card
    /// adds its disk port: each with the ids of the bus and of the slot of the device that adds
    /// it
    fn added_buses(&mut self) -> Vec<(Bus, String, String)> {
        let mut added = vec![];
        for bus in self.model.buses() {
            let Some(ops) = self.config.bus_ops(&bus.id) else {
                continue;
            };
            for slot in bus.slots {
                let Some(device) = ops.device(&slot.id) else {
                    continue;
                };
                for (extra, _) in device.extra_buses() {
                    added.push((extra, bus.id.clone(), slot.id.clone()));
                }
            }
        }
        added
    }

    /// The device in a slot, seen through its function
    pub(super) fn function_in(&mut self, bus: &str, slot: &str) -> Result<FunctionMut<'_>> {
        let device = self
            .bus_ops(bus)
            .context("This machine has no such bus")?
            .device(slot)
            .context("There is no device in this slot")?;
        Ok(device.function())
    }

    /// The buses of the machine, their slots and the devices in them, as they are now: the
    /// buses of the Mac model, then those that devices add
    pub fn buses(&mut self) -> Vec<Bus> {
        let mut buses = self.model.buses();
        // A bus that a device adds goes at the end of the list, and is filled in its turn
        let mut index = 0;
        while index < buses.len() {
            let mut added = vec![];
            if let Some(ops) = self.bus_ops(&buses[index].id) {
                for slot in &mut buses[index].slots {
                    slot.device = ops.device(&slot.id).map(|device| {
                        added.extend(device.extra_buses().into_iter().map(|(bus, _)| bus));
                        Device {
                            model: device.info().model,
                            rom: device.rom().map(Path::to_path_buf),
                            function: device.config(),
                            name: device.name(),
                        }
                    });
                }
            }
            buses.extend(added);
            index += 1;
        }
        buses
    }

    /// Shows a blank screen on every display of the machine: the devices of its buses whose
    /// function is Display. Called when the machine resets or stops.
    pub(super) fn video_blank(&mut self) -> Result<()> {
        for bus in self.buses() {
            for slot in &bus.slots {
                if let Ok(FunctionMut::Display(display)) = self.function_in(&bus.id, &slot.id) {
                    display.blank()?;
                }
            }
        }
        Ok(())
    }

    /// The displays of the machine, for the frontend: one handle per device whose function is
    /// Display and that has something on its connector, in the order of the list of buses.
    /// Called once, when the machine is made or restored from a state.
    pub(super) fn displays(&mut self) -> Vec<DisplayHandle> {
        let mut displays = vec![];
        for bus in self.buses() {
            for slot in &bus.slots {
                let Some(device) = &slot.device else {
                    continue;
                };
                let Ok(FunctionMut::Display(display)) = self.function_in(&bus.id, &slot.id) else {
                    continue;
                };
                if !display.status().connected {
                    continue;
                }
                let name = if bus.kind == BusKind::BuiltIn {
                    "Built-in display".to_string()
                } else {
                    format!(
                        "{} slot {}: {}",
                        bus.title,
                        slot.id,
                        device.model.info().name
                    )
                };
                displays.push(DisplayHandle {
                    name,
                    bus: bus.id.clone(),
                    slot: slot.id.clone(),
                    index: 0,
                    receiver: display.frames(),
                });
            }
        }
        displays
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

    pub(super) fn load_media(
        &mut self,
        bus: &str,
        slot: &str,
        path: &Path,
        read_only: bool,
    ) -> Result<()> {
        match self.function_in(bus, slot)? {
            FunctionMut::Floppy(floppy) => floppy.load(path, read_only),
            FunctionMut::Cdrom(cdrom) => cdrom.load(path),
            FunctionMut::HardDisk(disk) => disk.load(path),
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

    /// The floppy drive in a slot
    pub(super) fn floppy_in(&mut self, bus: &str, slot: &str) -> Result<&mut dyn FloppyFunction> {
        match self.function_in(bus, slot)? {
            FunctionMut::Floppy(floppy) => Ok(floppy),
            _ => bail!("This device isn't a floppy drive"),
        }
    }

    /// Writes the disk of a floppy drive back to the file it came from, if a save is due or
    /// `force` is set, and reports a failed save to the user
    pub(super) fn floppy_write_back(&mut self, bus: &str, slot: &str, force: bool) {
        if let Ok(floppy) = self.floppy_in(bus, slot)
            && let Err(e) = floppy.write_back(force)
        {
            self.user_error(&format!("{}: {:#}", self.slot_label(bus, slot), e));
        }
    }

    /// Runs the writeback saves that are due on every floppy drive. With `force`, saves any
    /// dirty disk that has writeback armed.
    pub(super) fn flush_pending_writebacks(&mut self, force: bool) {
        let buses = self.buses.clone();
        for (bus, slot) in floppy_slots(&buses) {
            self.floppy_write_back(&bus.id, &slot.id, force);
        }
    }

    /// Sends the frontend the image of each disk that was ejected since the last call
    pub(super) fn send_ejected_floppies(&mut self) -> Result<()> {
        // An ejected disk is written back before its image is handed over
        self.flush_pending_writebacks(false);
        let buses = self.buses.clone();
        for (bus, slot) in floppy_slots(&buses) {
            if let Ok(floppy) = self.floppy_in(&bus.id, &slot.id)
                && let Some(image) = floppy.take_ejected_image()
            {
                self.event_sender.send(EmulatorEvent::FloppyEjected(
                    bus.id.clone(),
                    slot.id.clone(),
                    image,
                ))?;
            }
        }
        Ok(())
    }

    /// Seeds the random bits of every floppy drive
    pub(super) fn seed_weak_bits(&mut self, seed: Option<u64>) {
        let buses = self.buses.clone();
        for (bus, slot) in floppy_slots(&buses) {
            if let Ok(floppy) = self.floppy_in(&bus.id, &slot.id) {
                floppy.seed_weak_bits(seed);
            }
        }
    }

    pub(super) fn eject_media(&mut self, bus: &str, slot: &str) -> Result<()> {
        // The writes since the last save go with the disk
        self.floppy_write_back(bus, slot, true);
        match self.function_in(bus, slot)? {
            FunctionMut::Floppy(floppy) => {
                floppy.eject();
                Ok(())
            }
            _ => bail!("This device has no disk to eject"),
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
    ///
    /// A device model that is `fixed`, such as a floppy drive, is refused: the machine only
    /// gets those when it is made.
    pub fn add_device(&mut self, config: &DeviceConfig) -> Result<()> {
        self.add_device_to(config, false)
    }

    /// Adds a device to the machine: one that is being made if `new_machine` is set, which
    /// then takes the device models that are `fixed` too
    pub(super) fn add_device_to(&mut self, config: &DeviceConfig, new_machine: bool) -> Result<()> {
        let mac = self.model;
        let buses = mac.buses();
        let kind = buses
            .iter()
            .find(|bus| bus.id == config.bus)
            .context("This machine has no such bus")?
            .kind;
        let bus = self
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
        if info.fixed && !new_machine {
            bail!("{} can't be added once the machine is made", info.name);
        }
        let create = info
            .create
            .with_context(|| format!("{} can't be added to a machine", info.name))?;
        let config = &self.with_ethernet_address(config, info.function);
        let bus = self
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

        // A printer writes its pages to the default folder, unless its Config names one
        let default_print_dir = self.default_print_dir.clone();
        let function = self.function_in(&config.bus, &config.slot)?;
        if let (FunctionMut::Printer(printer), Some(dir)) = (function, default_print_dir) {
            printer.set_default_dir(&dir);
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
    pub(super) fn configure_device(&mut self, config: &DeviceConfig) -> Result<()> {
        let device = self
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

    /// Gives every printer of the machine the folder for its pages when its Config names none,
    /// and keeps it for the printers added later
    pub(super) fn set_default_print_dir(&mut self, dir: PathBuf) {
        for bus in self.buses() {
            for slot in &bus.slots {
                if let Ok(FunctionMut::Printer(printer)) = self.function_in(&bus.id, &slot.id) {
                    printer.set_default_dir(&dir);
                }
            }
        }
        self.default_print_dir = Some(dir);
    }

    /// Removes the device in a slot from the machine
    pub fn remove_device(&mut self, bus: &str, slot: &str) -> Result<()> {
        let bus = self.bus_ops(bus).context("This machine has no such bus")?;
        if let Some(device) = bus.device(slot)
            && device.info().fixed
        {
            bail!(
                "{} can't be removed once the machine is made",
                device.info().name
            );
        }
        bus.detach(slot)
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
    use crate::devices::{CdromStatus, DisplayConfig, DisplayStatus, FunctionStatus, built_in};
    #[cfg(feature = "ethernet")]
    use crate::devices::{EthernetConfig, EthernetLinkType};
    use crate::emulator::MachineConfig;
    use crate::emulator::comm::EmulatorEvent;
    use crate::mac::{MacModel, MacMonitor};

    /// A Macintosh Plus, which has a SCSI bus
    fn plus() -> Emulator {
        make(MacModel::Plus, vec![]).unwrap()
    }

    /// A machine made from a list of devices, all of which it must take
    fn make(model: MacModel, devices: Vec<DeviceConfig>) -> Result<Emulator> {
        let rom = vec![0; 1024 * 1024];
        let (emu, _, refused) = Emulator::new(&MachineConfig::new(&rom, model), devices)?;
        assert_eq!(refused, []);
        Ok(emu)
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

    /// The device in a slot, as the list of buses has it
    #[cfg(feature = "ethernet")]
    fn device_in(emu: &mut Emulator, bus: &str, slot: &str) -> Option<Device> {
        let buses = emu.buses();
        let bus = buses.iter().find(|b| b.id == bus).unwrap();
        let slot = bus.slots.iter().find(|s| s.id == slot).unwrap();
        slot.device.clone()
    }

    fn status(emu: &mut Emulator, slot: &str) -> FunctionStatus {
        emu.function_in(bus_id::SCSI, slot).unwrap().status()
    }

    /// A folder of its own for a test, removed when the test is done
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};

            // Two tests may ask for a folder of the same name at the same time
            static COUNT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "snow_devices_{}_{}_{}",
                name,
                std::process::id(),
                COUNT.fetch_add(1, Ordering::Relaxed)
            ));
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

    /// The device config of a video card in a NuBus slot
    fn video_card(
        slot: &str,
        model: DeviceModel,
        rom: Option<&Path>,
        monitor: Option<MacMonitor>,
    ) -> DeviceConfig {
        DeviceConfig {
            bus: bus_id::NUBUS.into(),
            slot: slot.into(),
            model: Some(model),
            rom: rom.map(Path::to_path_buf),
            function: FunctionConfig::Display(DisplayConfig {
                monitor,
                vram_size: None,
            }),
        }
    }

    /// The device configs that a machine of these tests needs to have a screen, with `rom` as
    /// the ROM file of its video: the video of an SE/30, and an 8-24 card in the first slot of
    /// a Macintosh II
    fn video_configs(model: MacModel, rom: &Path) -> Vec<DeviceConfig> {
        match model {
            MacModel::SE30 => vec![DeviceConfig {
                bus: bus_id::BUILT_IN.into(),
                slot: built_in::VIDEO.into(),
                model: Some(DeviceModel::SE30Video),
                rom: Some(rom.to_path_buf()),
                function: FunctionConfig::Display(DisplayConfig::default()),
            }],
            MacModel::MacII | MacModel::MacIIFDHD | MacModel::MacIIx | MacModel::MacIIcx => {
                vec![video_card(
                    "9",
                    DeviceModel::DisplayCard824,
                    Some(rom),
                    None,
                )]
            }
            _ => vec![],
        }
    }

    /// A machine of each Mac model, with what it needs to have a screen
    fn machine(model: MacModel) -> Emulator {
        let dir = TestDir::new("machine");
        let rom = dir.file("video.rom", 0);
        make(model, video_configs(model, &rom)).unwrap()
    }

    /// The device events sent since the last call: lists of buses, and the statuses of the
    /// SCSI bus by slot id
    fn device_events(emu: &Emulator) -> (usize, Vec<(String, Option<FunctionStatus>)>) {
        let mut lists = 0;
        let mut statuses = vec![];
        for event in emu.event_recv.try_iter() {
            match event {
                EmulatorEvent::Buses(_) => lists += 1,
                EmulatorEvent::DeviceStatus(bus, slot, status) if bus == bus_id::SCSI => {
                    statuses.push((slot, status));
                }
                _ => (),
            }
        }
        (lists, statuses)
    }

    #[test]
    fn every_bus_of_a_mac_model_is_one_of_its_machine() {
        use strum::IntoEnumIterator;

        let dir = TestDir::new("buses");
        let rom = dir.file("video.rom", 0);
        for model in MacModel::iter() {
            // An SE/30 of these tests has the ROM file of its video, and a Macintosh II a
            // video card in its first slot
            let devices = video_configs(model, &rom);
            let mut emu = make(model, devices.clone()).unwrap();
            let mut buses = model.buses();
            for config in &devices {
                let bus = buses.iter_mut().find(|b| b.id == config.bus).unwrap();
                let slot = bus.slots.iter_mut().find(|s| s.id == config.slot).unwrap();
                slot.device = Some(Device {
                    model: config.model.unwrap(),
                    rom: config.rom.clone(),
                    function: config.function.clone(),
                    name: None,
                });
            }
            for bus in &buses {
                let ops = emu
                    .config
                    .bus_ops(&bus.id)
                    .unwrap_or_else(|| panic!("{model} has no {} bus", bus.id));
                for slot in &bus.slots {
                    match &slot.device {
                        // The device the Mac model has there
                        Some(device) => {
                            let model = ops.device(&slot.id).unwrap().info().model;
                            assert_eq!(model, device.model);
                        }
                        // An empty slot of the bus, not one the bus doesn't know
                        None => {
                            assert!(ops.device(&slot.id).is_none());
                            assert!(ops.detach(&slot.id).is_err());
                        }
                    }
                }
            }
            assert_eq!(emu.buses(), buses);
            assert_eq!(emu.devices(), devices);
        }
    }

    /// A machine with a monitor and a VRAM size chosen for its video: its built-in video, or
    /// the card of a Macintosh II
    fn machine_with_video(
        model: MacModel,
        monitor: Option<MacMonitor>,
        vram_size: Option<usize>,
    ) -> Result<Emulator> {
        let dir = TestDir::new("video");
        let rom = dir.file("video.rom", 0);
        let video = match model {
            MacModel::MacII => video_card("9", DeviceModel::DisplayCard824, Some(&rom), monitor),
            // The video the Mac model has, whichever device model that is
            _ => DeviceConfig {
                bus: bus_id::BUILT_IN.into(),
                slot: built_in::VIDEO.into(),
                model: None,
                rom: None,
                function: FunctionConfig::Display(DisplayConfig { monitor, vram_size }),
            },
        };
        make(model, vec![video])
    }

    /// A device config of the Built-in bus is given to the device the machine has there, with
    /// `configure_device()`, as `Emulator::new()` does
    #[test]
    fn a_built_in_device_is_configured() {
        let mut emu = make(MacModel::LC, vec![]).unwrap();
        let mut video = DeviceConfig {
            bus: bus_id::BUILT_IN.into(),
            slot: built_in::VIDEO.into(),
            model: None,
            rom: None,
            function: FunctionConfig::Display(DisplayConfig::default()),
        };
        emu.configure_device(&video).unwrap();
        video.model = Some(DeviceModel::SonoraVideo);
        assert!(emu.configure_device(&video).is_err());
    }

    #[test]
    fn a_built_in_video_is_reported_with_what_was_chosen() {
        let video = |model, monitor, vram_size| DeviceConfig {
            bus: bus_id::BUILT_IN.into(),
            slot: built_in::VIDEO.into(),
            model: Some(model),
            rom: None,
            function: FunctionConfig::Display(DisplayConfig { monitor, vram_size }),
        };
        let cases = [
            (MacModel::LC, DeviceModel::V8Video, 256 * 1024),
            (MacModel::LCIII, DeviceModel::SonoraVideo, 512 * 1024),
            (MacModel::LC475, DeviceModel::MemcJrVideo, 512 * 1024),
            (MacModel::Quadra800, DeviceModel::DjMemcVideo, 512 * 1024),
        ];
        for (mac, model, vram_size) in cases {
            let monitor = Some(MacMonitor::RGB12);
            let mut emu = machine_with_video(mac, monitor, Some(vram_size)).unwrap();
            assert_eq!(
                emu.devices(),
                [video(model, monitor, Some(vram_size))],
                "{mac}"
            );
            let status = emu
                .function_in(bus_id::BUILT_IN, built_in::VIDEO)
                .unwrap()
                .status();
            assert_eq!(
                status,
                FunctionStatus::Display(DisplayStatus { connected: true })
            );
        }

        // The Quadra 700 has one size of VRAM
        let monitor = Some(MacMonitor::RGB16);
        let mut emu = machine_with_video(MacModel::Quadra700, monitor, None).unwrap();
        assert_eq!(
            emu.devices(),
            [video(DeviceModel::DafbVideo, monitor, None)]
        );

        // With nothing chosen, the video is the one the Mac model has: it isn't reported
        let mut emu = machine_with_video(MacModel::LC, None, None).unwrap();
        assert_eq!(emu.devices(), []);
    }

    #[test]
    fn a_machine_with_no_screen_is_refused() {
        // The only video of a Macintosh II is its card
        assert!(machine_with_video(MacModel::MacII, Some(MacMonitor::RGB12), None).is_ok());
        let none = machine_with_video(MacModel::MacII, Some(MacMonitor::NoMonitor), None);
        let error = none.err().unwrap().to_string();
        assert!(error.contains("no screen"), "{error}");

        // So is one with no card, and the device config that couldn't be used is in the error
        assert!(make(MacModel::MacII, vec![]).is_err());
        let no_file = video_card(
            "9",
            DeviceModel::DisplayCard824,
            Some(Path::new("no such file")),
            None,
        );
        let error = make(MacModel::MacII, vec![no_file]).err().unwrap();
        let error = error.to_string();
        assert!(error.contains("no screen"), "{error}");
        assert!(error.contains("NuBus #9: Cannot read"), "{error}");

        // An SE/30 needs the ROM file of its video
        let error = make(MacModel::SE30, vec![]).err().unwrap().to_string();
        assert!(error.contains("needs its ROM file"), "{error}");
    }

    #[test]
    fn a_machine_is_made_from_a_list_of_devices() {
        use strum::IntoEnumIterator;

        let dir = TestDir::new("list");
        let rom = dir.file("video.rom", 0);
        for model in MacModel::iter() {
            let machine_rom = vec![0; 1024 * 1024];
            let machine = MachineConfig::new(&machine_rom, model);

            // With what it needs for a screen and nothing else, the machine has one display
            let mut devices = video_configs(model, &rom);
            let (mut emu, displays, refused) = Emulator::new(&machine, devices.clone()).unwrap();
            assert_eq!(refused, []);
            assert_eq!(displays.len(), 1, "{model}");
            let display = &displays[0];
            match model {
                MacModel::MacII | MacModel::MacIIFDHD | MacModel::MacIIx | MacModel::MacIIcx => {
                    assert_eq!(display.name, "NuBus slot 9: Macintosh Display Card 8-24");
                    assert_eq!(
                        (display.bus.as_str(), display.slot.as_str()),
                        ("NuBus", "9")
                    );
                }
                _ => {
                    assert_eq!(display.name, "Built-in display");
                    assert_eq!(display.bus, bus_id::BUILT_IN);
                    assert_eq!(display.slot, built_in::VIDEO);
                }
            }
            assert_eq!(emu.devices(), devices);

            // With devices of its buses, it reports the list it was made from, and a machine
            // made from that list reports the same
            if model.has_scsi() {
                devices.push(cdrom("3", None));
            }
            // A Mac with 400K drives takes an 800K drive
            let drive = model.buses()[0].slots[0].device.as_ref().unwrap().model;
            if drive == DeviceModel::Mac400KDrive {
                devices.push(DeviceConfig {
                    bus: bus_id::SWIM.into(),
                    slot: "1".into(),
                    model: Some(DeviceModel::Mac800KPwmDrive),
                    rom: None,
                    function: FunctionConfig::Floppy,
                });
            }
            let mut emu = make(model, devices.clone()).unwrap();
            let reported = emu.devices();
            assert_eq!(reported.len(), devices.len(), "{model}");
            for config in &devices {
                assert!(reported.contains(config), "{model}: {config:?}");
            }
            assert_eq!(make(model, reported.clone()).unwrap().devices(), reported);
        }
    }

    #[test]
    fn a_video_card_in_each_slot_is_a_display() {
        let dir = TestDir::new("cards");
        let rom = dir.file("video.rom", 0);
        let rom = Some(rom.as_path());
        let machine_rom = vec![0; 1024 * 1024];
        let machine = MachineConfig::new(&machine_rom, MacModel::MacII);

        // The displays are in the order of the slots, whatever the order of the list, and a
        // card with nothing on its connector isn't one
        let devices = vec![
            video_card("B", DeviceModel::MacIIVideoCard, rom, None),
            video_card(
                "A",
                DeviceModel::DisplayCard824,
                rom,
                Some(MacMonitor::NoMonitor),
            ),
            video_card("9", DeviceModel::DisplayCard824, rom, None),
        ];
        let (_, displays, refused) = Emulator::new(&machine, devices).unwrap();
        assert_eq!(refused, []);
        let names: Vec<_> = displays.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "NuBus slot 9: Macintosh Display Card 8-24",
                "NuBus slot B: Macintosh II Video Card (Toby)"
            ]
        );
        assert!(displays.iter().all(|d| d.index == 0));
    }

    #[test]
    fn a_device_config_that_cannot_be_used_does_not_stop_the_machine() {
        let dir = TestDir::new("refused");
        let no_image = scsi(
            "0",
            Some(DeviceModel::ScsiHardDisk),
            FunctionConfig::HardDisk(HardDiskConfig {
                media: Some(dir.0.join("no such image")),
            }),
        );
        let no_folder = cdrom("3", Some(&dir.0.join("no such folder")));
        let no_slot = cdrom("9", None);
        let devices = vec![
            no_image.clone(),
            cdrom("4", None),
            no_folder.clone(),
            no_slot.clone(),
        ];
        let rom = vec![0; 1024 * 1024];
        let machine = MachineConfig::new(&rom, MacModel::Plus);
        let (mut emu, _, refused) = Emulator::new(&machine, devices).unwrap();

        // What the list couldn't have comes first, then what couldn't be made
        let configs: Vec<_> = refused.iter().map(|r| &r.config).collect();
        assert_eq!(configs, [&no_slot, &no_image, &no_folder]);
        assert_eq!(refused[0].reason, "The bus has no such slot");
        assert!(refused.iter().all(|r| !r.reason.is_empty()));

        // The disk isn't there. The CD-ROM drive whose folder is missing is, empty.
        assert!(emu.function_in(bus_id::SCSI, "0").is_err());
        assert!(matches!(status(&mut emu, "4"), FunctionStatus::Cdrom(_)));
        let FunctionStatus::Cdrom(drive) = status(&mut emu, "3") else {
            panic!("A CD-ROM drive has the Cdrom function");
        };
        assert_eq!(drive.image, None);
    }

    #[test]
    fn a_video_card_is_made_from_its_rom_file() {
        let dir = TestDir::new("video_card");
        let rom = dir.file("card.rom", 4096);
        let card = video_card;
        let mut emu = machine(MacModel::MacII);

        // A second card, with the monitor chosen for it: the machine reports both as given
        let second = card(
            "A",
            DeviceModel::DisplayCard824,
            Some(&rom),
            Some(MacMonitor::RGB12),
        );
        emu.add_device_to(&second, true).unwrap();
        let third = card("B", DeviceModel::MacIIVideoCard, Some(&rom), None);
        emu.add_device_to(&third, true).unwrap();
        assert_eq!(emu.devices()[1..], [second.clone(), third]);
        assert_eq!(
            emu.function_in(bus_id::NUBUS, "A").unwrap().status(),
            FunctionStatus::Display(DisplayStatus { connected: true })
        );

        // A card with nothing on its connector shows no display
        let none = card(
            "C",
            DeviceModel::DisplayCard824,
            Some(&rom),
            Some(MacMonitor::NoMonitor),
        );
        emu.add_device_to(&none, true).unwrap();
        assert_eq!(
            emu.function_in(bus_id::NUBUS, "C").unwrap().status(),
            FunctionStatus::Display(DisplayStatus { connected: false })
        );

        // A card needs its ROM file, is only added when the machine is made, and goes in a
        // slot the machine has
        let no_rom = card("D", DeviceModel::DisplayCard824, None, None);
        assert!(emu.add_device_to(&no_rom, true).is_err());
        let missing = card(
            "D",
            DeviceModel::DisplayCard824,
            Some(&dir.0.join("no")),
            None,
        );
        assert!(emu.add_device_to(&missing, true).is_err());
        let late = card("D", DeviceModel::DisplayCard824, Some(&rom), None);
        assert!(emu.add_device(&late).is_err());
        let mut iicx = machine(MacModel::MacIIcx);
        assert!(iicx.add_device_to(&late, true).is_err());
        // No NuBus card goes in a machine without NuBus
        assert!(
            machine(MacModel::Plus)
                .add_device_to(&second, true)
                .is_err()
        );
    }

    #[test]
    fn the_apple_iie_card_is_made_from_a_device_config() {
        use crate::mac::maclc::pds_iie::{IieCardSettings, IieDiskDevice};

        let settings = IieCardSettings {
            security_jumper: false,
            disk_chain: [IieDiskDevice::UniDisk35, IieDiskDevice::Apple525],
        };
        let card = DeviceConfig {
            bus: bus_id::PDS_LC.into(),
            slot: "0".into(),
            model: Some(DeviceModel::AppleIIePdsCard),
            rom: None,
            function: FunctionConfig::AppleIIeCard(settings),
        };
        for model in [
            MacModel::LC,
            MacModel::LCII,
            MacModel::LCIII,
            MacModel::LC475,
        ] {
            // The card is only added when the machine is made
            let mut emu = machine(model);
            assert!(emu.add_device(&card).is_err());
            emu.add_device_to(&card, true).unwrap();

            // The machine reports it with its settings, and can't lose it
            assert_eq!(emu.devices(), vec![card.clone()]);
            assert!(emu.remove_device(bus_id::PDS_LC, "0").is_err());
            // It came with the drives its settings name, on the bus it adds: the last one
            let buses = emu.buses();
            let disk = buses.last().unwrap();
            assert_eq!(disk.id, bus_id::IIE_DISK);
            let drives = disk.slots.iter().map(|slot| {
                let device = slot.device.as_ref().unwrap();
                (slot.id.as_str(), device.model, device.name.as_deref())
            });
            assert!(drives.eq([
                ("1", DeviceModel::UniDisk35, Some("Apple IIe UniDisk 3.5")),
                (
                    "2",
                    DeviceModel::Apple525Drive,
                    Some("Apple IIe 5.25\" drive 2")
                ),
            ]));
        }

        // It fits no other slot
        let mut se30 = machine(MacModel::SE30);
        let wrong = DeviceConfig {
            bus: bus_id::PDS_SE30.into(),
            ..card
        };
        assert!(se30.add_device_to(&wrong, true).is_err());
    }

    #[test]
    fn the_drives_of_the_apple_iie_card_are_on_its_disk_port() {
        use crate::mac::maclc::pds_iie::{IieCardSettings, IieDiskDevice};

        let settings = IieCardSettings {
            security_jumper: false,
            disk_chain: [IieDiskDevice::UniDisk35, IieDiskDevice::Apple525],
        };
        let card = DeviceConfig {
            bus: bus_id::PDS_LC.into(),
            slot: "0".into(),
            model: Some(DeviceModel::AppleIIePdsCard),
            rom: None,
            function: FunctionConfig::AppleIIeCard(settings),
        };
        for model in [MacModel::LC, MacModel::LCIII, MacModel::LC475] {
            let mut emu = make(model, vec![card.clone()]).unwrap();

            // A drive of the card is reached as the Mac's are
            let floppy = emu.floppy_in(bus_id::IIE_DISK, "2").unwrap();
            assert!(floppy.status().ejected);

            // The machine answers for its own buses only: the emulator reaches the card's
            // through the card
            assert!(emu.config.bus_ops(bus_id::IIE_DISK).is_none());
            assert!(emu.bus_ops(bus_id::IIE_DISK).is_some());

            // Without the card, the machine has no such bus
            assert!(machine(model).bus_ops(bus_id::IIE_DISK).is_none());
        }
    }

    /// The device models of the floppy drives of a machine, in the order of their slots
    fn floppy_drives(emu: &mut Emulator) -> Vec<DeviceModel> {
        let buses = emu.buses();
        let bus = buses.iter().find(|bus| bus.id == bus_id::SWIM).unwrap();
        bus.slots
            .iter()
            .map(|slot| slot.device.as_ref().unwrap().model)
            .collect()
    }

    #[test]
    fn every_mac_model_has_its_floppy_drives() {
        use DeviceModel::{Mac400KDrive, Mac800KDrive, MacSuperDrive};
        use strum::IntoEnumIterator;

        for model in MacModel::iter() {
            // The type and the number of drives of each Mac model, as they were before the
            // drives became devices
            let (drive, count) = match model {
                MacModel::Early128K | MacModel::Early512K => (Mac400KDrive, 2),
                MacModel::Early512Ke | MacModel::Plus | MacModel::MacII => (Mac800KDrive, 2),
                MacModel::SE => (Mac800KDrive, 3),
                MacModel::SeFdhd | MacModel::Portable | MacModel::Portable15MB => {
                    (MacSuperDrive, 3)
                }
                MacModel::Classic
                | MacModel::MacIIFDHD
                | MacModel::MacIIx
                | MacModel::MacIIcx
                | MacModel::SE30
                | MacModel::LC
                | MacModel::LCII
                | MacModel::Quadra700 => (MacSuperDrive, 2),
                MacModel::LCIII | MacModel::LC475 | MacModel::Quadra800 => (MacSuperDrive, 1),
            };
            let mut emu = machine(model);
            assert_eq!(floppy_drives(&mut emu), vec![drive; count], "{model}");
        }
    }

    #[test]
    fn drives_of_another_type_take_the_place_of_those_of_the_mac_model() {
        let drive = |slot: &str| DeviceConfig {
            bus: bus_id::SWIM.into(),
            slot: slot.into(),
            model: Some(DeviceModel::Mac800KPwmDrive),
            rom: None,
            function: FunctionConfig::Floppy,
        };
        let mut emu = make(MacModel::Early128K, vec![drive("1"), drive("2")]).unwrap();
        assert_eq!(
            floppy_drives(&mut emu),
            vec![DeviceModel::Mac800KPwmDrive; 2]
        );

        // They are in the list the machine can be made from again
        let configs = emu.devices();
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[1].bus, bus_id::SWIM);
        assert_eq!(configs[1].slot, "2");
        assert_eq!(configs[1].model, Some(DeviceModel::Mac800KPwmDrive));
    }

    #[test]
    fn a_floppy_is_loaded_and_ejected_by_slot() {
        use snow_floppy::loaders::{FloppyImageSaver, Moof};
        use snow_floppy::{Floppy, FloppyImage, FloppyType};

        let dir = TestDir::new("floppy");
        let path = dir.0.join("disk.moof");
        let blank = FloppyImage::new(FloppyType::Mac800K, "disk");
        Moof::save_file(&blank, &path.to_string_lossy()).unwrap();
        let mut emu = plus();
        assert_eq!(emu.slot_label(bus_id::SWIM, "1"), "Floppy #1");
        assert_eq!(emu.slot_label(bus_id::SCSI, "3"), "SCSI ID #3");

        emu.load_media(bus_id::SWIM, "1", &path, true).unwrap();
        let floppy = emu.floppy_in(bus_id::SWIM, "1").unwrap();
        assert!(!floppy.status().ejected);
        assert_eq!(floppy.status().title, "disk");
        // A Plus has no drive on its third connector
        assert!(emu.load_media(bus_id::SWIM, "3", &path, true).is_err());
        assert!(emu.floppy_in(bus_id::SCSI, "0").is_err());

        // The frontend gets the image of the ejected disk, with the ids of its slot
        emu.status_update().unwrap();
        emu.event_recv.try_iter().count();
        emu.eject_media(bus_id::SWIM, "1").unwrap();
        emu.status_update().unwrap();
        let mut ejected = vec![];
        for event in emu.event_recv.try_iter() {
            if let EmulatorEvent::FloppyEjected(bus, slot, image) = event {
                ejected.push((bus, slot, image.get_title().to_string()));
            }
        }
        assert_eq!(
            ejected,
            [("SWIM".to_string(), "1".to_string(), "disk".to_string())]
        );
        assert!(emu.floppy_in(bus_id::SWIM, "1").unwrap().status().ejected);
    }

    #[test]
    fn a_floppy_drive_is_only_added_when_the_machine_is_made() {
        let mut emu = machine(MacModel::LCIII);
        let drive = |slot: &str| DeviceConfig {
            bus: bus_id::SWIM.into(),
            slot: slot.into(),
            model: Some(DeviceModel::MacSuperDrive),
            rom: None,
            function: FunctionConfig::Floppy,
        };
        // The LC III has one drive, and a second connector
        let e = emu.add_device(&drive("2")).unwrap_err();
        assert_eq!(
            e.to_string(),
            "SuperDrive can't be added once the machine is made"
        );
        assert!(emu.remove_device(bus_id::SWIM, "1").is_err());
        assert_eq!(floppy_drives(&mut emu), [DeviceModel::MacSuperDrive]);
        // The drive that is there takes a device config that changes nothing, and stays
        assert!(emu.add_device(&drive("1")).is_err());
        emu.configure_device(&drive("1")).unwrap();
    }

    #[test]
    fn a_status_is_sent_when_it_changes() {
        let dir = TestDir::new("status");
        let image = dir.file("disc.iso", 4096);
        let mut emu = plus();
        // A new machine sends its list, and no status of the SCSI bus: its slots are empty
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
        emu.load_media(bus_id::SCSI, "3", &image, false).unwrap();
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
        let bus = buses.iter().find(|bus| bus.id == bus_id::SCSI).unwrap();
        let slot = |id: &str| bus.slots.iter().find(|s| s.id == id).unwrap();
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
            device_in(&mut emu, bus_id::SCSI, "4"),
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
        let device = device_in(&mut emu, bus_id::SCSI, "6").unwrap();
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

    #[cfg(feature = "printer")]
    #[test]
    fn a_printer_given_no_folder_uses_the_default_folder() {
        fn folder(emu: &mut Emulator, slot: &str) -> PathBuf {
            let FunctionStatus::Printer(printer) = status(emu, slot) else {
                panic!("A printer has the Printer function");
            };
            printer.output_dir
        }
        let printer = |slot| {
            scsi(
                slot,
                Some(DeviceModel::LaserWriterIISC),
                FunctionConfig::Printer(PrinterConfig::default()),
            )
        };

        // A printer that is there when the folder is given, and one added after
        let mut emu = plus();
        emu.add_device(&printer("5")).unwrap();
        emu.attach_printer(4, PathBuf::from("pages")).unwrap();
        assert_eq!(folder(&mut emu, "5"), std::env::current_dir().unwrap());
        emu.set_default_print_dir(PathBuf::from("desktop"));
        emu.add_device(&printer("6")).unwrap();
        assert_eq!(folder(&mut emu, "5"), PathBuf::from("desktop"));
        assert_eq!(folder(&mut emu, "6"), PathBuf::from("desktop"));
        // A printer that was given a folder keeps it
        assert_eq!(folder(&mut emu, "4"), PathBuf::from("pages"));
        // The list the machine can be made from again has what the frontend gave
        assert!(emu.devices().contains(&printer("5")));
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
        let mut emu = machine(MacModel::Early512K);
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
