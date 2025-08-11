mod a2r2;
mod a2r3;
mod auto;
mod bitfile;
mod dart;
mod diskcopy42;

#[cfg(feature = "fluxfox")]
mod fluxfox;

mod moof;
mod pfi;
mod pri;
mod raw;

use std::path::Path;

pub use a2r2::A2Rv2;
pub use a2r3::A2Rv3;
pub use auto::Autodetect;
pub use auto::ImageType;
pub use bitfile::Bitfile;
pub use dart::Dart;
pub use diskcopy42::Diskcopy42;
pub use moof::Moof;
pub use pfi::PFI;
pub use pri::PRI;
pub use raw::RawImage;

use crate::FloppyImage;

use anyhow::Result;

/// A loader to read a specific format and transform it into a usable FloppyImage
pub trait FloppyImageLoader {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage>;

    fn load_file(filename: &str) -> Result<FloppyImage> {
        Self::load(
            &std::fs::read(filename)?,
            Path::new(filename).file_name().and_then(|s| s.to_str()),
        )
    }
}

/// A saver to write a specific format from a FloppyImage
/// Not every loader needs to implement a saver
pub trait FloppyImageSaver {
    fn write(img: &FloppyImage, w: &mut impl std::io::Write) -> Result<()>;

    fn save_vec(img: &FloppyImage) -> Result<Vec<u8>> {
        let mut v = vec![];
        Self::write(img, &mut v)?;
        Ok(v)
    }

    fn save_file(img: &FloppyImage, filename: &str) -> Result<()> {
        let mut f = std::fs::File::create(filename)?;
        Self::write(img, &mut f)?;
        Ok(())
    }
}
