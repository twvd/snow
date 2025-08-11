//! Auto-detect image file type and load

#[cfg(feature = "fluxfox")]
use crate::loaders::fluxfox::Fluxfox;
use crate::loaders::{
    A2Rv2, A2Rv3, Bitfile, Dart, Diskcopy42, FloppyImageLoader, Moof, RawImage, PFI, PRI,
};
use crate::{FloppyImage, FloppyType};

use anyhow::{bail, Result};
use strum::{Display, IntoEnumIterator};

/// Types of supported floppy images
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Display, Copy, Clone)]
pub enum ImageType {
    A2R2,
    A2R3,
    Bitfile,
    DART,
    DC42,
    Fluxfox,
    MOOF,
    PFI,
    PRI,
    Raw,
}

impl ImageType {
    pub const EXTENSIONS: [&'static str; 10] = [
        "a2r", "moof", "dart", "dc42", "dsk", "pfi", "pri", "raw", "img", "image",
    ];

    pub fn as_friendly_str(&self) -> &'static str {
        match self {
            Self::A2R2 => "Applesauce A2R v2.x",
            Self::A2R3 => "Applesauce A2R v3.x",
            Self::Bitfile => "Bitfile",
            Self::DART => "Apple DART",
            Self::DC42 => "Apple DiskCopy 4.2",
            Self::Fluxfox => "Fluxfox",
            Self::MOOF => "Applesauce MOOF",
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
        // Apple DART
        if data.len() > 0x54
            && [0u8, 1u8, 2u8].contains(&data[0])
            && [1u8, 2, 3, 16, 17, 18].contains(&data[1])
            && [400u16, 800, 1440].contains(&u16::from_be_bytes(data[2..=3].try_into()?))
        {
            return Ok(ImageType::DART);
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
            ImageType::Bitfile => Bitfile::load(data, filename),
            ImageType::DART => Dart::load(data, filename),
            ImageType::DC42 => Diskcopy42::load(data, filename),
            ImageType::MOOF => Moof::load(data, filename),
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
