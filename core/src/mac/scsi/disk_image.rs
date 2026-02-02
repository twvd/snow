//! SCSI disk image abstraction

use anyhow::{bail, Context, Result};
#[cfg(feature = "mmap")]
use memmap2::MmapMut;
use std::path::{Path, PathBuf};

pub trait DiskImage: Send {
    fn byte_len(&self) -> usize;
    fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8>;
    fn write_bytes(&mut self, offset: usize, data: &[u8]);
    fn media_bytes(&self) -> Option<&[u8]>;
    fn image_path(&self) -> Option<&Path>;
    fn branch_media(&mut self, _path: &Path) -> Result<()> {
        bail!("branch_media not supported");
    }
}

pub(crate) struct FileDiskImage {
    /// Disk contents
    #[cfg(feature = "mmap")]
    disk: MmapMut,

    #[cfg(not(feature = "mmap"))]
    disk: Vec<u8>,

    /// Path where the original image resides
    path: PathBuf,
}

impl FileDiskImage {
    pub(super) fn open(filename: &Path) -> Result<Self> {
        Self::open_file(filename)
    }

    pub(super) fn open_block_sized(filename: &Path, block_size: usize) -> Result<Self> {
        let image = Self::open_file(filename)?;
        if !image.byte_len().is_multiple_of(block_size) {
            bail!(
                "Cannot load disk image {}: not multiple of {}",
                filename.display(),
                block_size
            );
        }
        Ok(image)
    }

    fn open_file(filename: &Path) -> Result<Self> {
        if !filename.exists() {
            bail!("File not found: {}", filename.display());
        }

        #[cfg(feature = "mmap")]
        let disk = Self::mmap_file(filename)?;

        #[cfg(not(feature = "mmap"))]
        let disk = {
            use std::fs;

            let disk = fs::read(filename)
                .with_context(|| format!("Failed to open file {}", filename.display()))?;
            disk
        };

        Ok(Self {
            disk,
            path: filename.to_path_buf(),
        })
    }

    #[cfg(feature = "mmap")]
    fn mmap_file(filename: &Path) -> Result<MmapMut> {
        use fs2::FileExt;
        use std::fs::OpenOptions;
        use std::io::{Seek, SeekFrom};

        if !filename.exists() {
            bail!("File not found: {}", filename.display());
        }
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(filename)
            .with_context(|| format!("Failed to open {}", filename.display()))?;
        let file_size = f.seek(SeekFrom::End(0))? as usize;
        f.seek(SeekFrom::Start(0))?;
        f.try_lock_exclusive()
            .with_context(|| format!("Failed to lock {}", filename.display()))?;
        let mmapped = unsafe {
            use memmap2::MmapOptions;

            MmapOptions::new()
                .len(file_size)
                .map_mut(&f)
                .with_context(|| format!("Failed to mmap file {}", filename.display()))?
        };
        Ok(mmapped)
    }
}

impl DiskImage for FileDiskImage {
    fn byte_len(&self) -> usize {
        self.disk.len()
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8> {
        self.disk[offset..(offset + length)].to_vec()
    }

    fn write_bytes(&mut self, offset: usize, data: &[u8]) {
        self.disk[offset..(offset + data.len())].copy_from_slice(data);
    }

    fn media_bytes(&self) -> Option<&[u8]> {
        Some(&self.disk)
    }

    fn image_path(&self) -> Option<&Path> {
        Some(self.path.as_ref())
    }

    fn branch_media(&mut self, path: &Path) -> Result<()> {
        #[cfg(feature = "mmap")]
        {
            use std::fs::File;
            use std::io::Write;

            // Create a fresh disk file
            {
                let mut f = File::create(path)?;
                f.write_all(&self.disk)?;
            }
            self.disk = Self::mmap_file(path)?;
            self.path = path.to_path_buf();
            Ok(())
        }
        #[cfg(not(feature = "mmap"))]
        {
            let _ = path;
            bail!("Requires 'mmap' feature");
        }
    }
}
