pub type EmulatorCommandSender = crossbeam_channel::Sender<EmulatorCommand>;

/// A command/event that can be sent to the emulator
pub enum EmulatorCommand {
    Quit,
    InsertFloppy(Box<[u8]>),
    MouseUpdateRelative {
        relx: i16,
        rely: i16,
        btn: Option<bool>,
    },
}
