//! Raw, sector-based image format

use super::FloppyImageLoader;
use crate::macformat::MacFormatEncoder;
use crate::FloppyType;

use anyhow::bail;
use strum::IntoEnumIterator;

/// Raw image loader
pub struct RawImage {}

impl FloppyImageLoader for RawImage {
    fn load(data: &[u8]) -> anyhow::Result<crate::FloppyImage> {
        let Some(floppytype) = FloppyType::iter().find(|t| t.get_logical_size() == data.len())
        else {
            bail!("Invalid raw image length: {}", data.len())
        };

        MacFormatEncoder::encode(floppytype, data, None, "")
    }
}
