use crate::{bus::Address, cpu_m68k::regs::RegisterFile, tickable::Ticks};

pub type EmulatorCommandSender = crossbeam_channel::Sender<EmulatorCommand>;
pub type EmulatorEventReceiver = crossbeam_channel::Receiver<EmulatorEvent>;

/// A command/event that can be sent to the emulator
#[derive(Debug)]
pub enum EmulatorCommand {
    Quit,
    InsertFloppy(Box<[u8]>),
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
