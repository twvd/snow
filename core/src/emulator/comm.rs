//! Communication between emulator and frontend

use crate::bus::Address;
use crate::cpu_m68k::regs::RegisterFile;
use crate::keymap::KeyEvent;
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
#[derive(Debug)]
pub enum EmulatorSpeed {
    /// Actual speed accurate to the real hardware
    Accurate,
    /// Actual speed when sound is played, otherwise uncapped
    Dynamic,
    /// Uncapped at all times, sound disabled
    Uncapped,
}

/// Structure with general emulator status
#[derive(Debug)]
pub struct EmulatorStatus {
    pub regs: RegisterFile,
    pub running: bool,
    pub breakpoints: Vec<Address>,
    pub cycles: Ticks,
}

/// A status message/event received from the emulator
#[derive(Debug)]
pub enum EmulatorEvent {
    Status(EmulatorStatus),
    NextCode((Address, Vec<u8>)),
}
