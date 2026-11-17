//! Communication between emulator and frontend

use serde::{Deserialize, Serialize};
use snow_floppy::FloppyImage;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::bus::Address;
use crate::cpu_m68k::bus::InspectView;
use crate::cpu_m68k::cpu::{HistoryEntry, SystrapHistoryEntry};
use crate::cpu_m68k::regs::{Register, RegisterFile};
use crate::debuggable::DebuggableProperties;
use crate::devices::{Bus, DeviceConfig, EthernetLinkType, FunctionStatus};
use crate::emulator::MouseMode;
use crate::keymap::KeyEvent;
use crate::mac::MacModel;
use crate::mac::maclc::pds_iie::GamePort;
use crate::mac::scc::SccCh;
use crate::mac::serial_bridge::{SerialBridgeConfig, SerialBridgeStatus};
use crate::tickable::Ticks;

pub use crate::cpu_m68k::cpu::{Breakpoint, BusBreakpoint};
use crate::renderer::AudioProvider;

pub type EmulatorCommandSender = crossbeam_channel::Sender<EmulatorCommand>;
pub type EmulatorEventReceiver = crossbeam_channel::Receiver<EmulatorEvent>;

pub type InputRecording = Vec<(Ticks, EmulatorCommand)>;

/// A command/event that can be sent to the emulator
#[derive(Serialize, Deserialize, Clone)]
pub enum EmulatorCommand {
    Quit,
    /// Sets the joystick and buttons on the Apple IIe card's game port.
    SetIieGamePort(GamePort),
    // The commands below name a slot by the id of its bus and the id of the slot
    /// Adds a device to the machine: see `Emulator::add_device()`
    AddDevice(DeviceConfig),
    /// Removes the device in a slot
    RemoveDevice(String, String),
    /// Loads the image in a file in the drive in a slot: a floppy drive, a CD-ROM drive, or a
    /// hard disk that takes another image while the machine runs. The last argument asks for
    /// the image to be write-protected, for a floppy drive that can write.
    LoadMedia(String, String, PathBuf, bool),
    /// Ejects the disk of the drive in a slot
    EjectMedia(String, String),
    /// Inserts a floppy image, passing the image as boxed object.
    /// Parameters: bus id, slot id, image, write-protect
    #[serde(skip)]
    InsertFloppyImage(String, String, Box<FloppyImage>, bool),
    /// Saves the disk of the floppy drive in a slot to this file
    SaveFloppy(String, String, PathBuf),
    /// Toggle automatic writeback for a floppy drive.
    /// Parameters: bus id, slot id, enabled
    SetFloppyWriteback(String, String, bool),
    /// Makes the disk of the floppy drive in a slot turn faster or slower, by this many RPM
    SetFloppyRpmAdjustment(String, String, i32),
    /// Makes the hard disk in a slot carry on from a copy of its image, in this file
    BranchDisk(String, String, PathBuf),
    /// Gives the CD-ROM drive in a slot a folder of disc images
    CdromSetFolder(String, String, PathBuf),
    EthernetSetLink(String, String, EthernetLinkType),
    EthernetStartCapture(String, String, PathBuf),
    EthernetStopCapture(String, String),
    MouseUpdateAbsolute {
        x: u16,
        y: u16,
    },
    MouseUpdateRelative {
        relx: i16,
        rely: i16,
        btn: Option<bool>,
    },
    Run,
    Stop,
    Reset,
    Step,
    StepOut,
    StepOver,
    ToggleBreakpoint(Breakpoint),
    /// Writes memory, at physical addresses if `phys`, else logical ones
    InspectWrite {
        addr: Address,
        data: Vec<u8>,
        phys: bool,
    },
    /// Sets or replaces the notifier with this id, see [`MemoryNotifierRange`]
    MemoryNotifierSet(u32, MemoryNotifierRange),
    MemoryNotifierRemove(u32),
    /// Reads the text of the guest's clipboard: answered by
    /// [`EmulatorEvent::ScrapText`]
    ReadScrapText,
    /// Looks for bytes from an address on: answered by
    /// [`EmulatorEvent::MemorySearchResult`]
    MemorySearch {
        start: Address,
        needle: Vec<u8>,
        view: InspectView,
    },
    /// Writes a range of memory to a file, with zeros for what can't be read
    MemoryExport {
        path: PathBuf,
        start: Address,
        len: usize,
        view: InspectView,
    },
    KeyEvent(KeyEvent),
    ReleaseAllInputs,
    CpuSetPC(u32),
    #[serde(skip)]
    SetSpeed(EmulatorSpeed),
    SetOverclock(Option<u64>),
    /// Makes a data cache flush also clear the instruction cache (68040)
    SetCacheFlushWorkaround(bool),
    /// Makes a user-mode PFLUSH flush the ATCs (68040)
    SetVmPflushWorkaround(bool),
    ProgKey,
    #[serde(skip)]
    WriteRegister(Register, u32),
    StartRecordingInput,
    EndRecordingInput,
    ReplayInputRecording(InputRecording, bool),
    SetInstructionHistory(bool),
    SetPeripheralDebug(bool),
    SccReceiveData(SccCh, Vec<u8>),
    SetSystrapHistory(bool),
    SetSharedDir(Option<PathBuf>),
    #[cfg(feature = "savestates")]
    SaveState(PathBuf, Option<Vec<u8>>),
    SetDebugFramebuffers(bool),
    /// Seeds the floppy noise (weak bits) RNG for deterministic behaviour.
    /// `None` reverts to non-deterministic randomness.
    SetFloppyWeakBitsSeed(Option<u64>),
    #[serde(skip)]
    SerialBridgeEnable(SccCh, SerialBridgeConfig),
    SerialBridgeDisable(SccCh),
    SetMouseMode(MouseMode),
    #[serde(skip)]
    ReplaceAudioProvider(Arc<Mutex<dyn AudioProvider>>),
}

