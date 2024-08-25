use crate::{bus::Address, cpu_m68k::regs::RegisterFile, mac::keyboard::KeyEvent, tickable::Ticks};

pub type EmulatorCommandSender = crossbeam_channel::Sender<EmulatorCommand>;
pub type EmulatorEventReceiver = crossbeam_channel::Receiver<EmulatorEvent>;

/// A command/event that can be sent to the emulator
pub enum EmulatorCommand {
    Quit,
    InsertFloppy(Box<[u8]>),
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
