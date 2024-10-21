mod auto;
mod bitfile;
mod diskcopy42;
mod moof;
mod raw;

pub use auto::Autodetect;
pub use bitfile::Bitfile;
pub use diskcopy42::Diskcopy42;
pub use moof::Moof;
pub use raw::RawImage;

use crate::FloppyImage;

use anyhow::Result;

/// A loader to read a specific format and transform it into a usable FloppyImage
pub trait FloppyImageLoader {
    fn load(data: &[u8]) -> Result<FloppyImage>;

    fn load_file(filename: &str) -> Result<FloppyImage> {
        Self::load(&std::fs::read(filename)?)
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
