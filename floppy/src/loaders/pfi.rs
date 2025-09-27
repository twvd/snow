//! PCE PFI format
//! Flux format
//! http://www.hampa.ch/pce/index.html

use std::collections::HashMap;
use std::io::{Seek, SeekFrom};

use super::FloppyImageLoader;
use crate::{Floppy, FloppyImage, FloppyType, OriginalTrackType, TrackLength};

use anyhow::{bail, Result};
use binrw::io::Cursor;
use binrw::{binrw, BinRead};
use log::*;

const CRC_PFI: crc::Algorithm<u32> = crc::Algorithm {
    width: 32,
    poly: 0x1edc6f41,
    init: 0,
    refin: false,
    refout: false,
    xorout: 0,
    check: 0,
    residue: 0,
};

/// Standardized chunk header
#[binrw]
#[brw(big)]
#[derive(Debug)]
struct ChunkHeader {
    /// ASCII chunk identifier
    pub id: [u8; 4],

    /// Chunk size in bytes
    pub size: u32,
}

/// Standardized chunk tail
#[binrw]
#[brw(big)]
#[derive(Debug)]
#[allow(dead_code)]
struct ChunkTail {
    pub crc: u32,
}

/// File header chunk payload
#[binrw]
#[brw(big)]
#[derive(Debug)]
#[allow(dead_code)]
struct PayloadHeader {
    pub version: u8,
}

/// Track header
#[binrw]
#[brw(big)]
#[derive(Debug)]
struct PayloadTrack {
    pub track: u32,
    pub head: u32,
    pub clock: u32,
}

/// PCE PFI image format loader
pub struct PFI {}

impl FloppyImageLoader for PFI {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);

        let mut tracks: HashMap<(usize, usize), &[u8]> = HashMap::new();
        let mut cur_track = 0;
        let mut cur_side = 0;
        let mut saw_header = false;

        // Parse chunks from file
        while let Ok(chunk) = ChunkHeader::read(&mut cursor) {
            let startpos = cursor.position();

            // Check CRC of the entire chunk
            let checksum = crc::Crc::<u32>::new(&CRC_PFI).checksum(
                &data[(startpos as usize - 8)..(startpos as usize + chunk.size as usize)],
            );
            let chunk_checksum = u32::from_be_bytes(
                data[(startpos as usize + chunk.size as usize)
                    ..(startpos as usize + chunk.size as usize + 4)]
                    .try_into()?,
            );
            if checksum != chunk_checksum {
                bail!(
                    "Checksum for chunk '{}' incorrect, saw {:08X}, expected {:08X}",
                    String::from_utf8_lossy(&chunk.id),
                    chunk_checksum,
                    checksum
                );
            }

            if &chunk.id != b"PFI " && !saw_header {
                bail!("File header not found");
            }

            match &chunk.id {
                b"PFI " => {
                    saw_header = true;
                }
                b"TRAK" => {
                    let payload = PayloadTrack::read(&mut cursor)?;

                    if payload.clock != 8_000_000 {
                        bail!(
                            "Unsupported clock rate {} on side {} track {}",
                            payload.clock,
                            cur_side,
                            cur_track
                        );
                    }
                    cur_track = payload.track as usize;
                    cur_side = payload.head as usize;
                }
                b"TEXT" => (),
                b"DATA" => {
                    tracks.insert(
                        (cur_side, cur_track),
                        &data[(startpos as usize + 8)..(startpos as usize + chunk.size as usize)],
                    );
                }
                b"INDX" => (),
                b"END " => break,
                _ => {
                    warn!(
                        "Found unsupported chunk '{}', skipping",
                        String::from_utf8_lossy(&chunk.id)
                    );
                }
            }

            // Always consume the amount of bytes the chunk header reports
            cursor.seek(SeekFrom::Start(startpos + u64::from(chunk.size) + 4))?;
        }

        let mut img = FloppyImage::new_empty(
            if tracks.keys().any(|&(s, _t)| s > 0) {
                FloppyType::Mac800K
            } else {
                FloppyType::Mac400K
            },
            filename.unwrap_or_default(),
        );

        // Fill tracks
        for ((side, track), data) in tracks {
            if let TrackLength::Transitions(t) = img.get_track_length(side, track) {
                if t > 0 {
                    // Multiple captures encountered, we just use the first and hope it's good.
                    continue;
                }
            }
            img.origtracktype[side][track] = OriginalTrackType::RawFlux;

            let mut p = 0;
            while p < data.len() {
                match data[p] {
                    0 => {
                        bail!(
                            "Invalid 00 in side {}, track {}, position {}",
                            side,
                            track,
                            p
                        );
                    }
                    1 => {
                        img.push_flux(
                            side,
                            track,
                            i16::from_be_bytes(data[p..(p + 2)].try_into()?),
                        );
                        p += 2;
                    }
                    2 | 3 => {
                        bail!(
                            "Transition too long: side {}, track {}, position {}",
                            side,
                            track,
                            p
                        );
                    }
                    4..=7 => {
                        let d = [data[p] - 4, data[p + 1]];
                        img.push_flux(side, track, i16::from_be_bytes(d));
                        p += 1;
                    }
                    _ => {
                        img.push_flux(side, track, data[p] as i16);
                    }
                }
                p += 1;
            }
        }

        Ok(img)
    }
}
