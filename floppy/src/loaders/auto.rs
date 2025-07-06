//! Auto-detect image file type and load

#[cfg(feature = "fluxfox")]
use crate::loaders::fluxfox::Fluxfox;
use crate::loaders::{
    A2Rv2, A2Rv3, Bitfile, Diskcopy42, FloppyImageLoader, Moof, RawImage, PFI, PRI,
};
use crate::{FloppyImage, FloppyType};

use anyhow::{bail, Result};
use strum::{Display, IntoEnumIterator};

/// Types of supported floppy images
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Display, Copy, Clone)]
pub enum ImageType {
    A2R2,
    A2R3,
    MOOF,
    Bitfile,
    DC42,
    Fluxfox,
    PFI,
    PRI,
    Raw,
}

impl ImageType {
    pub const EXTENSIONS: [&'static str; 9] = [
        "a2r", "moof", "dc42", "dsk", "pfi", "pri", "raw", "img", "image",
    ];

    pub fn as_friendly_str(&self) -> &'static str {
        match self {
            Self::A2R2 => "Applesauce A2R v2.x",
            Self::A2R3 => "Applesauce A2R v3.x",
            Self::MOOF => "Applesauce MOOF",
            Self::Bitfile => "Bitfile",
            Self::DC42 => "Apple DiskCopy 4.2",
            Self::Fluxfox => "Fluxfox",
            Self::PFI => "PCE PFI",
            Self::PRI => "PCE PRI",
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
        // A2R v2
        if data.len() >= 8 && data[0..8] == *b"A2R2\xFF\n\r\n" {
            return Ok(ImageType::A2R2);
        }
        // A2R v3
        if data.len() >= 8 && data[0..8] == *b"A2R3\xFF\n\r\n" {
            return Ok(ImageType::A2R3);
        }
        // PFI
        if data.len() >= 4 && data[0..4] == *b"PFI " {
            return Ok(ImageType::PFI);
        }
        // PFI
        if data.len() >= 4 && data[0..4] == *b"PRI " {
            return Ok(ImageType::PRI);
        }
        // Apple DiskCopy 4.2
        if data.len() > 0x53 && data[0x52..=0x53] == [0x01, 0x00] {
            return Ok(ImageType::DC42);
        }
        // Raw image
        if FloppyType::iter().any(|t| t.get_logical_size() == data.len()) {
            return Ok(ImageType::Raw);
        }
        // Bitfile / 'Dave format'
        if data.len() >= 10
            && data[0..4] == data[4..8]
            && [0, 1].contains(&data[8])
            && [0, 1].contains(&data[9])
        {
            return Ok(ImageType::Bitfile);
        }

        #[cfg(feature = "fluxfox")]
        if Fluxfox::detect(data) {
            return Ok(ImageType::Fluxfox);
        }

        bail!("Unsupported image file type");
    }
}

impl FloppyImageLoader for Autodetect {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        match Self::detect(data)? {
            ImageType::A2R2 => A2Rv2::load(data, filename),
            ImageType::A2R3 => A2Rv3::load(data, filename),
            ImageType::MOOF => Moof::load(data, filename),
            ImageType::Bitfile => Bitfile::load(data, filename),
            ImageType::DC42 => Diskcopy42::load(data, filename),
            ImageType::PFI => PFI::load(data, filename),
            ImageType::PRI => PRI::load(data, filename),
            ImageType::Raw => RawImage::load(data, filename),
            ImageType::Fluxfox => {
                #[cfg(feature = "fluxfox")]
                {
                    Fluxfox::load(data, filename)
                }
                #[cfg(not(feature = "fluxfox"))]
                {
                    unreachable!()
                }
            }
        }
    }
}
