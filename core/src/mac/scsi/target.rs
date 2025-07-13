//! SCSI target generic/shared code

use std::path::Path;

use anyhow::Result;

use crate::mac::scsi::{ScsiCmdResult, STATUS_CHECK_CONDITION, STATUS_GOOD};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// Enumeration of supported emulated SCSI target types (devices)
pub enum ScsiTargetType {
    Disk,
    Cdrom,
}

/// Some events that may occur to feed to the UI through EmulatorEvent
pub enum ScsiTargetEvent {
    MediaEjected,
}

/// An abstraction of a generic SCSI target
pub(crate) trait ScsiTarget {
    fn take_event(&mut self) -> Option<ScsiTargetEvent>;

    fn target_type(&self) -> ScsiTargetType;
    fn unit_ready(&mut self) -> Result<ScsiCmdResult>;
    fn inquiry(&mut self, cmd: &[u8]) -> Result<ScsiCmdResult>;
    fn mode_sense(&mut self, page: u8) -> Result<ScsiCmdResult>;

    /// Request sense result (code, asc, ascq)
    fn req_sense(&mut self) -> (u8, u16);

    // For block devices
    fn blocksize(&self) -> Option<usize>;
    fn blocks(&self) -> Option<usize>;
    fn read(&self, block_offset: usize, block_count: usize) -> &[u8];
    fn write(&mut self, block_offset: usize, data: &[u8]);
    fn image_fn(&self) -> Option<&Path>;
    fn load_media(&mut self, path: &Path) -> Result<()>;

    /// Device-specific commands
    fn specific_cmd(&mut self, cmd: &[u8], outdata: Option<&[u8]>) -> Result<ScsiCmdResult>;

    /// Returns the drives total capacity in bytes
    fn capacity(&self) -> Option<usize> {
        Some(self.blocksize()? * self.blocks()?)
    }

    fn cmd(&mut self, cmd: &[u8], outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        match cmd[0] {
            0x00 => {
                // UNIT READY
                self.unit_ready()
            }
            0x03 => {
                // REQUEST SENSE
                let (key, asc) = self.req_sense();
                let mut result = vec![0; 14];
                result[2] = key & 0x0F;
                result[12..14].copy_from_slice(&asc.to_be_bytes());
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x04 => {
                // FORMAT UNIT(6)
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            0x08 => {
                // READ(6)
                let Some(blocks) = self.blocks() else {
                    log::warn!("READ(6) command to non-block device");
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };
                let blocknum = (u32::from_be_bytes(cmd[0..4].try_into()?) & 0x1F_FFFF) as usize;
                let blockcnt = if cmd[4] == 0 { 256 } else { cmd[4] as usize };

                if blocknum + blockcnt > blocks {
                    log::error!("Reading beyond disk");
                    Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                } else {
                    Ok(ScsiCmdResult::DataIn(
                        self.read(blocknum, blockcnt).to_vec(),
                    ))
                }
            }
            0x0A => {
                // WRITE(6)
                let (Some(blocksize), Some(blocks)) = (self.blocksize(), self.blocks()) else {
                    log::warn!("WRITE(6) command to non-block device");
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };
                let blocknum = (u32::from_be_bytes(cmd[0..4].try_into()?) & 0x1F_FFFF) as usize;
                let blockcnt = if cmd[4] == 0 { 256 } else { cmd[4] as usize };

                if let Some(data) = outdata {
                    if blocknum + blockcnt > blocks {
                        log::error!("Writing beyond disk");
                        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                    } else {
                        self.write(blocknum, data);
                        Ok(ScsiCmdResult::Status(STATUS_GOOD))
                    }
                } else {
                    Ok(ScsiCmdResult::DataOut(blockcnt * blocksize))
                }
            }
            0x12 => {
                // INQUIRY
                self.inquiry(cmd)
            }
            0x15 => {
                // MODE SELECT(6)
                Ok(ScsiCmdResult::DataIn(vec![0; 40]))
            }
            0x1A => {
                // MODE SENSE(6)
                self.mode_sense(cmd[2] & 0x3F)
            }
            0x25 => {
                // READ CAPACITY(10)
                let mut result = vec![0; 8];
                let (Some(blocksize), Some(blocks)) = (self.blocksize(), self.blocks()) else {
                    log::warn!("READ CAPACITY(10) command to non-block device");
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                result[0..4].copy_from_slice(&((blocks as u32) - 1).to_be_bytes());
                result[4..8].copy_from_slice(&(blocksize as u32).to_be_bytes());
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x28 => {
                // READ(10)
                let Some(blocks) = self.blocks() else {
                    log::warn!("READ(10) command to non-block device");
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };
                let blocknum = (u32::from_be_bytes(cmd[2..6].try_into()?)) as usize;
                let blockcnt = (u16::from_be_bytes(cmd[7..9].try_into()?)) as usize;

                if blocknum + blockcnt > blocks {
                    log::error!("Reading beyond disk");
                    Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                } else {
                    Ok(ScsiCmdResult::DataIn(
                        self.read(blocknum, blockcnt).to_vec(),
                    ))
                }
            }
            0x2A => {
                // WRITE(10)
                let (Some(blocksize), Some(blocks)) = (self.blocksize(), self.blocks()) else {
                    log::warn!("WRITE(10) command to non-block device");
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };
                let blocknum = (u32::from_be_bytes(cmd[2..6].try_into()?)) as usize;
                let blockcnt = (u16::from_be_bytes(cmd[7..9].try_into()?)) as usize;

                if let Some(data) = outdata {
                    if blocknum + blockcnt > blocks {
                        log::error!("Writing beyond disk");
                        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                    } else {
                        self.write(blocknum, data);
                        Ok(ScsiCmdResult::Status(STATUS_GOOD))
                    }
                } else {
                    Ok(ScsiCmdResult::DataOut(blockcnt * blocksize))
                }
            }
            0x2F => {
                // VERIFY(10)
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            0x3C => {
                // READ BUFFER(10)
                let result = vec![0; 4];
                // 0 reserved (0)
                // 1-3 buffer length (0)
                Ok(ScsiCmdResult::DataIn(result))
            }
            _ => self.specific_cmd(cmd, outdata),
        }
    }
}
