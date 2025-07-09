//! SCSI hard disk drive (block device)

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
    pub(super) fn load_disk(filename: &Path) -> Option<Self> {
        use fs2::FileExt;
        use std::fs::OpenOptions;

        if !Path::new(filename).exists() {
            // File not found
            return None;
        }

        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(filename)
            .inspect_err(|e| {
                log::error!(
                    "Opening disk image {} failed: {}",
                    filename.to_string_lossy(),
                    e
                );
            })
            .ok()?;

        f.lock_exclusive()
            .inspect_err(|e| {
                log::error!(
                    "Cannot lock disk image {}: {}",
                    filename.to_string_lossy(),
                    e
                );
            })
            .ok()?;

        let mmapped = unsafe {
            MmapMut::map_mut(&f)
                .inspect_err(|e| {
                    log::error!(
                        "Cannot mmap image file {}: {}",
                        filename.to_string_lossy(),
                        e
                    );
                })
                .ok()?
        };

        if mmapped.len() % DISK_BLOCKSIZE != 0 {
            log::error!(
                "Cannot load disk image {}: not multiple of {}",
                filename.to_string_lossy(),
                DISK_BLOCKSIZE
            );
            return None;
        }

        Some(Self {
            disk: mmapped,
            path: filename.to_path_buf(),
        })
    }

    #[cfg(not(feature = "mmap"))]
    pub(super) fn load_disk(filename: &Path) -> Option<Self> {
        use std::fs;

        if !Path::new(filename).exists() {
            // File not found
            return None;
        }

        let disk = match fs::read(filename) {
            Ok(d) => d,
            Err(e) => {
                log::error!("Failed to open file: {}", e);
                return None;
            }
        };

        if disk.len() % DISK_BLOCKSIZE != 0 {
            log::error!(
                "Cannot load disk image {}: not multiple of {}",
                filename.to_string_lossy(),
                DISK_BLOCKSIZE
            );
            return None;
        }

        Some(Self {
            disk,
            path: filename.to_path_buf(),
        })
    }

    /// Returns the drives total capacity in bytes
    pub(super) fn capacity(&self) -> usize {
        self.disk.len()
    }
}