/// Emulator speed tweak
#[derive(Debug, Copy, Clone, strum::Display, PartialEq, Serialize, Deserialize)]
pub enum EmulatorSpeed {
    /// Actual speed accurate to the real hardware
    Accurate,
    /// Actual speed when sound is played, otherwise uncapped
    Dynamic,
    /// Uncapped at all times, sound disabled
    Uncapped,
    /// Sync to 60 fps video, sound disabled
    Video,
    /// Fast-forward capped at a maximum speedup multiplier, sound disabled
    FastForward(f64),
}

/// Structure with general emulator status
#[derive(Debug)]
pub struct EmulatorStatus {
    pub regs: RegisterFile,
    pub running: bool,
    pub breakpoints: Vec<Breakpoint>,
    pub cycles: Ticks,

    pub model: MacModel,
    pub has_pmmu: bool,
    pub speed: EmulatorSpeed,
    /// Size of the RAM
    pub ram_size: usize,
    pub effective_speed: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EthernetCaptureStatus {
    pub active: bool,
    pub filename: Option<PathBuf>,
    pub packet_count: usize,
    pub error: Option<String>,
}

/// A friendly message ready for display to a user
#[derive(Debug)]
pub enum UserMessageType {
    Success,
    Notice,
    Warning,
    Error,
}

/// A range of memory that the frontend wants to be told about when what it holds
/// changes
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryNotifierRange {
    pub addr: Address,
    pub len: u32,
    pub view: InspectView,
    /// Starts at the PC, not at `addr`. Moves with the status updates.
    pub follow_pc: bool,
}

/// A status message/event received from the emulator
#[derive(strum::Display)]
pub enum EmulatorEvent {
    Status(Box<EmulatorStatus>),
    UserMessage(UserMessageType, String),
    /// The guest or the user ejected the disk of the floppy drive in a slot: its image
    FloppyEjected(String, String, Box<FloppyImage>),
    /// The buses of the machine, their slots and the devices in them. Sent when the machine is
    /// made, and again when a device is added, removed or gets another Config. The statuses
    /// received before it are those of the list before it.
    Buses(Arc<Vec<Bus>>),
    /// The status of the device in a slot, by the ids of its bus and of the slot, when it
    /// differs from the last one sent
    DeviceStatus(String, String, Option<FunctionStatus>),
    /// What a notifier's range holds, when it has changed. None for what can't
    /// be read.
    MemoryNotify {
        id: u32,
        addr: Address,
        data: Vec<Option<u8>>,
    },
    MemorySearchResult(Option<Address>),
    /// The text of the guest's clipboard, if it has any
    ScrapText(Option<String>),
    RecordedInput(InputRecording),
    InstructionHistory(Vec<HistoryEntry>),
    PeripheralDebug(DebuggableProperties),
    SccTransmitData(SccCh, Vec<u8>),
    SystrapHistory(Vec<SystrapHistoryEntry>),
    /// Serial bridge status update
    SerialBridgeStatus(SccCh, Option<SerialBridgeStatus>),
    /// The Apple IIe Card came to the front (true) or left it
    IieInFront(bool),
}
