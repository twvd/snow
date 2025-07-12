//! SCSI controller, devices and associated code

pub mod cdrom;
pub mod controller;
pub mod disk;
pub mod target;

pub const STATUS_GOOD: u8 = 0;
pub const STATUS_CHECK_CONDITION: u8 = 2;

pub const CC_KEY_MEDIUM_ERROR: u8 = 0x03;
pub const CC_KEY_ILLEGAL_REQUEST: u8 = 0x05;

pub const ASC_INVALID_FIELD_IN_CDB: u16 = 0x2400;
pub const ASC_MEDIUM_NOT_PRESENT: u16 = 0x3A00;

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
        // START/STOP UNIT
        | 0x1B
        // PREVENT/ALLOW MEDIA REMOVAL
        | 0x1E
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
        // READ TOC
        | 0x43
        => Some(10),
        _ => {
            None
        }
    }
}

/// Result of a command
pub(crate) enum ScsiCmdResult {
    /// Immediately turn to the Status phase
    Status(u8),
    /// Returns data to the initiator
    DataIn(Vec<u8>),
    /// Expects data written to target
    DataOut(usize),
}
