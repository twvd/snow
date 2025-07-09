//! SCSI hard disk drive (block device)

use anyhow::{bail, Context, Result};
#[cfg(feature = "mmap")]
use memmap2::MmapMut;

use std::path::Path;
use std::path::PathBuf;

pub const DISK_BLOCKSIZE: usize = 512;

pub(super) struct ScsiTargetDisk {
    /// Disk contents
    #[cfg(feature = "mmap")]
    pub(super) disk: MmapMut,

    #[cfg(not(feature = "mmap"))]
    pub(super) disk: Vec<u8>,

    /// Path where the original image resides
    pub(super) path: PathBuf,
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

        f.lock_exclusive()
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

    /// Returns the drives total capacity in bytes
    pub(super) fn capacity(&self) -> usize {
        self.disk.len()
    }
}
