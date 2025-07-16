//! SCSI hard disk drive (block device)

use anyhow::{bail, Context, Result};
#[cfg(feature = "mmap")]
use memmap2::MmapMut;

use std::path::Path;
use std::path::PathBuf;

use crate::mac::scsi::target::ScsiTarget;
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::STATUS_CHECK_CONDITION;
use crate::mac::scsi::STATUS_GOOD;

pub const DISK_BLOCKSIZE: usize = 512;

pub(super) struct ScsiTargetDisk {
    /// Disk contents
    #[cfg(feature = "mmap")]
    pub(super) disk: MmapMut,

    #[cfg(not(feature = "mmap"))]
    pub(super) disk: Vec<u8>,

    /// Path where the original image resides
    pub(super) path: PathBuf,

    /// Check condition code
    cc_code: u8,

    /// Check condition ASC
    cc_asc: u16,
}

impl ScsiTargetDisk {
    /// Try to load a disk image, given the filename of the image.
    ///
    /// This locks the file on disk and memory maps the file for use by
    /// the emulator for fast access and automatic writes back to disk,
    /// at the discretion of the operating system.
    #[cfg(feature = "mmap")]
    pub(super) fn load_disk(filename: &Path) -> Result<Self> {
        use fs2::FileExt;
        use std::{
            fs::OpenOptions,
            io::{Seek, SeekFrom},
        };

        if !Path::new(filename).exists() {
            bail!("File not found: {}", filename.display());
        }

        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(filename)
            .with_context(|| format!("Failed to open {}", filename.display()))?;

        let file_size = f.seek(SeekFrom::End(0))? as usize;
        f.seek(SeekFrom::Start(0))?;

        if file_size % DISK_BLOCKSIZE != 0 {
            bail!(
                "Cannot load disk image {}: not multiple of {}",
                filename.display(),
                DISK_BLOCKSIZE
            );
        }

        f.try_lock_exclusive()
            .with_context(|| format!("Failed to lock {}", filename.display()))?;

        let mmapped = unsafe {
            use memmap2::MmapOptions;

            MmapOptions::new()
                .len(file_size)
                .map_mut(&f)
                .with_context(|| format!("Failed to mmap file {}", filename.display()))?
        };

        Ok(Self {
            disk: mmapped,
            path: filename.to_path_buf(),
            cc_code: 0,
            cc_asc: 0,
        })
    }

    #[cfg(not(feature = "mmap"))]
    pub(super) fn load_disk(filename: &Path) -> Result<Self> {
        use std::fs;

        if !Path::new(filename).exists() {
            bail!("File not found: {}", filename.display());
        }

        let disk = fs::read(filename)
            .with_context(|| format!("Failed to open file {}", filename.display()))?;

        if disk.len() % DISK_BLOCKSIZE != 0 {
            bail!(
                "Cannot load disk image {}: not multiple of {}",
                filename.display(),
                DISK_BLOCKSIZE
            );
        }

        Ok(Self {
            disk,
            path: filename.to_path_buf(),
            cc_code: 0,
            cc_asc: 0,
        })
    }
}

impl ScsiTarget for ScsiTargetDisk {
    fn load_media(&mut self, _path: &Path) -> Result<()> {
        bail!("load_media on non-removable disk");
    }

    fn take_event(&mut self) -> Option<super::target::ScsiTargetEvent> {
        None
    }

    fn target_type(&self) -> ScsiTargetType {
        ScsiTargetType::Disk
    }

    fn req_sense(&mut self) -> (u8, u16) {
        (0, 0)
    }

    fn unit_ready(&mut self) -> Result<ScsiCmdResult> {
        Ok(ScsiCmdResult::Status(STATUS_GOOD))
    }

    fn inquiry(&mut self, _cmd: &[u8]) -> Result<ScsiCmdResult> {
        let mut result = vec![0; 36];

        // 0 Peripheral qualifier (5-7), peripheral device type (4-0)
        result[0] = 0; // Magnetic disk

        // 4 Additional length (N-4), min. 32
        result[4] = result.len() as u8 - 4;

        // 8..16 Vendor identification
        result[8..(8 + 4)].copy_from_slice(b"SNOW");

        // 16..32 Product identification
        result[16..(16 + 11)].copy_from_slice(b"VIRTUAL HDD");
        Ok(ScsiCmdResult::DataIn(result))
    }

    fn mode_sense(&mut self, page: u8) -> Option<Vec<u8>> {
        match page {
            0x01 => {
                // Read/write recovery page

                // Error recovery stuff, can remain at 0.
                // Also, HD SC Setup doesn't seem to care as long as we respond to this command.

                Some(vec![0; 6])
            }
            0x03 => {
                // Format device page

                // The remaining bytes can remain at 0 as they indicate information on how many
                // sectors/tracks are reserved for defect management.
                // Also, HD SC Setup doesn't seem to care as long as we respond to this command.

                Some(vec![0; 0x16])
            }
            0x30 => {
                // ? Non-standard mode page

                let mut result = vec![0; 20];

                // The string below has to appear for HD SC Setup and possibly other tools to work.
                // https://68kmla.org/bb/index.php?threads/apple-rom-hard-disks.44920/post-493863
                result[0..20].copy_from_slice(b"APPLE COMPUTER, INC.");

                Some(result)
            }
            _ => None,
        }
    }

    fn blocksize(&self) -> Option<usize> {
        Some(DISK_BLOCKSIZE)
    }

    fn blocks(&self) -> Option<usize> {
        Some(self.disk.len() / DISK_BLOCKSIZE)
    }

    fn read(&self, block_offset: usize, block_count: usize) -> Vec<u8> {
        self.disk[(block_offset * DISK_BLOCKSIZE)..((block_offset + block_count) * DISK_BLOCKSIZE)]
            .to_vec()
    }

    fn write(&mut self, block_offset: usize, data: &[u8]) {
        let offset = block_offset * DISK_BLOCKSIZE;
        self.disk[offset..(offset + data.len())].copy_from_slice(data);
    }

    fn image_fn(&self) -> Option<&Path> {
        Some(self.path.as_ref())
    }

    fn specific_cmd(&mut self, cmd: &[u8], _outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        log::error!("Unknown command {:02X}", cmd[0]);
        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
    }

    fn ms_density(&self) -> u8 {
        0
    }

    fn ms_media_type(&self) -> u8 {
        0
    }

    fn ms_device_specific(&self) -> u8 {
        0
    }

    fn set_cc(&mut self, code: u8, asc: u16) {
        self.cc_code = code;
        self.cc_asc = asc;
    }

    fn set_blocksize(&mut self, _blocksize: usize) -> bool {
        false
    }
}
