//! The functions of the devices
//!
//! A function is what a device does for the user, such as HardDisk or Ethernet. It is a trait
//! whose methods act on the device, and it comes with a Status struct: what the frontend shows
//! of the device while the machine runs. The emulator calls the methods of a function to run
//! the commands of the frontend and to report the statuses.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use snow_floppy::{FloppyImage, FloppyType};

use crate::emulator::comm::EthernetCaptureStatus;
use crate::mac::MacMonitor;
use crate::mac::maclc::pds_iie::{GamePort, IieCardSettings};
use crate::mac::scsi::disk_image::DiskImage;
use crate::renderer::{AudioProvider, FrameReceiver};

pub trait FloppyFunction {
    /// The live state of the drive. Called at each status update.
    fn status(&self) -> FloppyStatus;
    /// Puts the disk of the image file `path` in the drive. Called when the user picks an
    /// image.
    fn load(&mut self, path: &Path, read_only: bool) -> Result<()>;
    /// Ejects the disk. Called when the user ejects it from the menu.
    fn eject(&mut self);
    /// Puts the disk of an image already in memory in the drive, such as a blank disk. Called
    /// when the user picks one. Refused by default.
    fn insert_image(&mut self, _image: Box<FloppyImage>, _read_only: bool) -> Result<()> {
        bail!("This drive takes no image from memory")
    }
    /// Saves the disk to the image file `path`. Called when the user saves the disk. Refused
    /// by default.
    fn save(&mut self, _path: &Path) -> Result<()> {
        bail!("This drive can't save its disk")
    }
    /// Turns write-back on or off: with it, the writes of the guest go to the file the disk
    /// came from. Called when the user changes it. Refused by default.
    fn set_writeback(&mut self, _enabled: bool) -> Result<()> {
        bail!("This drive can't write its disk back")
    }
    /// Writes the disk back to the file it came from, if write-back is on and the drive has
    /// stopped since the last write; with `force`, even while the disk turns. A disk that was
    /// ejected with unsaved writes is written back too. Called by the emulator after each
    /// batch of instructions, and with `force` before a menu eject and when it quits. Nothing
    /// by default.
    fn write_back(&mut self, _force: bool) -> Result<()> {
        Ok(())
    }
    /// Takes the image of the last disk ejected, which the frontend can put back or save.
    /// Called at each status update. None by default.
    fn take_ejected_image(&mut self) -> Option<Box<FloppyImage>> {
        None
    }
    /// Makes the disk turn faster or slower, by `rpm` turns per minute. Called when the user
    /// changes the speed adjustment. Refused by default.
    fn set_rpm_adjustment(&mut self, _rpm: i32) -> Result<()> {
        bail!("The speed of this drive can't be adjusted")
    }
    /// Seeds the random bits of the drive, for runs that must repeat. None: random again.
    /// Called on every floppy drive when the frontend sets the seed. Nothing by default.
    fn seed_weak_bits(&mut self, _seed: Option<u64>) {}
}

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
    /// Gives the adapter the machine's NAT gateway, which every adapter on a NAT link shares.
    /// Called when the machine is made or restored, and when the adapter is added.
    #[cfg(feature = "ethernet")]
    fn set_shared_nat(&mut self, nat: crate::net::SharedNat);
    // Packet capture to a pcap file. Legacy interfaces: will be removed when Ethernet support
    // is split out of the DaynaPORT.
    fn start_capture(&mut self, filename: &Path) -> Result<()>;
    fn stop_capture(&mut self) -> Option<(PathBuf, usize)>;
}

pub trait PrinterFunction {
    /// The live state of the printer. Called at each status update.
    fn status(&self) -> PrinterStatus;
    /// Gives the printer the folder for its pages when its Config names none. Called when the
    /// frontend gives that folder, and when a printer is added after it did.
    fn set_default_dir(&mut self, dir: &Path);
}

/// A video display: a video card, or the built-in video of a machine. It owns its renderer.
pub trait DisplayFunction {
    /// The live state of the display. Called at each status update.
    fn status(&self) -> DisplayStatus;
    /// Where the frontend picks up the frames of this display. Called by the emulator once,
    /// when the machine is made or loaded from a save state.
    fn frames(&self) -> FrameReceiver;
    /// Shows a blank screen. Called when the machine resets or stops.
    fn blank(&mut self) -> Result<()>;
}

/// What only the Apple IIe Card has
pub trait AppleIIeCardFunction {
    /// Whether the Apple IIe has the screen and the keyboard, as the event `IieInFront` reports
    /// it. Called by the emulator after each batch of instructions.
    fn in_front(&self) -> bool;
    /// Gives the card the state of the joystick. Called when the frontend sends it.
    fn set_game_port(&mut self, port: GamePort);
}

