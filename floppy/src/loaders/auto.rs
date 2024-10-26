//! Auto-detect image file type and load

use crate::{
    loaders::{Bitfile, Diskcopy42, FloppyImageLoader, Moof, RawImage},
    FloppyImage, FloppyType,
};

use anyhow::{bail, Result};
use strum::{Display, IntoEnumIterator};

/// Types of supported floppy images
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Display, Copy, Clone)]
pub enum ImageType {
    MOOF,
    Bitfile,
    DC42,
    Raw,
}

impl ImageType {
    pub fn as_friendly_str(&self) -> &'static str {
        match self {
            Self::MOOF => "Applesauce MOOF",
            Self::Bitfile => "Bitfile",
            Self::DC42 => "Apple DiskCopy 4.2",
            Self::Raw => "Raw image",
        }
    }
}

pub struct Autodetect {}

impl Autodetect {
    pub fn detect(data: &[u8]) -> Result<ImageType> {
        // MOOF
        if data.len() >= 8 && data[0..8] == *b"MOOF\xFF\n\r\n" {
            return Ok(ImageType::MOOF);
        }
        // Bitfile / 'Dave format'
        if data.len() >= 10
            && data[0..4] == data[4..8]
            && [0, 1].contains(&data[8])
            && [0, 1].contains(&data[9])
        {
            return Ok(ImageType::Bitfile);
        }
        // Apple DiskCopy 4.2
        if data[0x52..=0x53] == [0x01, 0x00] {
            return Ok(ImageType::DC42);
        }
        // Raw image
        if FloppyType::iter().any(|t| t.get_logical_size() == data.len()) {
            return Ok(ImageType::Raw);
        }

        bail!("Unsupported image file type");
    }
}

impl FloppyImageLoader for Autodetect {
    fn load(data: &[u8]) -> Result<FloppyImage> {
        match Self::detect(data)? {
            ImageType::MOOF => Moof::load(data),
            ImageType::Bitfile => Bitfile::load(data),
            ImageType::DC42 => Diskcopy42::load(data),
            ImageType::Raw => RawImage::load(data),
        }
    }
}
