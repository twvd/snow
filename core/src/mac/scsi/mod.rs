//! SCSI controller, devices and associated code

use std::path::Path;

use anyhow::Result;

use crate::mac::scsi::controller::ScsiCmdResult;

pub mod controller;
pub mod disk;

pub enum ScsiTargetType {
    Disk,
    Cdrom,
}

trait ScsiTarget {
    fn inquiry(&mut self, cmd: &[u8]) -> Result<ScsiCmdResult>;
    fn mode_sense(&mut self, page: u8) -> Result<ScsiCmdResult>;

    // For block devices
    fn blocksize(&self) -> Option<usize>;
    fn blocks(&self) -> Option<usize>;
    fn read(&self, block_offset: usize, block_count: usize) -> &[u8];
    fn write(&mut self, block_offset: usize, data: &[u8]);
    fn image_fn(&self) -> Option<&Path>;

    /// Returns the drives total capacity in bytes
    fn capacity(&self) -> Option<usize> {
        Some(self.blocksize()? * self.blocks()?)
    }
}
