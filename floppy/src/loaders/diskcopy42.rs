//! Apple DiskCopy 4.2 format
//! Sector-based image format
//! https://www.discferret.com/wiki/Apple_DiskCopy_4.2

#[cfg(feature = "fluxfox")]
use super::fluxfox::Fluxfox;
use super::FloppyImageLoader;
use crate::macformat::MacFormatEncoder;
use crate::{FloppyImage, FloppyType};

use anyhow::{bail, Result};
use binrw::io::Cursor;
use binrw::{binrw, BinRead};

#[binrw]
#[derive(Debug, Clone, Copy)]
enum Dc42Encoding {
    /// 00 = GCR CLV ssdd (400k)
    #[brw(magic = 0u8)]
    GcrClvSsDd,
    /// 01 = GCR CLV dsdd (800k)
    #[brw(magic = 1u8)]
    GcrClvDsDd,
    /// 02 = MFM CAV dsdd (720k)
    #[brw(magic = 2u8)]
    MfmCavDsDd,
    /// 03 = MFM CAV dshd (1440k)
    #[brw(magic = 3u8)]
    MfmCavDsHd,
}

#[binrw]
#[derive(Debug, Clone, Copy)]
enum Dc42Format {
    /// $02 = Mac 400k
    #[brw(magic = 0x02u8)]
    Mac400K,
    /// $12 = Lisa 400k (observed, documentation error claims this is for mac 400k disks, but this is wrong)
    #[brw(magic = 0x12u8)]
    Lisa400K,
    /// $22 = Disk formatted as Mac 800k
    #[brw(magic = 0x22u8)]
    Mac800K,
    /// $24 = Disk formatted as Prodos 800k (AppleIIgs format)
    #[brw(magic = 0x24u8)]
    Prodos800K,
    /// $96 = INVALID
    #[brw(magic = 0x96u8)]
    Invalid,
}

#[binrw]
#[brw(big)]
#[derive(Debug)]
struct Dc42Raw {
    #[bw(calc = name.len() as u8)]
    pub name_len: u8,
    #[br(args { count: name_len as usize }, pad_size_to = 63, map = |s: Vec<u8>| String::from_utf8_lossy(&s).to_string())]
    #[bw(map = |s: &String| s.as_bytes().to_vec(), pad_size_to = 63)]
    pub name: String,
    #[bw(calc = data.len() as u32)]
    pub data_size: u32,
    #[bw(calc = tags.len() as u32)]
    pub tag_size: u32,
    pub data_crc: u32,
    pub tag_crc: u32,
    pub encoding: Dc42Encoding,
    pub format: Dc42Format,
    #[brw(magic = 0x0100u16)]
    #[br(args { count: data_size as usize })]
    pub data: Vec<u8>,
    #[br(args { count: tag_size as usize })]
    pub tags: Vec<u8>,
}

impl Dc42Raw {
    pub fn get_type(&self) -> Result<FloppyType> {
        match (self.encoding, self.format) {
            (Dc42Encoding::GcrClvSsDd, Dc42Format::Mac400K) => Ok(FloppyType::Mac400K),
            (Dc42Encoding::GcrClvSsDd, Dc42Format::Lisa400K) => Ok(FloppyType::Mac400K),
            (Dc42Encoding::GcrClvDsDd, Dc42Format::Mac800K) => Ok(FloppyType::Mac800K),
            (Dc42Encoding::GcrClvDsDd, Dc42Format::Prodos800K) => Ok(FloppyType::Mac800K),
            (Dc42Encoding::MfmCavDsHd, _) => Ok(FloppyType::Mfm144M),
            _ => bail!(
                "Unknown type, encoding: {:?} format: {:?}",
                self.encoding,
                self.format
            ),
        }
    }
}

/// Apple DiskCopy 4.2 image loader
pub struct Diskcopy42 {}

impl FloppyImageLoader for Diskcopy42 {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);
        let raw = Dc42Raw::read(&mut cursor)?;
        let title = if raw.name.is_empty() {
            filename.unwrap_or_default()
        } else {
            &raw.name
        };

        let floppytype = raw.get_type()?;
        if raw.data.len() != floppytype.get_logical_size() {
            bail!(
                "Image data length is {}, but expected {}",
                raw.data.len(),
                floppytype.get_logical_size()
            );
        }

        if raw.get_type()? == FloppyType::Mfm144M {
            #[cfg(feature = "fluxfox")]
            {
                // Hand-off to Fluxfox
                Fluxfox::load(&raw.data, filename)
            }
            #[cfg(not(feature = "fluxfox"))]
            {
                bail!("Requires fluxfox feature");
            }
        } else if raw.tags.is_empty() {
            MacFormatEncoder::encode(floppytype, &raw.data, None, title)
        } else {
            MacFormatEncoder::encode(floppytype, &raw.data, Some(&raw.tags), title)
        }
    }
}