/// The function of a device model, with what a device of that function supports, for the
/// frontend to offer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionInfo {
    Floppy(FloppyInfo),
    HardDisk,
    Cdrom,
    Ethernet(EthernetInfo),
    Printer,
    Display(DisplayInfo),
    AppleIIeCard,
}

impl FunctionInfo {
    /// The Config of this function in which nothing was chosen: that of a device the machine
    /// has without being asked. Called when the list of buses of a Mac model is built.
    pub fn default_config(&self) -> FunctionConfig {
        match self {
            Self::Floppy(_) => FunctionConfig::Floppy,
            Self::HardDisk => FunctionConfig::HardDisk(HardDiskConfig::default()),
            Self::Cdrom => FunctionConfig::Cdrom(CdromConfig::default()),
            Self::Ethernet(_) => FunctionConfig::Ethernet(EthernetConfig {
                mac: None,
                link: EthernetLinkType::Down,
            }),
            Self::Printer => FunctionConfig::Printer(PrinterConfig::default()),
            Self::Display(_) => FunctionConfig::Display(DisplayConfig::default()),
            Self::AppleIIeCard => FunctionConfig::AppleIIeCard(IieCardSettings::default()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthernetInfo {
    /// The maker's prefix: the first three bytes of the Ethernet addresses of this device model
    pub oui: [u8; 3],
}

impl FunctionInfo {
    /// Whether a Config is the one of this function
    pub const fn takes(&self, config: &FunctionConfig) -> bool {
        matches!(
            (self, config),
            (Self::Floppy(_), FunctionConfig::Floppy)
                | (Self::HardDisk, FunctionConfig::HardDisk(_))
                | (Self::Cdrom, FunctionConfig::Cdrom(_))
                | (Self::Ethernet(_), FunctionConfig::Ethernet(_))
                | (Self::Printer, FunctionConfig::Printer(_))
                | (Self::Display(_), FunctionConfig::Display(_))
                | (Self::AppleIIeCard, FunctionConfig::AppleIIeCard(_))
        )
    }
}

/// What was chosen for a device, by function
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionConfig {
    Floppy,
    HardDisk(HardDiskConfig),
    Cdrom(CdromConfig),
    Ethernet(EthernetConfig),
    Printer(PrinterConfig),
    Display(DisplayConfig),
    AppleIIeCard(IieCardSettings),
}

/// A device, seen through its function
pub enum FunctionMut<'a> {
    Floppy(&'a mut dyn FloppyFunction),
    HardDisk(&'a mut dyn HardDiskFunction),
    Cdrom(&'a mut dyn CdromFunction),
    Ethernet(&'a mut dyn EthernetFunction),
    Printer(&'a mut dyn PrinterFunction),
    Display(&'a mut dyn DisplayFunction),
    AppleIIeCard(&'a mut dyn AppleIIeCardFunction),
}

/// The live state of a device, for the frontend to show
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionStatus {
    Floppy(FloppyStatus),
    HardDisk(HardDiskStatus),
    Cdrom(CdromStatus),
    Ethernet(EthernetStatus),
    Printer(PrinterStatus),
    Display(DisplayStatus),
    AppleIIeCard,
}

impl FunctionMut<'_> {
    pub fn status(&self) -> FunctionStatus {
        match self {
            Self::Floppy(floppy) => FunctionStatus::Floppy(floppy.status()),
            Self::HardDisk(disk) => FunctionStatus::HardDisk(disk.status()),
            Self::Cdrom(cdrom) => FunctionStatus::Cdrom(cdrom.status()),
            Self::Ethernet(ethernet) => FunctionStatus::Ethernet(ethernet.status()),
            Self::Printer(printer) => FunctionStatus::Printer(printer.status()),
            Self::Display(display) => FunctionStatus::Display(display.status()),
            Self::AppleIIeCard(_) => FunctionStatus::AppleIIeCard,
        }
    }
}

/// What a floppy drive's device model accepts and can do
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloppyInfo {
    /// The image files it accepts
    pub images: FloppyImages,
    /// The blank disks it can be given
    pub blank_formats: &'static [FloppyType],
    /// The guest ejects its disks. False: the user does.
    pub guest_ejects: bool,
    /// Its speed can be changed
    pub rpm_control: bool,
    /// It writes to its image
    pub writable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloppyImages {
    /// The Mac's floppy images
    Mac,
    /// Image files of another kind: what the file dialog calls them, and their extensions
    Files {
        label: &'static str,
        extensions: &'static [&'static str],
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloppyStatus {
    /// The title of the disk in the drive
    pub title: String,
    /// No disk is in the drive
    pub ejected: bool,
    /// The disk has writes that aren't saved to a file
    pub dirty: bool,
    /// The disk can be written back to the file it came from
    pub writeback_supported: bool,
    /// Its writes go to that file
    pub writeback_enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardDiskConfig {
    /// The image file. None: given later, by `load_image()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardDiskStatus {
    /// The image file. None: the image isn't a file.
    pub media: Option<PathBuf>,
    /// In bytes. 0: the disk has no image yet.
    pub capacity: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CdromConfig {
    /// A folder of disc images the guest switches between
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdromStatus {
    /// The image of the disc in the drive. None: no disc.
    pub image: Option<PathBuf>,
    /// Its folder of disc images, if it has one
    pub folder: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EthernetConfig {
    /// The adapter's Ethernet address. None: a random one, with the maker's prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<[u8; 6]>,
    /// What it is connected to on the host
    pub link: EthernetLinkType,
}

/// The network of the machine's NAT gateway, and its DNS server
///
/// The adapters on a NAT link get the addresses after the gateway's. The gateway passes the DNS
/// queries sent to it on to the DNS server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NatConfig {
    /// The gateway's address
    pub gateway: [u8; 4],
    /// The length of the network prefix, in bits: 8 for a class A network, 16 for class B,
    /// 24 for class C
    pub prefix_len: u8,
    /// The DNS server that DNS queries to the gateway go to. None: the host's, the first one
    /// in /etc/resolv.conf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<std::net::IpAddr>,
}

impl Default for NatConfig {
    /// The gateway at 10.0.0.1, on 10.0.0.0/8, and the host's DNS server
    fn default() -> Self {
        Self {
            gateway: [10, 0, 0, 1],
            prefix_len: 8,
            dns: None,
        }
    }
}

impl NatConfig {
    /// The network mask
    pub fn netmask(&self) -> [u8; 4] {
        u32::MAX
            .checked_shl(32 - u32::from(self.prefix_len))
            .unwrap_or(0)
            .to_be_bytes()
    }

    /// Checks that it is a network with room for adapters: a prefix of 8 to 30 bits, and the
    /// gateway neither the network's own address nor its broadcast address
    pub fn check(&self) -> Result<()> {
        if !(8..=30).contains(&self.prefix_len) {
            bail!("The network prefix must be 8 to 30 bits long");
        }
        let mask = u32::from_be_bytes(self.netmask());
        let gateway = u32::from_be_bytes(self.gateway);
        if gateway & !mask == 0 || gateway & !mask == !mask {
            bail!("The gateway can't be the network's own address or its broadcast address");
        }
        Ok(())
    }
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterConfig {
    /// The folder its pages go to. None: the default folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterStatus {
    /// The folder its pages go to
    pub output_dir: PathBuf,
}

/// What a device model with the Display function supports
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayInfo {
    /// What can be on its connector, `MacMonitor::NoMonitor` included, ordered by size. Empty:
    /// it has no connector, the screen is the machine's own.
    pub monitors: &'static [MacMonitor],
    /// The index in `monitors` of the monitor it has when none is chosen
    pub default_monitor: usize,
    /// The VRAM sizes it can have, in bytes, from the smallest. One: no choice.
    pub vram_sizes: &'static [usize],
    /// The index in `vram_sizes` of the size it has when none is chosen
    pub default_vram_size: usize,
}

impl DisplayInfo {
    /// The monitor of a video for which none is chosen. None: it has no connector.
    pub fn default_monitor(&self) -> Option<MacMonitor> {
        self.monitors.get(self.default_monitor).copied()
    }

    /// The VRAM size of a video for which none is chosen. None: its VRAM has one size, which
    /// the device model doesn't list.
    pub fn default_vram_size(&self) -> Option<usize> {
        self.vram_sizes.get(self.default_vram_size).copied()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// One of `DisplayInfo::monitors`. None: the default monitor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<MacMonitor>,
    /// One of `DisplayInfo::vram_sizes`. None: the default size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_size: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayStatus {
    /// False: nothing is on its connector, so it shows no display
    pub connected: bool,
}

/// One display of a machine, for the frontend to show
pub struct DisplayHandle {
    /// In the menus: "Built-in display", "NuBus slot 9: Macintosh Display Card 8-24"
    pub name: String,
    /// The device that shows it, by the ids of its bus and slot
    pub bus: String,
    pub slot: String,
    /// Which of the device's displays: 0 unless it shows several
    pub index: usize,
    /// Where its frames arrive
    pub receiver: FrameReceiver,
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
