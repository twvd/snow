//! Communication between emulator and frontend

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use snow_floppy::FloppyImage;

use crate::bus::Address;
use crate::cpu_m68k::cpu::HistoryEntry;
use crate::cpu_m68k::regs::{Register, RegisterFile};
use crate::keymap::KeyEvent;
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
    InsertFloppy(usize, String),
    InsertFloppyWriteProtected(usize, String),
    #[serde(skip)]
    InsertFloppyImage(usize, Box<FloppyImage>),
    SaveFloppy(usize, PathBuf),
    EjectFloppy(usize),
    LoadHddImage(usize, PathBuf),
    DetachHddImage(usize),
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
    BusWrite(Address, Vec<u8>),
    Disassemble(Address, usize),
    KeyEvent(KeyEvent),
    ToggleBusTrace,
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
}

/// Emulator speed tweak
#[derive(Debug, Copy, Clone, strum::Display, Eq, PartialEq)]
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
    pub hdd: [Option<HddStatus>; 7],
}

#[derive(Debug, Clone)]
pub struct HddStatus {
    pub image: PathBuf,
    pub capacity: usize,
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
    Memory((Address, Vec<u8>)),
    RecordedInput(InputRecording),
    InstructionHistory(Vec<HistoryEntry>),
}
