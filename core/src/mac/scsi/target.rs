//! SCSI target generic/shared code

use std::path::Path;

use anyhow::Result;

use crate::mac::scsi::{
    ScsiCmdResult, ASC_INVALID_FIELD_IN_CDB, ASC_LOGICAL_BLOCK_ADDRESS_OUT_OF_RANGE,
    ASC_MEDIUM_NOT_PRESENT, CC_KEY_ILLEGAL_REQUEST, CC_KEY_MEDIUM_ERROR, STATUS_CHECK_CONDITION,
    STATUS_GOOD,
};

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
#[typetag::serde(tag = "type")]
pub(crate) trait ScsiTarget: Send {
    #[cfg(feature = "savestates")]
    fn after_deserialize(&mut self, imgfn: &Path) -> Result<()>;

    fn set_cc(&mut self, code: u8, asc: u16);
    fn set_blocksize(&mut self, blocksize: usize) -> bool;
    fn take_event(&mut self) -> Option<ScsiTargetEvent>;

    fn target_type(&self) -> ScsiTargetType;
    fn unit_ready(&mut self) -> Result<ScsiCmdResult>;
    fn inquiry(&mut self, cmd: &[u8]) -> Result<ScsiCmdResult>;
    fn mode_sense(&mut self, page: u8) -> Option<Vec<u8>>;
    fn ms_density(&self) -> u8;
    fn ms_media_type(&self) -> u8;
    fn ms_device_specific(&self) -> u8;

    /// Request sense result (code, asc, ascq)
    fn req_sense(&mut self) -> (u8, u16);

    // For block devices
    fn blocksize(&self) -> Option<usize>;
    fn blocks(&self) -> Option<usize>;
    fn read(&self, block_offset: usize, block_count: usize) -> Vec<u8>;
    fn write(&mut self, block_offset: usize, data: &[u8]);
    fn image_fn(&self) -> Option<&Path>;
    fn load_media(&mut self, path: &Path) -> Result<()>;
    fn branch_media(&mut self, path: &Path) -> Result<()>;
    fn media(&self) -> Option<&[u8]>;

    /// Device-specific commands
    fn specific_cmd(&mut self, cmd: &[u8], outdata: Option<&[u8]>) -> Result<ScsiCmdResult>;

    /// Returns the drives total capacity in bytes
    fn capacity(&self) -> Option<usize> {
        Some(self.blocksize()? * self.blocks()?)
    }

    fn check_lba_within_capacity(&mut self, lba: u32) -> bool {
        if let Some(capacity) = self.capacity() {
            if lba as usize >= capacity / self.blocksize().unwrap() {
                log::error!(
                    "Seeking beyond disk, lba: {}, capacity: {}, blocksize: {}",
                    lba,
                    capacity,
                    self.blocksize().unwrap()
                );
                self.set_cc(
                    CC_KEY_ILLEGAL_REQUEST,
                    ASC_LOGICAL_BLOCK_ADDRESS_OUT_OF_RANGE,
                );
                return false;
            }
        }
        true
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
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
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
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
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
            0x0B => {
                // SEEK(6)
                let lba: u32 = ((u32::from(cmd[1]) & 0x1F) << 16)
                    | (u32::from(cmd[2]) << 8)
                    | u32::from(cmd[3]);

                if self.check_lba_within_capacity(lba) {
                    Ok(ScsiCmdResult::Status(STATUS_GOOD))
                } else {
                    Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                }
            }
            0x12 => {
                // INQUIRY
                self.inquiry(cmd)
            }
            0x15 => {
                // MODE SELECT(6)
                if let Some(od) = outdata {
                    if od.len() < 12 {
                        log::error!("Outdata for MODE SELECT(6) too short: {}", od.len());
                        return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                    }
                    if let Some(current_blocksize) = self.blocksize() {
                        let blocksize = (usize::from(od[9]) << 16)
                            | (usize::from(od[10]) << 8)
                            | usize::from(od[11]);
                        if current_blocksize != blocksize && !self.set_blocksize(blocksize) {
                            log::error!("Failed to change block size to {}", blocksize);
                        }
                    }
                    Ok(ScsiCmdResult::Status(STATUS_GOOD))
                } else {
                    Ok(ScsiCmdResult::DataOut(cmd[4] as usize))
                }
            }
            0x1A => {
                // MODE SENSE(6)
                let page = cmd[2] & 0x3F;
                let _subpage = cmd[3];
                let dbd = cmd[1] & (1 << 3) != 0;
                let pc = (cmd[2] >> 6) & 0b11;
                let alloc_len = cmd[4] as usize;

                let mut result: Vec<u8> = vec![];

                if pc != 0b00 {
                    log::error!("MODE SENSE(6) unimplemented PC: {}", pc);
                }

                // Length placeholder
                result.push(0);
                // Media type
                result.push(self.ms_media_type());
                // Device specific parameter
                result.push(self.ms_device_specific());
                // Block Descriptor length
                result.push(if dbd { 0 } else { 8 });

                if !dbd {
                    // Block descriptor
                    // Density
                    result.push(self.ms_density());
                    // 3x number of blocks + 1x reserved
                    result.extend_from_slice(&[0, 0, 0, 0]);

                    // Block size
                    let blocksize = self.blocksize().unwrap() as u32;
                    result.push((blocksize >> 16) as u8);
                    result.push((blocksize >> 8) as u8);
                    result.push(blocksize as u8);
                }

                if page == 0x3F {
                    // Return all supported pages
                    for p in 0..=0x3E {
                        if let Some(pagedata) = self.mode_sense(p) {
                            result.push(p);
                            result.push(pagedata.len() as u8);
                            result.extend_from_slice(&pagedata);
                        }
                    }
                } else if let Some(pagedata) = self.mode_sense(page) {
                    result.push(page);
                    result.push(pagedata.len() as u8);
                    result.extend_from_slice(&pagedata);
                } else {
                    log::warn!("Unknown MODE SENSE page {:02X}", page);
                    self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                result[0] = result.len() as u8;
                result.truncate(alloc_len);
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x2B => {
                // SEEK(10)
                let lba: u32 = (u32::from(cmd[2]) << 24)
                    | (u32::from(cmd[3]) << 16)
                    | (u32::from(cmd[4]) << 8)
                    | u32::from(cmd[5]);

                if self.check_lba_within_capacity(lba) {
                    Ok(ScsiCmdResult::Status(STATUS_GOOD))
                } else {
                    Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                }
            }
            0x25 => {
                // READ CAPACITY(10)
                let mut result = vec![0; 8];
                let (Some(blocksize), Some(blocks)) = (self.blocksize(), self.blocks()) else {
                    log::warn!("READ CAPACITY(10) command to non-block device");
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
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
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
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
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
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
