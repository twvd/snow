//! The machine's devices as the frontend sees them
//!
//! A machine exposes lists of ports, one list per family (the floppy drives, the SCSI IDs, ...).
//! A port is fixed, with a device the machine put there, or hotswap, empty for the user to fill.
//! The frontend knows functions, like a hard disk or a floppy, and shows the family as text. It
//! doesn't know what sits behind a port: only this file does.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use snow_floppy::Floppy;

use super::Emulator;
use crate::mac::MacModel;
#[cfg(feature = "ethernet")]
use crate::mac::scsi::ethernet::EthernetLinkType;
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::swim::MAX_DCDS;
use crate::mac::swim::dcd::{DCD_BLOCKSIZE, DcdDevice};

#[cfg(feature = "ethernet")]
use super::comm::EthernetCaptureStatus;

/// Where a port is. The lists keep their shape for the life of a machine, so this stays valid.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortAddr {
    pub list: usize,
    pub port: usize,
}

#[derive(Debug, Clone)]
pub struct PortList {
    /// Shown to the user. With a port's ID, it also identifies the port in a workspace.
    pub family: String,
    pub ports: Vec<DevicePort>,
    /// Most users of this machine don't want this in their menus. It still shows when it has
    /// a device in it, or if the user wants it to.
    pub hidden_by_default: bool,
}

#[derive(Debug, Clone)]
pub struct DevicePort {
    /// SCSI ID, floppy number, ...
    pub id: String,
    pub name: Option<String>,
    /// The device can be removed
    pub hotswap: bool,
    /// What can go in this port
    pub offers: Vec<PortOffer>,
    /// Those of the offers that are unavailable right now
    pub blocked: Vec<PortOffer>,
    /// Something the user should know before using this port
    pub hint: Option<String>,
    /// Hard disk images can be of a volume, without a partition map
    pub takes_volume_images: bool,
    pub device: Option<Device>,
}

impl DevicePort {
    /// How the user knows this port: "SCSI #3"
    pub fn label(&self, family: &str) -> String {
        match &self.name {
            Some(name) => format!("{} #{} {}", family, self.id, name),
            None => format!("{} #{}", family, self.id),
        }
    }

    /// Whether this can go in the port right now
    pub fn can_take(&self, offer: PortOffer) -> bool {
        self.offers.contains(&offer) && !self.blocked.contains(&offer)
    }
}

/// Something the frontend can offer to put in a port
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortOffer {
    /// Creating a hard disk image and attaching it
    NewHardDisk,
    /// Attaching a hard disk image
    HardDisk,
    /// Attaching a hard disk image the guest can't change
    ReadOnlyHardDisk,
    CdromDrive,
    /// Replacing whatever CD-ROM drive is there by a folder of images
    CdromFolder,
    #[cfg(feature = "ethernet")]
    Ethernet,
    #[cfg(feature = "printer")]
    Printer,
}

/// A device to put in a port
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NewDevice {
    HardDisk(PathBuf),
    ReadOnlyHardDisk(PathBuf),
    CdromDrive,
    CdromFolder(PathBuf),
    #[cfg(feature = "ethernet")]
    Ethernet,
    /// Parameter: output directory
    #[cfg(feature = "printer")]
    Printer(PathBuf),
}

