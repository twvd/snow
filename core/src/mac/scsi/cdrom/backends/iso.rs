use anyhow::{Result, anyhow};
use std::path::Path;

use crate::mac::scsi::{
    cdrom::{
        CdromBackend, CdromError, DATA_TRACK, LBA_START_SECTOR, RawSector, SessionInfo, TrackInfo,
    },
    disk_image::DiskImage,
};

pub struct IsoCdromBackend {
    image: Box<dyn DiskImage>,
    session: SessionInfo,
    track: TrackInfo,
}

impl IsoCdromBackend {
    pub fn new(image: Box<dyn DiskImage>) -> Result<Self> {
        let sector_count: u32 = image.byte_len().div_ceil(2048).try_into()?;
        Ok(Self {
            image,
            session: SessionInfo {
                number: 1,
                disc_type: 0x00,
                leadin: 0,
                leadout: LBA_START_SECTOR + sector_count,
            },
            track: TrackInfo {
                tno: 1,
                session: 1,
                control: DATA_TRACK,
                sector: LBA_START_SECTOR,
            },
        })
    }
}

impl CdromBackend for IsoCdromBackend {
    fn check_media(&mut self) -> Result<(), CdromError> {
        Ok(())
    }

    fn byte_len(&self) -> usize {
        self.image.byte_len()
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError> {
        Ok(self.image.read_bytes(offset, length))
    }

    fn image_path(&self) -> Option<&Path> {
        self.image.image_path()
    }

    fn sessions(&self) -> Option<&[SessionInfo]> {
        Some(std::slice::from_ref(&self.session))
    }

    fn tracks(&self) -> Option<&[TrackInfo]> {
        Some(std::slice::from_ref(&self.track))
    }

    fn read_cdda_sector(&self, _sector: u32) -> Result<RawSector, CdromError> {
        Err(anyhow!("Reading CDDA sectors is not implemented for ISO files").into())
    }
}
