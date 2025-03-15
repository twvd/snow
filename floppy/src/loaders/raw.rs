//! Raw, sector-based image format

use super::FloppyImageLoader;
#[cfg(feature = "fluxfox")]
use crate::loaders::fluxfox::Fluxfox;
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

        if floppytype == FloppyType::Mfm144M {
            #[cfg(feature = "fluxfox")]
            {
                // Hand-off to Fluxfox
                Fluxfox::load(data, filename)
            }
            #[cfg(not(feature = "fluxfox"))]
            {
                bail!("Requires fluxfox feature");
            }
        } else {
            MacFormatEncoder::encode(floppytype, data, None, filename.unwrap_or_default())
        }
    }
}
