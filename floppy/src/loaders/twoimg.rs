//! Apple II 2IMG ('.2mg') format
//! A 64-byte header in front of a sector image. Only the ProDOS order 3.5"
//! disk sizes make sense in a Macintosh drive; the data is then a raw image.
//! https://gswv.apple2.org.za/a2zine/Docs/DiskImage_2MG_Info.txt

use super::{FloppyImageLoader, RawImage};
use crate::FloppyImage;

use anyhow::{Result, bail};

/// Image format field: the data is in ProDOS block order
const FORMAT_PRODOS: u32 = 1;
/// Flags field: the disk is write-protected
const FLAG_LOCKED: u32 = 1 << 31;

pub struct TwoImg {}

impl TwoImg {
    pub fn detect(data: &[u8]) -> bool {
        data.len() >= 64 && data[0..4] == *b"2IMG"
    }
}

fn le32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

impl FloppyImageLoader for TwoImg {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        if !Self::detect(data) {
            bail!("Not a 2IMG image");
        }
        let format = le32(data, 0x0C);
        let flags = le32(data, 0x10);
        let offset = le32(data, 0x18) as usize;
        let len = le32(data, 0x1C) as usize;
        if format != FORMAT_PRODOS {
            bail!("Unsupported 2IMG image format {}", format);
        }
        let Some(blocks) = data.get(offset..offset + len) else {
            bail!("2IMG data out of bounds");
        };

        let mut img = RawImage::load(blocks, filename)?;
        if flags & FLAG_LOCKED != 0 {
            img.set_force_wp();
        }
        Ok(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Floppy;

    fn image(format: u32, flags: u32, len: usize) -> Vec<u8> {
        let mut v = vec![0; 64 + len];
        v[0..4].copy_from_slice(b"2IMG");
        v[0x08..0x0A].copy_from_slice(&64u16.to_le_bytes());
        v[0x0C..0x10].copy_from_slice(&format.to_le_bytes());
        v[0x10..0x14].copy_from_slice(&flags.to_le_bytes());
        v[0x14..0x18].copy_from_slice(&((len / 512) as u32).to_le_bytes());
        v[0x18..0x1C].copy_from_slice(&64u32.to_le_bytes());
        v[0x1C..0x20].copy_from_slice(&(len as u32).to_le_bytes());
        v
    }

    #[test]
    fn prodos_800k() {
        let img = TwoImg::load(&image(FORMAT_PRODOS, 0, 819200), None).unwrap();
        assert!(!img.get_write_protect());
    }

    #[test]
    fn locked() {
        let img = TwoImg::load(&image(FORMAT_PRODOS, FLAG_LOCKED, 819200), None).unwrap();
        assert!(img.get_write_protect());
    }

    #[test]
    fn dos_order_rejected() {
        assert!(TwoImg::load(&image(0, 0, 819200), None).is_err());
    }

    #[test]
    fn short_data_rejected() {
        let mut v = image(FORMAT_PRODOS, 0, 819200);
        v.truncate(1000);
        assert!(TwoImg::load(&v, None).is_err());
    }
}
