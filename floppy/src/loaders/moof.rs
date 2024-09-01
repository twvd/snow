//! Applesauce MOOF file format loader
//! https://applesaucefdc.com/moof-reference/

use super::FloppyImageLoader;
use crate::{Floppy, FloppyImage, FloppyType};

use anyhow::{bail, Context, Result};
use log::*;

pub struct Moof {}
impl Moof {
    const HEADER_LEN: usize = 12;
}

impl FloppyImageLoader for Moof {
    fn load(data: &[u8]) -> Result<FloppyImage> {
        if data[0..8] != *b"MOOF\xFF\n\r\n" {
            bail!("Not a MOOF file");
        }

        // TODO checksum

        let mut image = None;
        let mut trackmap = None;

        let mut offset = Self::HEADER_LEN;
        while offset < data.len() {
            let chunk_id = &data[offset..offset + 4];
            let chunk_len = u32::from_le_bytes(data[offset + 4..offset + 8].try_into()?) as usize;
            let chunk = &data[offset + 8..offset + 8 + chunk_len];
            offset += 8 + chunk_len;

            match chunk_id {
                b"INFO" => {
                    image = Some(FloppyImage::new(match chunk[1] {
                        // 1 = SSDD GCR (400K)
                        1 => FloppyType::Mac400K,
                        // 2 = DSDD GCR (800K)
                        2 => FloppyType::Mac800K,
                        // 3 = DSHD MFM (1.44M)
                        // 4 = Twiggy
                        _ => bail!("Unsupported disk type: {:02X}", data[1]),
                    }));
                }
                b"TMAP" => {
                    trackmap = Some(&chunk[0..160]);
                }
                b"FLUX" | b"META" => {
                    // Ignore
                }
                b"TRKS" => {
                    let img = image.as_mut().context("TRKS section before INFO section")?;
                    let tmap = trackmap
                        .as_ref()
                        .context("TRKS section before TMAP section")?;

                    for track in 0..80 {
                        for side in 0..2 {
                            let entry = tmap[track * 2 + side] as usize;
                            if entry == 255 {
                                continue;
                            }
                            let trk = &chunk[entry * 8..(entry + 1) * 8];

                            let start_block = u16::from_le_bytes(trk[0..2].try_into()?) as usize;
                            let block_count = u16::from_le_bytes(trk[2..4].try_into()?) as usize;
                            let bit_count = u32::from_le_bytes(trk[4..8].try_into()?) as usize;
                            img.set_actual_track_length(side, track, bit_count);

                            let block = &data[start_block * 512..(start_block + block_count) * 512];
                            for blockbit in 0..bit_count {
                                let byte = blockbit / 8;
                                let bit = 7 - blockbit % 8;
                                img.set_track_bit(
                                    side,
                                    track,
                                    blockbit,
                                    block[byte] & (1 << bit) != 0,
                                );
                            }
                        }
                    }
                }
                _ => {
                    warn!("Unknown chunk ID: {:?}", chunk_id);
                }
            }
        }

        image.context("No chunks in image")
    }
}
