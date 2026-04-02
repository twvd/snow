use anyhow::{bail, Result};
use std::path::Path;

use crate::mac::scsi::{
    cdrom::{CdromBackend, Msf, SessionInfo, TrackInfo, DATA_TRACK, RAW_SECTOR_LEN},
    disk_image::DiskImage,
};

pub struct IsoCdromBackend {
    image: Box<dyn DiskImage>,
    session: SessionInfo,
}

impl IsoCdromBackend {
    pub fn new(image: Box<dyn DiskImage>) -> Result<Self> {
        const START_SECTOR: u32 = Msf::new(0, 2, 0).to_sector();
        let sector_count: u32 = image.byte_len().div_ceil(2048).try_into()?;
        let leadout_sector = START_SECTOR + sector_count;
        Ok(Self {
            image,
            session: SessionInfo {
                leadout: Msf::from_sector(leadout_sector)?,
                tracks: vec![TrackInfo {
                    tno: 1,
                    control: DATA_TRACK,
                    sector: START_SECTOR,
                }],
            },
        })
    }
}

impl CdromBackend for IsoCdromBackend {
    fn byte_len(&self) -> usize {
        self.image.byte_len()
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8> {
        self.image.read_bytes(offset, length)
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
