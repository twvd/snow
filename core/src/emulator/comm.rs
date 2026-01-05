//! Communication between emulator and frontend

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use snow_floppy::FloppyImage;

use crate::bus::Address;
use crate::cpu_m68k::cpu::{HistoryEntry, SystrapHistoryEntry};
use crate::cpu_m68k::regs::{Register, RegisterFile};
use crate::debuggable::DebuggableProperties;
use crate::keymap::KeyEvent;
use crate::mac::scc::SccCh;
#[cfg(feature = "ethernet")]
use crate::mac::scsi::ethernet::EthernetLinkType;
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::serial_bridge::{SerialBridgeConfig, SerialBridgeStatus};
use crate::mac::MacModel;
use crate::tickable::Ticks;

pub use crate::cpu_m68k::cpu::{Breakpoint, BusBreakpoint};

pub type EmulatorCommandSender = crossbeam_channel::Sender<EmulatorCommand>;
pub type EmulatorEventReceiver = crossbeam_channel::Receiver<EmulatorEvent>;

pub type InputRecording = Vec<(Ticks, EmulatorCommand)>;

/// A command/event that can be sent to the emulator
#[derive(Serialize, Deserialize, Clone)]
pub enum EmulatorCommand {
    Quit,
    /// Inserts a floppy image, passing the image as boxed object.
    /// Parameters: drive id, image, write-protect
    InsertFloppy(usize, String, bool),
    /// Inserts a floppy image, passing the image as boxed object.
    /// Parameters: drive id, image, write-protect
    #[serde(skip)]
    InsertFloppyImage(usize, Box<FloppyImage>, bool),
    SaveFloppy(usize, PathBuf),
    EjectFloppy(usize),
    ScsiAttachHdd(usize, PathBuf),
    ScsiBranchHdd(usize, PathBuf),
    ScsiAttachCdrom(usize),
    ScsiLoadMedia(usize, PathBuf),
    #[cfg(feature = "ethernet")]
    ScsiAttachEthernet(usize),
    #[cfg(feature = "ethernet")]
    EthernetSetLink(usize, EthernetLinkType),
    DetachScsiTarget(usize),
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
    BusInspectWrite(Address, Vec<u8>),
    Disassemble(Address, usize),
    KeyEvent(KeyEvent),
    ReleaseAllInputs,
    CpuSetPC(u32),
    #[serde(skip)]
    SetSpeed(EmulatorSpeed),
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
    SetFloppyRpmAdjustment(usize, i32),
    #[serde(skip)]
    SerialBridgeEnable(SccCh, SerialBridgeConfig),
    SerialBridgeDisable(SccCh),
}

/// Emulator speed tweak
#[derive(Debug, Copy, Clone, strum::Display, Eq, PartialEq, Serialize, Deserialize)]
pub enum EmulatorSpeed {
    /// Actual speed accurate to the real hardware
    Accurate,
    /// Actual speed when sound is played, otherwise uncapped
    Dynamic,
    /// Uncapped at all times, sound disabled
    Uncapped,
    /// Sync to 60 fps video, sound disabled
    Video,
}

/// Structure with general emulator status
#[derive(Debug)]
pub struct EmulatorStatus {
    pub regs: RegisterFile,
    pub running: bool,
    pub breakpoints: Vec<Breakpoint>,
    pub cycles: Ticks,

    pub fdd: [FddStatus; 3],
    pub model: MacModel,
    pub speed: EmulatorSpeed,
    pub effective_speed: f64,
    pub scsi: [Option<ScsiTargetStatus>; 7],
}

#[derive(Debug, Clone)]
pub struct ScsiTargetStatus {
    pub target_type: ScsiTargetType,
    pub image: Option<PathBuf>,
    pub capacity: Option<usize>,
    #[cfg(feature = "ethernet")]
    pub link_type: Option<EthernetLinkType>,
}

#[derive(Debug)]
pub struct FddStatus {
    pub present: bool,
    pub ejected: bool,
    pub motor: bool,
    pub writing: bool,
    pub track: usize,
    pub image_title: String,
    pub dirty: bool,
    pub drive_type: crate::mac::swim::drive::DriveType,
}

/// A friendly message ready for display to a user
#[derive(Debug)]
pub enum UserMessageType {
    Success,
    Notice,
    Warning,
    Error,
}

/// A status message/event received from the emulator
#[derive(strum::Display)]
pub enum EmulatorEvent {
    Status(Box<EmulatorStatus>),
    NextCode((Address, Vec<u8>)),
    UserMessage(UserMessageType, String),
    FloppyEjected(usize, Box<FloppyImage>),
    ScsiMediaEjected(usize),
    Memory((Address, Vec<u8>, usize)),
    RecordedInput(InputRecording),
    InstructionHistory(Vec<HistoryEntry>),
    PeripheralDebug(DebuggableProperties),
    SccTransmitData(SccCh, Vec<u8>),
    SystrapHistory(Vec<SystrapHistoryEntry>),
    /// Serial bridge status update
    SerialBridgeStatus(SccCh, Option<SerialBridgeStatus>),
}
