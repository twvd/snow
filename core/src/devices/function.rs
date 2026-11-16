//! The functions of the devices
//!
//! A function is what a device does for the user, such as HardDisk or Ethernet. It is a trait
//! whose methods act on the device, and it comes with a Status struct: what the frontend shows
//! of the device while the machine runs. The emulator calls the methods of a function to run
//! the commands of the frontend and to report the statuses.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::emulator::comm::EthernetCaptureStatus;
use crate::mac::scsi::disk_image::DiskImage;
use crate::renderer::AudioProvider;

pub trait HardDiskFunction {
    /// The live state of the disk. Called at each status update.
    fn status(&self) -> HardDiskStatus;
    /// Carries on from a copy of the image, in the file `path`. Called when the user branches
    /// the disk.
    fn branch(&mut self, path: &Path) -> Result<()>;
    /// Gives the disk its storage, for a frontend that keeps its images itself. Refused if the
    /// disk has one. Called once, after the disk is made with no image file, and by the
    /// emulator when it restores a save state.
    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()>;
    /// The disk's image, as it is now, which a save state holds after the machine's state.
    /// Called by the emulator when it saves a state, for every device whose function is
    /// HardDisk. None if the disk has no storage. When the state is restored, the emulator
    /// writes the image to a temporary file and gives it to the disk with `load_image()`.
    fn savestate_image(&self) -> Option<&[u8]>;
    /// Replaces the disk's image with the file `path`, on a drive that can't be removed while
    /// the machine runs, such as an HD20. Called when the user picks an image for such a drive.
    /// Refused by default: a disk that can be removed is detached and attached instead.
    fn load(&mut self, _path: &Path) -> Result<()> {
        bail!("The image of this disk can't be replaced")
    }
}

pub trait CdromFunction {
    /// The live state of the drive. Called at each status update.
    fn status(&self) -> CdromStatus;
    /// Puts the disc of the image file `path` in the drive. Called when the user picks an image.
    fn load(&mut self, path: &Path) -> Result<()>;
    /// Puts the disc of an image that isn't a file in the drive, for a frontend that keeps its
    /// images itself
    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()>;
    /// Gives the drive a folder of images, which the guest swaps, and puts the first one in the
    /// drive. Called when the user picks a folder.
    fn set_folder(&mut self, folder: &Path) -> Result<()>;
    /// Called by the emulator to give the drive the host's sound output, for audio CDs
    fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()>;
}

pub trait EthernetFunction {
    /// The live state of the adapter. Called at each status update.
    fn status(&self) -> EthernetStatus;
    /// Connects the adapter to something else on the host. Called when the user picks a link.
    fn set_link(&mut self, link: EthernetLinkType) -> Result<()>;
    // Packet capture to a pcap file. Legacy interfaces: will be removed when Ethernet support
    // is split out of the DaynaPORT.
    fn start_capture(&mut self, filename: &Path) -> Result<()>;
    fn stop_capture(&mut self) -> Option<(PathBuf, usize)>;
}

pub trait PrinterFunction {
    /// The live state of the printer. Called at each status update.
    fn status(&self) -> PrinterStatus;
}

/// A device, seen through its function
pub enum FunctionMut<'a> {
    HardDisk(&'a mut dyn HardDiskFunction),
    Cdrom(&'a mut dyn CdromFunction),
    Ethernet(&'a mut dyn EthernetFunction),
    Printer(&'a mut dyn PrinterFunction),
}

/// The live state of a device, for the frontend to show
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionStatus {
    HardDisk(HardDiskStatus),
    Cdrom(CdromStatus),
    Ethernet(EthernetStatus),
    Printer(PrinterStatus),
}

impl FunctionMut<'_> {
    pub fn status(&self) -> FunctionStatus {
        match self {
            Self::HardDisk(disk) => FunctionStatus::HardDisk(disk.status()),
            Self::Cdrom(cdrom) => FunctionStatus::Cdrom(cdrom.status()),
            Self::Ethernet(ethernet) => FunctionStatus::Ethernet(ethernet.status()),
            Self::Printer(printer) => FunctionStatus::Printer(printer.status()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardDiskStatus {
    /// The image file. None: the image isn't a file.
    pub media: Option<PathBuf>,
    /// In bytes. 0: the disk has no image yet.
    pub capacity: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdromStatus {
    /// The image of the disc in the drive. None: no disc.
    pub image: Option<PathBuf>,
    /// Its folder of disc images, if it has one
    pub folder: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthernetStatus {
    /// The adapter's Ethernet address
    pub mac: [u8; 6],
    /// What it is connected to on the host
    pub link: EthernetLinkType,
    /// A capture of its packets to a file, if any
    pub capture: Option<EthernetCaptureStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterStatus {
    /// The folder its pages go to
    pub output_dir: PathBuf,
}

/// What an Ethernet adapter is connected to on the host
///
/// Every build has every link, so that a file written by one build can be read by another. A
/// link that a build can't start is refused by `EthernetFunction::set_link()`.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum EthernetLinkType {
    /// No link
    Down,
    /// Userland NAT
    NAT,
    /// Userland NAT with HTTPS stripping
    NATHttpsStripping,
    /// Raw sockets based bridge
    Bridge(u32),
    /// Tap interface based bridge
    TapBridge(String),
}

// Clippy is wrong, I don't think you can conditionally tag #[default] on an enum based on features
#[allow(clippy::derivable_impls)]
impl Default for EthernetLinkType {
    fn default() -> Self {
        #[cfg(feature = "ethernet_nat")]
        {
            Self::NAT
        }
        #[cfg(not(feature = "ethernet_nat"))]
        {
            Self::Down
        }
    }
}
