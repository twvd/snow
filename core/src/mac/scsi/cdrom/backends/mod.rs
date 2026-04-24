use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::mac::scsi::cdrom::CdromBackend;

pub mod cuesheet;
pub mod iso;
#[cfg(windows)]
pub mod windows_drive;

pub struct PhysicalCdromDrive {
    pub friendly_name: String,
    pub path: PathBuf,
}

/// Query the physical CD-ROM drives available.
///
/// Returns an empty Vec if physical CD-ROM drives are supported
/// but none are available. Returns None if physical CD-ROM drives
/// are not supported on this platform.
pub fn query_physical_cdrom_drives() -> Option<Vec<PhysicalCdromDrive>> {
    #[allow(unused_mut, unused_assignments)]
    let mut result = None;

    #[cfg(windows)]
    {
        result = Some(windows_drive::query_physical_cdrom_drives());
    }

    result
}

#[allow(unused_variables)]
pub fn is_physical_cdrom_drive_path(path: &Path) -> bool {
    #[allow(unused_mut, unused_assignments)]
    let mut result = false;

    #[cfg(windows)]
    {
        result = windows_drive::is_physical_cdrom_drive_path(path);
    }

    result
}

#[allow(unused_variables)]
pub fn new_physical_cdrom_drive_backend(path: &Path) -> Result<Box<dyn CdromBackend>> {
    #[allow(unused_mut, unused_assignments)]
    let mut result = None;

    #[cfg(windows)]
    {
        result = Some(Box::new(windows_drive::WindowsDriveCdromBackend::new(
            path,
        )?));
    }

    if result.is_none() {
        unimplemented!("Physical CD-ROM drives are not supported on this platform");
    }

    Ok(result.unwrap())
}
