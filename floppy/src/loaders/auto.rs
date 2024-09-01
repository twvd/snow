//! Auto-detect image file type and load

use crate::{
    loaders::{Bitfile, FloppyImageLoader, Moof},
    FloppyImage,
};

use anyhow::{bail, Result};

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

        bail!("Unsupported image file type");
    }
}
