//! SCSI hard disk drive (block device)

use anyhow::{bail, Context, Result};
#[cfg(feature = "mmap")]
use memmap2::Mmap;

use std::path::Path;
use std::path::PathBuf;

use crate::mac::scsi::target::ScsiTarget;
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::STATUS_CHECK_CONDITION;

pub const DISK_BLOCKSIZE: usize = 512;

pub(super) struct ScsiTargetCdrom {
    /// Disk contents
    #[cfg(feature = "mmap")]
    pub(super) disk: Mmap,

    #[cfg(not(feature = "mmap"))]
    pub(super) disk: Vec<u8>,

    /// Path where the original image resides
    pub(super) path: PathBuf,
}

impl ScsiTargetCdrom {
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

        f.lock_exclusive()
            .with_context(|| format!("Failed to lock {}", filename.display()))?;

        let mmapped = unsafe {
            use memmap2::MmapOptions;

            MmapOptions::new()
                .len(file_size)
                .map(&f)
                .with_context(|| format!("Failed to mmap file {}", filename.display()))?
        };

        Ok(Self {
            disk: mmapped,
            path: filename.to_path_buf(),
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
        })
    }
}

impl ScsiTarget for ScsiTargetCdrom {
    fn inquiry(&mut self, _cmd: &[u8]) -> Result<ScsiCmdResult> {
        let mut result = vec![0; 36];

        // 0 Peripheral qualifier (5-7), peripheral device type (4-0)
        result[0] = 5; // CD-ROM drive
        result[1] = 0x80; // Media removable

        // 4 Additional length (N-4), min. 32
        result[4] = result.len() as u8 - 4;

        // 8..16 Vendor identification
        result[8..(8 + 4)].copy_from_slice(b"SNOW");

        // 16..32 Product identification
        result[16..(16 + 14)].copy_from_slice(b"CD-ROM CDU-55S");
        Ok(ScsiCmdResult::DataIn(result))
    }

    fn mode_sense(&mut self, page: u8) -> Result<ScsiCmdResult> {
        match page {
            0x30 => {
                // ? Non-standard mode page

                let mut result = vec![0; 36];
                // Page code
                result[0] = 0x30;
                // Page length
                result[1] = 0x16;

                result[14..(14 + 22)].copy_from_slice(b"APPLE COMPUTER, INC   ");

                Ok(ScsiCmdResult::DataIn(result))
            }
            _ => {
                log::warn!("Unknown MODE SENSE page {:02X}", page);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }

    fn blocksize(&self) -> Option<usize> {
        Some(DISK_BLOCKSIZE)
    }

    fn blocks(&self) -> Option<usize> {
        Some(self.disk.len() / DISK_BLOCKSIZE)
    }

    fn read(&self, block_offset: usize, block_count: usize) -> &[u8] {
        &self.disk[(block_offset * DISK_BLOCKSIZE)..((block_offset + block_count) * DISK_BLOCKSIZE)]
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {
        log::error!("Write command to CD-ROM");
    }

    fn image_fn(&self) -> Option<&Path> {
        Some(self.path.as_ref())
    }
}
