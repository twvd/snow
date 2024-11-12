//! A file format that contains mostly 0 and 1 to describe physical track data, with headers for each tracks.
//!
//! The format is, for each track:
//! u32 track_len
//! u32 track_len
//! u8[track_len] track data, 0 or 1 for each bit
//!
//! Supports Mac400K, Mac800k. Mostly for development and testing.

use super::{FloppyImageLoader, FloppyImageSaver};
use crate::{Floppy, FloppyImage, FloppyType, TrackLength};

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
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let tracks = Self::count_tracks(data)?;
        let mut image = FloppyImage::new_empty(
            match tracks {
                80 => FloppyType::Mac400K,
                160 => FloppyType::Mac800K,
                _ => bail!("Invalid amount of tracks: {}", tracks),
            },
            filename.unwrap_or_default(),
        );

        let mut offset = 0;
        for tracknum in 0..tracks {
            let bytes = u32::from_le_bytes(data[offset..(offset + 4)].try_into().unwrap()) as usize;
            let bits =
                u32::from_le_bytes(data[(offset + 4)..(offset + 8)].try_into().unwrap()) as usize;
            if bits != bytes {
                bail!("Length fields not equal");
            }
            image.set_actual_track_length(tracknum / 80, tracknum % 80, bits);
            offset += 8;
            for p in 0..bytes {
                image.set_track_bit(
                    tracknum / 80,
                    tracknum % 80,
                    p,
                    match data[offset] {
                        0 => false,
                        1 => true,
                        _ => bail!("Invalid bit value at offset {}: {}", offset, data[offset]),
                    },
                );
                offset += 1;
            }
        }
        Ok(image)
    }
}

impl FloppyImageSaver for Bitfile {
    fn write(img: &FloppyImage, w: &mut impl std::io::Write) -> Result<()> {
        for side in 0..img.get_side_count() {
            for track in 0..img.get_track_count() {
                let TrackLength::Bits(tracklen) = img.get_track_length(side, track) else {
                    unreachable!()
                };

                w.write_all(&(tracklen as u32).to_le_bytes())?;
                w.write_all(&(tracklen as u32).to_le_bytes())?;
                for p in 0..tracklen {
                    w.write_all(if img.get_track_bit(side, track, p) {
                        &[1_u8]
                    } else {
                        &[0_u8]
                    })?;
                }
            }
        }

        Ok(())
    }
}
