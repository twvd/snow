//! Communication between emulator and frontend

use crate::bus::Address;
use crate::cpu_m68k::regs::RegisterFile;
use crate::keymap::KeyEvent;
use crate::mac::MacModel;
use crate::tickable::Ticks;

pub type EmulatorCommandSender = crossbeam_channel::Sender<EmulatorCommand>;
pub type EmulatorEventReceiver = crossbeam_channel::Receiver<EmulatorEvent>;

/// A command/event that can be sent to the emulator
pub enum EmulatorCommand {
    Quit,
    InsertFloppy(usize, String),
    SaveFloppy(usize, String),
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
    Step,
    ToggleBreakpoint(Address),
    BusWrite(Address, Vec<u8>),
    Disassemble(Address, usize),
    KeyEvent(KeyEvent),
    ToggleBusTrace,
    CpuSetPC(u32),
    SetSpeed(EmulatorSpeed),
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
    pub breakpoints: Vec<Address>,
    pub cycles: Ticks,

    pub fdd: [FddStatus; 3],
    pub model: MacModel,
    pub speed: EmulatorSpeed,
    pub hdd: [Option<usize>; 7],
}

#[derive(Debug)]
pub struct FddStatus {
    pub present: bool,
    pub ejected: bool,
    pub motor: bool,
    pub writing: bool,
    pub track: usize,
    pub image_title: String,
}

/// A status message/event received from the emulator
#[derive(Debug)]
pub enum EmulatorEvent {
    Status(Box<EmulatorStatus>),
    NextCode((Address, Vec<u8>)),
}