impl NewDevice {
    const fn offer(&self) -> PortOffer {
        match self {
            Self::HardDisk(_) => PortOffer::HardDisk,
            Self::ReadOnlyHardDisk(_) => PortOffer::ReadOnlyHardDisk,
            Self::CdromDrive => PortOffer::CdromDrive,
            Self::CdromFolder(_) => PortOffer::CdromFolder,
            #[cfg(feature = "ethernet")]
            Self::Ethernet => PortOffer::Ethernet,
            #[cfg(feature = "printer")]
            Self::Printer(_) => PortOffer::Printer,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Device {
    Floppy(FloppyPort),
    Disk(DiskPort),
    #[cfg(feature = "ethernet")]
    Ethernet(EthernetPort),
    #[cfg(feature = "printer")]
    Printer,
}

#[derive(Debug, Clone)]
pub struct FloppyPort {
    pub ejected: bool,
    pub motor: bool,
    pub writing: bool,
    pub track: usize,
    pub image_title: String,
    pub dirty: bool,
    pub drive_type: crate::mac::swim::drive::DriveType,
    /// True if the loaded image's source format supports writeback and was
    /// loaded from a file on disk (i.e. writeback is possible).
    pub writeback_supported: bool,
    /// True if writeback is currently armed for this drive.
    pub writeback_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskKind {
    HardDisk,
    Cdrom,
}

#[derive(Debug, Clone)]
pub struct DiskPort {
    pub kind: DiskKind,
    pub image: Option<PathBuf>,
    /// Toolbox-managed image folder for a folder-backed CD-ROM, if any.
    pub folder: Option<PathBuf>,
    pub capacity: Option<usize>,
    /// The image can be branched off to another file
    pub can_branch: bool,
    pub writable: bool,
}

#[cfg(feature = "ethernet")]
#[derive(Debug, Clone)]
pub struct EthernetPort {
    pub link_type: EthernetLinkType,
    pub capture_status: Option<EthernetCaptureStatus>,
}

/// What is behind a port
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PortTarget {
    Floppy(usize),
    Scsi(usize),
    Hd20(usize),
}

impl Emulator {
    fn port_layout(&self) -> Vec<(&'static str, Vec<PortTarget>)> {
        let mut lists = vec![];
        let floppies: Vec<_> = (0..self.config.swim().drives.len())
            .filter(|&i| self.config.swim().drives[i].is_present())
            .map(PortTarget::Floppy)
            .collect();
        if !floppies.is_empty() {
            lists.push(("Floppy", floppies));
        }
        if self.model.has_scsi() {
            lists.push(("SCSI", (0..7).map(PortTarget::Scsi).collect()));
        }
        // Behind the floppy drives on the same connector, but not in their way: a chain of up
        // to four, and the external drive answers after the last one.
        if self.config.swim().drives.iter().any(|d| d.is_present()) {
            lists.push(("HD20", (0..MAX_DCDS).map(PortTarget::Hd20).collect()));
        }
        lists
    }

    pub fn port_lists(&self) -> Vec<PortList> {
        self.port_layout()
            .into_iter()
            .map(|(family, targets)| PortList {
                family: family.to_owned(),
                ports: targets.into_iter().map(|t| self.device_port(t)).collect(),
                hidden_by_default: family == "HD20"
                    && !matches!(
                        self.model,
                        MacModel::Early128K
                            | MacModel::Early512K
                            | MacModel::Early512Ke
                            | MacModel::Plus
                    ),
            })
            .collect()
    }

    /// Finds a port by what a workspace knows of it
    pub fn find_port(&self, family: &str, id: &str) -> Option<PortAddr> {
        let lists = self.port_lists();
        let list = lists.iter().position(|l| l.family == family)?;
        let port = lists[list].ports.iter().position(|p| p.id == id)?;
        Some(PortAddr { list, port })
    }

    /// The Nth floppy drive
    pub fn floppy_addr(&self, n: usize) -> Option<PortAddr> {
        self.addr_of(PortTarget::Floppy(n))
    }

    pub(super) fn addr_of(&self, target: PortTarget) -> Option<PortAddr> {
        self.port_layout()
            .iter()
            .enumerate()
            .find_map(|(list, (_, targets))| {
                let port = targets.iter().position(|t| *t == target)?;
                Some(PortAddr { list, port })
            })
    }

    fn port_target(&self, addr: PortAddr) -> Result<PortTarget> {
        self.port_layout()
            .get(addr.list)
            .and_then(|(_, targets)| targets.get(addr.port).copied())
            .with_context(|| format!("No port at {:?}", addr))
    }

    /// The floppy drive of a port
    pub(super) fn floppy_drive(&self, addr: PortAddr) -> Result<usize> {
        match self.port_target(addr)? {
            PortTarget::Floppy(drive) => Ok(drive),
            t => bail!("{:?} is not a floppy drive", t),
        }
    }

    /// The SCSI ID of a port
    pub(super) fn scsi_id(&self, addr: PortAddr) -> Result<usize> {
        match self.port_target(addr)? {
            PortTarget::Scsi(id) => Ok(id),
            t => bail!("{:?} is not on SCSI", t),
        }
    }

    fn device_port(&self, target: PortTarget) -> DevicePort {
        match target {
            PortTarget::Floppy(i) => DevicePort {
                id: (i + 1).to_string(),
                name: None,
                hotswap: false,
                offers: vec![],
                blocked: vec![],
                hint: None,
                takes_volume_images: false,
                device: Some(Device::Floppy(self.floppy_port(i))),
            },
            PortTarget::Scsi(i) => DevicePort {
                id: i.to_string(),
                name: None,
                hotswap: true,
                offers: self.scsi_offers(i),
                blocked: self.scsi_blocked(),
                hint: None,
                takes_volume_images: false,
                device: self.scsi_device(i),
            },
            PortTarget::Hd20(i) => DevicePort {
                id: (i + 1).to_string(),
                name: None,
                hotswap: true,
                offers: vec![
                    PortOffer::NewHardDisk,
                    PortOffer::HardDisk,
                    PortOffer::ReadOnlyHardDisk,
                ],
                blocked: self.hd20_blocked(i),
                hint: Some(self.hd20_hint().to_owned()),
                takes_volume_images: true,
                device: self.hd20_device(i),
            },
        }
    }

    fn floppy_port(&self, i: usize) -> FloppyPort {
        let swim = self.config.swim();
        let drive = &swim.drives[i];
        FloppyPort {
            ejected: !drive.floppy_inserted,
            motor: drive.motor,
            writing: drive.motor && swim.is_writing(),
            track: drive.track,
            image_title: drive.floppy.get_title().to_owned(),
            dirty: drive.floppy.is_dirty(),
            drive_type: drive.drive_type,
            writeback_supported: drive.image_path.is_some() && drive.floppy.supports_writeback(),
            writeback_enabled: drive.writeback_enabled,
        }
    }

    fn scsi_offers(&self, id: usize) -> Vec<PortOffer> {
        let scsi = self.config.scsi();
        match scsi.get_target_type(id) {
            None => {
                #[allow(unused_mut)]
                let mut offers = vec![
                    PortOffer::NewHardDisk,
                    PortOffer::HardDisk,
                    PortOffer::CdromDrive,
                    PortOffer::CdromFolder,
                ];
                #[cfg(feature = "ethernet")]
                offers.push(PortOffer::Ethernet);
                #[cfg(feature = "printer")]
                offers.push(PortOffer::Printer);
                offers
            }
            Some(ScsiTargetType::Cdrom) => vec![PortOffer::CdromFolder],
            Some(_) => vec![],
        }
    }

    /// One of each on the bus: the frontend has offered no more than that so far.
    fn scsi_blocked(&self) -> Vec<PortOffer> {
        #[allow(unused_mut)]
        let mut blocked = vec![];
        #[cfg(feature = "ethernet")]
        if self.scsi_has(ScsiTargetType::Ethernet) {
            blocked.push(PortOffer::Ethernet);
        }
        #[cfg(feature = "printer")]
        if self.scsi_has(ScsiTargetType::Printer) {
            blocked.push(PortOffer::Printer);
        }
        blocked
    }

    #[cfg(any(feature = "ethernet", feature = "printer"))]
    fn scsi_has(&self, target_type: ScsiTargetType) -> bool {
        (0..7).any(|i| self.config.scsi().get_target_type(i) == Some(target_type))
    }

    fn scsi_device(&self, id: usize) -> Option<Device> {
        let scsi = self.config.scsi();
        let image = || scsi.get_disk_imagefn(id).map(Path::to_path_buf);
        Some(match scsi.get_target_type(id)? {
            ScsiTargetType::Disk => Device::Disk(DiskPort {
                kind: DiskKind::HardDisk,
                image: image(),
                folder: None,
                capacity: scsi.get_disk_capacity(id),
                can_branch: true,
                writable: true,
            }),
            ScsiTargetType::Cdrom => Device::Disk(DiskPort {
                kind: DiskKind::Cdrom,
                image: image(),
                folder: scsi.get_cdrom_folder(id).map(Path::to_path_buf),
                capacity: scsi.get_disk_capacity(id),
                can_branch: false,
                writable: false,
            }),
            #[cfg(feature = "ethernet")]
            ScsiTargetType::Ethernet => {
                let target = scsi.targets[id].as_ref()?;
                Device::Ethernet(EthernetPort {
                    link_type: target.eth_link()?,
                    capture_status: target.eth_capture_status(),
                })
            }
            #[cfg(feature = "printer")]
            ScsiTargetType::Printer => Device::Printer,
        })
    }

    /// A chain has no gaps: only the next free place takes a disk.
    fn hd20_blocked(&self, i: usize) -> Vec<PortOffer> {
        if i == self.config.swim().dcds.len() {
            vec![]
        } else {
            vec![
                PortOffer::NewHardDisk,
                PortOffer::HardDisk,
                PortOffer::ReadOnlyHardDisk,
            ]
        }
    }

    /// Which ROMs have the driver is in docs/HD20.md. Only the ones known not to are named.
    fn hd20_hint(&self) -> &'static str {
        match self.model {
            MacModel::Early128K => "This Mac doesn't have enough RAM to load the HD20 INIT.",
            MacModel::Early512K => {
                "This Mac needs the HD20 INIT from a boot floppy. It looks for HD20s when it \
                 starts up."
            }
            MacModel::MacII
            | MacModel::MacIIFDHD
            | MacModel::MacIIx
            | MacModel::MacIIcx
            | MacModel::SE30 => {
                "This Mac's ROM has no HD20 driver, it needs Apple's MacIIHD20 INIT. It looks \
                 for HD20s when it starts up."
            }
            _ => "The Mac looks for HD20s when it starts up.",
        }
    }

    fn hd20_device(&self, i: usize) -> Option<Device> {
        let dcd = self.config.swim().dcds.get(i)?;
        Some(Device::Disk(DiskPort {
            kind: DiskKind::HardDisk,
            image: dcd.image_path().map(Path::to_path_buf),
            folder: None,
            capacity: Some(dcd.blocks() * DCD_BLOCKSIZE),
            can_branch: false,
            writable: dcd.writable(),
        }))
    }

    /// Attaches an HD20 at the end of the chain
    fn attach_hd20(&mut self, path: &Path, writable: bool) -> Result<()> {
        let dcd = DcdDevice::open(path, writable)?;
        self.config.swim_mut().attach_dcd(dcd)?;
        Ok(())
    }

    /// Puts a device in a port
    pub fn add_device(&mut self, addr: PortAddr, device: NewDevice) -> Result<()> {
        let target = self.port_target(addr)?;
        if !self.device_port(target).can_take(device.offer()) {
            bail!("This port does not take this device: {:?}", device.offer());
        }
        match (target, device) {
            (PortTarget::Scsi(id), NewDevice::HardDisk(path)) => self.load_hdd_image(&path, id),
            (PortTarget::Scsi(id), NewDevice::CdromDrive) => {
                self.attach_cdrom(id);
                Ok(())
            }
            (PortTarget::Scsi(id), NewDevice::CdromFolder(dir)) => {
                self.attach_cdrom_folder(id, dir)
            }
            #[cfg(feature = "ethernet")]
            (PortTarget::Scsi(id), NewDevice::Ethernet) => {
                self.attach_ethernet(id);
                Ok(())
            }
            #[cfg(feature = "printer")]
            (PortTarget::Scsi(id), NewDevice::Printer(output_dir)) => {
                self.attach_printer(id, output_dir);
                Ok(())
            }
            (PortTarget::Scsi(_), NewDevice::ReadOnlyHardDisk(_)) => {
                unreachable!("SCSI ports don't offer this")
            }
            (PortTarget::Hd20(_), NewDevice::HardDisk(path)) => self.attach_hd20(&path, true),
            (PortTarget::Hd20(_), NewDevice::ReadOnlyHardDisk(path)) => {
                self.attach_hd20(&path, false)
            }
            (PortTarget::Hd20(_), _) => unreachable!("HD20 ports offer hard disks only"),
            (PortTarget::Floppy(_), _) => unreachable!("floppy ports offer nothing"),
        }
    }

    /// Takes a device out of a port
    pub(super) fn remove_device(&mut self, addr: PortAddr) -> Result<()> {
        let target = self.port_target(addr)?;
        if !self.device_port(target).hotswap {
            bail!("The device in this port cannot be removed");
        }
        match target {
            PortTarget::Scsi(id) => self.config.scsi_mut().detach_target(id),
            PortTarget::Hd20(i) => {
                self.config.swim_mut().detach_dcd(i)?;
            }
            PortTarget::Floppy(_) => unreachable!("floppy ports are fixed"),
        }
        Ok(())
    }
}
