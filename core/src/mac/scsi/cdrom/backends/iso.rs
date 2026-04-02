use anyhow::{bail, Result};
use std::path::Path;

use crate::mac::scsi::{
    cdrom::{CdromBackend, SessionInfo, TrackInfo, DATA_TRACK, LBA_START_SECTOR, RAW_SECTOR_LEN},
    disk_image::DiskImage,
};

pub struct IsoCdromBackend {
    image: Box<dyn DiskImage>,
    session: SessionInfo,
}

impl IsoCdromBackend {
    pub fn new(image: Box<dyn DiskImage>) -> Result<Self> {
        let sector_count: u32 = image.byte_len().div_ceil(2048).try_into()?;
        Ok(Self {
            image,
            session: SessionInfo {
                leadout: LBA_START_SECTOR + sector_count,
                tracks: vec![TrackInfo {
                    tno: 1,
                    control: DATA_TRACK,
                    sector: LBA_START_SECTOR,
                }],
            },
        })
    }
}

impl CdromBackend for IsoCdromBackend {
    fn byte_len(&self) -> usize {
        self.image.byte_len()
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>> {
        Ok(self.image.read_bytes(offset, length))
    }

    fn image_path(&self) -> Option<&Path> {
        self.image.image_path()
    }

    fn sessions(&self) -> Option<&[SessionInfo]> {
        Some(std::slice::from_ref(&self.session))
    }

    fn read_raw_sector(&self, _sector: u32) -> Result<[u8; RAW_SECTOR_LEN]> {
        // TODO: reconstruct raw sectors from ISO data
        // (probably only needed for some disc ripping software to work)
        bail!("Reading raw sectors is not implemented for ISO files");
    }
}
