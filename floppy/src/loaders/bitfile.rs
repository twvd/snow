//! A file format that contains mostly 0 and 1 to describe physical track data, with headers for each tracks.
//!
//! The format is, for each track:
//! u32 track_len
//! u32 track_len
//! u8[track_len] track data, 0 or 1 for each bit
//!
//! Supports Mac400K, Mac800k. Mostly for development and testing.

use super::{FloppyImageLoader, FloppyImageSaver};
use crate::{Floppy, FloppyImage, FloppyType};

use anyhow::{bail, Result};

pub struct Bitfile {}

impl Bitfile {
    fn count_tracks(data: &[u8]) -> Result<usize> {
        let mut offset = 0;
        let mut last = 0;

        // Determine amount of tracks
        for tracknum in 0.. {
            let bytes = u32::from_le_bytes(data[offset..(offset + 4)].try_into().unwrap()) as usize;
            let bits =
                u32::from_le_bytes(data[(offset + 4)..(offset + 8)].try_into().unwrap()) as usize;
            if bits != bytes {
                bail!("Length fields not equal");
            }
            offset += 8 + bytes;

            if data.len() <= offset {
                last = tracknum;
                break;
            }
        }
        Ok(last + 1)
    }
}

impl FloppyImageLoader for Bitfile {
    fn load(data: &[u8]) -> Result<FloppyImage> {
        let tracks = Self::count_tracks(data)?;
        let mut image = FloppyImage::new_empty(
            match tracks {
                80 => FloppyType::Mac400K,
                160 => FloppyType::Mac800K,
                _ => bail!("Invalid amount of tracks: {}", tracks),
            },
            "",
        );

        let mut offset = 0;
        for tracknum in 0..tracks {
            let mut zeroes = 0;
            let bytes = u32::from_le_bytes(data[offset..(offset + 4)].try_into().unwrap()) as usize;
            let bits =
                u32::from_le_bytes(data[(offset + 4)..(offset + 8)].try_into().unwrap()) as usize;
            if bits != bytes {
                bail!("Length fields not equal");
            }
            image.set_actual_track_length(tracknum / 80, tracknum % 80, bits);
            offset += 8;
            for _ in 0..bytes {
                match data[offset] {
                    0 => zeroes += 1,
                    1 => image.push(tracknum / 80, tracknum % 80, (zeroes + 1) * 16),
                    _ => bail!("Invalid bit value at offset {}: {}", offset, data[offset]),
                };
                offset += 1;
            }
            if zeroes > 0 {
                image.stitch(tracknum / 80, tracknum % 80, zeroes * 16);
            }
        }
        Ok(image)
    }
}

impl FloppyImageSaver for Bitfile {
    fn write(img: &FloppyImage, w: &mut impl std::io::Write) -> Result<()> {
        for side in 0..img.get_side_count() {
            for track in 0..img.get_track_count() {
                w.write_all(&(img.get_track_length(side, track) as u32).to_le_bytes())?;
                w.write_all(&(img.get_track_length(side, track) as u32).to_le_bytes())?;
                for p in 0..img.get_track_length(side, track) {
                    todo!()
                    //w.write_all(if img.get_track_bit(side, track, p) {
                    //    &[1_u8]
                    //} else {
                    //    &[0_u8]
                    //})?;
                }
            }
        }

        Ok(())
    }
}
