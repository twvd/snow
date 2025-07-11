//! SCSI controller, devices and associated code

pub mod controller;
pub mod disk;
pub mod target;

pub const STATUS_GOOD: u8 = 0;
pub const STATUS_CHECK_CONDITION: u8 = 2;

/// Result of a command
pub(super) enum ScsiCmdResult {
    /// Immediately turn to the Status phase
    Status(u8),
    /// Returns data to the initiator
    DataIn(Vec<u8>),
    /// Expects data written to target
    DataOut(usize),
}
