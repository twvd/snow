//! PCE PRI format
//! Bitstream format
//! http://www.hampa.ch/pce/index.html

use std::collections::HashMap;
use std::io::{Seek, SeekFrom};

use super::FloppyImageLoader;
use crate::{Floppy, FloppyImage, FloppyType, OriginalTrackType, TrackLength};

use anyhow::{bail, Result};
use binrw::io::Cursor;
use binrw::{binrw, BinRead};
use log::*;

const CRC_PRI: crc::Algorithm<u32> = crc::Algorithm {
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
    pub length: u32,
    pub clock: u32,
}

/// PCE PRI image format loader
pub struct PRI {}

impl FloppyImageLoader for PRI {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);

        let mut tracks: HashMap<(usize, usize), (usize, &[u8])> = HashMap::new();
        let mut cur_track = 0;
        let mut cur_side = 0;
        let mut cur_len = 0;
        let mut saw_header = false;

        // Parse chunks from file
        while let Ok(chunk) = ChunkHeader::read(&mut cursor) {
            let startpos = cursor.position();

            // Check CRC of the entire chunk
            let checksum = crc::Crc::<u32>::new(&CRC_PRI).checksum(
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

            if &chunk.id != b"PRI " && !saw_header {
                bail!("File header not found");
            }

            match &chunk.id {
                b"PRI " => {
                    saw_header = true;
                }
                b"TRAK" => {
                    let payload = PayloadTrack::read(&mut cursor)?;

                    if payload.clock != 500_000 {
                        bail!(
                            "Unsupported clock rate {} on side {} track {}",
                            payload.clock,
                            cur_side,
                            cur_track
                        );
                    }
                    cur_track = payload.track as usize;
                    cur_side = payload.head as usize;
                    cur_len = payload.length as usize;
                }
                b"TEXT" => (),
                b"DATA" => {
                    tracks.insert(
                        (cur_side, cur_track),
                        (
                            cur_len,
                            &data[(startpos as usize + 8)
                                ..(startpos as usize + chunk.size as usize)],
                        ),
                    );
                }
                b"BCLK" => {
                    bail!(
                        "BCLK encountered on side {}, track {}. Currently unsupported",
                        cur_side,
                        cur_track
                    );
                }
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
        for ((side, track), (bitlen, data)) in tracks {
            if let TrackLength::Bits(t) = img.get_track_length(side, track) {
                if t > 0 {
                    // Multiple captures encountered, we just use the first and hope it's good.
                    continue;
                }
            }
            img.origtracktype[side][track] = OriginalTrackType::Bitstream;
            img.set_actual_track_length(side, track, bitlen);

            // The DATA chunk may be shorter than the track length in the
            // preceding TRAK chunk suggests. In that case the remainder of
            // the track data should be set to 0.
            img.trackdata[side][track][0..data.len()].copy_from_slice(data);
        }

        Ok(img)
    }
}
