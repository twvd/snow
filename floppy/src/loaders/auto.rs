//! Auto-detect image file type and load

use crate::{
    loaders::{Bitfile, Diskcopy42, FloppyImageLoader, Moof, RawImage},
    FloppyImage, FloppyType,
};

use anyhow::{bail, Result};
use strum::IntoEnumIterator;

pub struct Autodetect {}

impl FloppyImageLoader for Autodetect {
    fn load(data: &[u8]) -> Result<FloppyImage> {
        // MOOF
        if data.len() >= 8 && data[0..8] == *b"MOOF\xFF\n\r\n" {
            return Moof::load(data);
        }
        // Bitfile / 'Dave format'
        if data.len() >= 10
            && data[0..4] == data[4..8]
            && [0, 1].contains(&data[8])
            && [0, 1].contains(&data[9])
        {
            return Bitfile::load(data);
        }
        // Apple DiskCopy 4.2
        if data[0x52..=0x53] == [0x01, 0x00] {
            return Diskcopy42::load(data);
        }
        // Raw image
        if FloppyType::iter().any(|t| t.get_logical_size() == data.len()) {
            return RawImage::load(data);
        }

        bail!("Unsupported image file type");
    }
}
