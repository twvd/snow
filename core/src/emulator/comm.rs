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
use crate::emulator::MouseMode;
use crate::emulator::devices::{DeviceConfig, SlotAddr};
use crate::keymap::KeyEvent;
use crate::mac::MacModel;
use crate::mac::maclc::pds_iie::GamePort;
use crate::mac::scc::SccCh;
#[cfg(feature = "ethernet")]
use crate::mac::scsi::ethernet::EthernetLinkType;
use crate::mac::serial_bridge::SerialBridgeStatus;
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
    /// Hands a device its settings, creating it if its slot is empty
    AddDevice(DeviceConfig),
    /// Takes a device out of its slot
    RemoveDevice(SlotAddr),
    /// Inserts a floppy image from a file.
    /// Parameters: slot, file, write-protect
    InsertFloppy(SlotAddr, String, bool),
    /// Inserts a floppy image, passing the image as boxed object.
    /// Parameters: slot, image, write-protect
    #[serde(skip)]
    InsertFloppyImage(SlotAddr, Box<FloppyImage>, bool),
    SaveFloppy(SlotAddr, PathBuf),
    EjectFloppy(SlotAddr),
    /// Sets the joystick and buttons on the Apple IIe card's game port.
    SetIieGamePort(GamePort),
    /// Toggle automatic writeback for a floppy drive.
    /// Parameters: slot, enabled
    SetFloppyWriteback(SlotAddr, bool),
    /// Branches a disk's image off to a new file
    BranchDisk(SlotAddr, PathBuf),
    /// Puts an image in a drive
    LoadMedia(SlotAddr, PathBuf),
    /// Takes the image out of a drive
    EjectMedia(SlotAddr),
    #[cfg(feature = "ethernet")]
    EthernetSetLink(SlotAddr, EthernetLinkType),
    #[cfg(feature = "ethernet")]
    EthernetStartCapture(SlotAddr, PathBuf),
    #[cfg(feature = "ethernet")]
    EthernetStopCapture(SlotAddr),
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
    SetFloppyRpmAdjustment(SlotAddr, i32),
    /// Seeds the floppy noise (weak bits) RNG for deterministic behaviour.
    /// `None` reverts to non-deterministic randomness.
    SetFloppyWeakBitsSeed(Option<u64>),
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
    pub buses: Vec<super::devices::Bus>,
}

#[cfg(feature = "ethernet")]
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    FloppyEjected(SlotAddr, Box<FloppyImage>),
    MediaEjected(SlotAddr),
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
