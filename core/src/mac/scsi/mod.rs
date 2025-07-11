//! SCSI controller, devices and associated code

pub mod controller;
pub mod disk;
pub mod target;

/// Result of a command
pub(super) enum ScsiCmdResult {
    /// Immediately turn to the Status phase
    Status(u8),
    /// Returns data to the initiator
    DataIn(Vec<u8>),
    /// Expects data written to target
    DataOut(usize),
}
