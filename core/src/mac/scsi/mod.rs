//! SCSI controller, devices and associated code

pub mod controller;
pub mod disk;
pub mod target;

pub const STATUS_GOOD: u8 = 0;
pub const STATUS_CHECK_CONDITION: u8 = 2;

const fn scsi_cmd_len(cmdnum: u8) -> Option<usize> {
    match cmdnum {
        // UNIT READY
        0x00
        // REQUEST SENSE
        | 0x03
        // FORMAT UNIT
        | 0x04
        // READ(6)
        | 0x08
        // WRITE(6)
        | 0x0A
        // INQUIRY
        | 0x12
        // MODE SELECT(6)
        | 0x15
        // MODE SENSE(6)
        | 0x1A
        => Some(6),
        // READ CAPACITY(10)
        0x25
        // READ(10)
        | 0x28
        // WRITE(10)
        | 0x2A
        // VERIFY(10)
        | 0x2F
        // READ BUFFER(10)
        | 0x3C
        => Some(10),
        _ => {
            None
        }
    }
}

/// Result of a command
pub(super) enum ScsiCmdResult {
    /// Immediately turn to the Status phase
    Status(u8),
    /// Returns data to the initiator
    DataIn(Vec<u8>),
    /// Expects data written to target
    DataOut(usize),
}
