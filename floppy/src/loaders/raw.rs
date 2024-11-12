//! Raw, sector-based image format

use super::FloppyImageLoader;
use crate::FloppyType;
use crate::{macformat::MacFormatEncoder, FloppyImage};

use anyhow::{bail, Result};
use strum::IntoEnumIterator;

/// Raw image loader
pub struct RawImage {}

impl FloppyImageLoader for RawImage {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let Some(floppytype) = FloppyType::iter().find(|t| t.get_logical_size() == data.len())
        else {
            bail!("Invalid raw image length: {}", data.len())
        };

        MacFormatEncoder::encode(floppytype, data, None, filename.unwrap_or_default())
    }
}
