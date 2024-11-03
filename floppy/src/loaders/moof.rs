//! Applesauce MOOF file format
//! Combined bitstream and flux image format
//! https://applesaucefdc.com/moof-reference/

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use super::FloppyImageLoader;
use crate::{Floppy, FloppyImage, FloppyType, OriginalTrackType};

use anyhow::{bail, Context, Result};
use binrw::io::Cursor;
use binrw::{binrw, BinRead};
use log::*;

/// Initial MOOF file header
#[binrw]
#[brw(little, magic = b"MOOF\xFF\n\r\n")]
struct MoofHeader {
    /// CRC32 of entire file
    pub crc: u32,
}

/// Standardized chunk header
#[binrw]
#[brw(little)]
#[derive(Debug)]
struct MoofChunkHeader {
    /// ASCII chunk identifier
    pub id: [u8; 4],

    /// Chunk size in bytes
    pub size: u32,
}

/// Disk type
#[binrw]
#[derive(Debug, Copy, Clone)]
enum MoofDiskType {
    /// SSDD GCR (400K)
    #[brw(magic = 1u8)]
    SSDDGCR400k,
    /// DSDD GCR (800K)
    #[brw(magic = 2u8)]
    DSDDGCR800k,
    /// DSHD MFM (1.44MB)
    #[brw(magic = 3u8)]
    DSHDMFM144Mb,
    /// Twiggy
    #[brw(magic = 4u8)]
    Twiggy,
}

impl std::convert::TryFrom<MoofDiskType> for FloppyType {
    type Error = anyhow::Error;

    fn try_from(value: MoofDiskType) -> std::result::Result<Self, Self::Error> {
        match value {
            MoofDiskType::SSDDGCR400k => Ok(Self::Mac400K),
            MoofDiskType::DSDDGCR800k => Ok(Self::Mac800K),
            _ => bail!("Unsupported MOOF disk type {:?}", value),
        }
    }
}

/// INFO chunk (minus header)
#[binrw]
#[brw(little)]
#[derive(Debug)]
struct MoofChunkInfo {
    pub version: u8,
    pub disktype: MoofDiskType,
    pub writeprotect: u8,
    pub synchronized: u8,
    pub optimal_bit_timing: u8,

    #[br(args { count: 32 }, map = |s: Vec<u8>| String::from_utf8_lossy(&s).to_string())]
    #[bw(map = |s: &String| s.as_bytes().to_vec(), pad_size_to = 32)]
    pub creator: String,

    pub zero: u8,
    pub largest_track: u16,
    pub flux_block: u16,
    pub flux_longest_track: u16,
}

/// TMAP/FLUX chunk (minus header)
#[binrw]
#[brw(little)]
struct MoofChunkTmap {
    pub tracks: [[u8; 2]; 80],
}

/// FLUX chunk (minus header)
#[binrw]
#[brw(little)]
struct MoofChunkFlux {
    pub entries: [[u8; 2]; 80],
}

/// TRKS chunk (minus header)
#[binrw]
#[brw(little)]
struct MoofChunkTrks {
    pub entries: [MoofChunkTrksEntry; 160],
    // BITS not relevant, will read those straight from the file as
    // the offsets are absolute anyway.
}

#[binrw]
#[brw(little)]
struct MoofChunkTrksEntry {
    pub start_blk: u16,
    pub blocks: u16,

    /// Bits for bitstream track, bytes for flux track
    pub bits_bytes: u32,
}

impl MoofChunkTrksEntry {
    pub fn data_range(&self) -> std::ops::Range<usize> {
        (self.start_blk as usize * 512)..((self.start_blk as usize + self.blocks as usize) * 512)
    }

    pub fn flux_range(&self) -> std::ops::Range<usize> {
        (self.start_blk as usize * 512)
            ..((self.start_blk as usize * 512) + self.bits_bytes as usize)
    }

    pub fn bit_range(&self) -> std::ops::Range<usize> {
        0..(self.bits_bytes as usize)
    }
}

/// Applesauce MOOF image file loader
pub struct Moof {}

impl Moof {
    fn parse_meta(meta: &str) -> HashMap<&str, &str> {
        let mut result = HashMap::new();

        for l in meta.lines() {
            let Some((k, v)) = l.split_once('\t') else {
                continue;
            };
            result.insert(k, v);
        }

        result
    }
}

impl FloppyImageLoader for Moof {
    fn load(data: &[u8]) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);
        let _header = MoofHeader::read(&mut cursor)?;
        // TODO checksum

        let mut info = None;
        let mut tmap = None;
        let mut trks = None;
        let mut flux = None;
        let mut meta = String::new();

        // Parse chunks from file
        while let Ok(chunk) = MoofChunkHeader::read(&mut cursor) {
            let startpos = cursor.position();

            match &chunk.id {
                b"INFO" => info = Some(MoofChunkInfo::read(&mut cursor)?),
                b"TMAP" => tmap = Some(MoofChunkTmap::read(&mut cursor)?),
                b"FLUX" => flux = Some(MoofChunkTmap::read(&mut cursor)?),
                b"TRKS" => trks = Some(MoofChunkTrks::read(&mut cursor)?),
                b"META" => {
                    let mut metaraw = vec![0u8; chunk.size as usize];
                    cursor.read_exact(&mut metaraw)?;
                    meta = String::from_utf8_lossy(&metaraw).to_string();
                }
                _ => {
                    warn!(
                        "Found unsupported chunk '{}' in MOOF file, skipping",
                        String::from_utf8_lossy(&chunk.id)
                    );
                }
            }

            // Always consume the amount of bytes the chunk header reports
            cursor.seek(SeekFrom::Start(startpos + u64::from(chunk.size)))?;
        }

        let info = info.context("No INFO chunk in file")?;
        let trks = trks.context("No TRKS chunk in file")?;
        let metadata = Self::parse_meta(&meta);
        let title = metadata.get("title").copied().unwrap_or("?");

        let mut img = FloppyImage::new_empty(
            info.disktype
                .try_into()
                .context(format!("Unsupported disk type: {:?}", info.disktype))?,
            title,
        );

        // Fill metadata
        for (k, v) in metadata {
            img.set_metadata(k, v);
        }

        // Fill in tracks
        for (track, side) in (0..80).flat_map(|t| (0..2).map(move |s| (t, s))) {
            if let Some(ref flux) = flux {
                // Flux track takes precedence
                if flux.tracks[track][side] != 255 {
                    let entry_idx = flux.tracks[track][side] as usize;
                    let trk = &trks.entries[entry_idx];
                    let mut last = 0;

                    for &b in &data[trk.flux_range()] {
                        last += b as i16;
                        if b == 255 {
                            continue;
                        }
                        if last == 0 {
                            warn!("transition of 0!");
                        }
                        img.push(side, track, last);
                        last = 0;
                    }

                    if last > 0 {
                        img.push(side, track, last);
                    }

                    img.origtracktype[side][track] = OriginalTrackType::Flux;
                    continue;
                }
            }
            if let Some(ref tmap) = tmap {
                let entry_idx = tmap.tracks[track][side] as usize;
                if entry_idx == 255 {
                    continue;
                }
                if entry_idx > 160 {
                    bail!("Encountered invalid TRKS entry index {}", entry_idx);
                }

                let trk = &trks.entries[entry_idx];

                // Fill track
                let block = &data[trk.data_range()];
                let mut zeroes = 0;
                for blockbit in trk.bit_range() {
                    let byte = blockbit / 8;
                    let bit = 7 - blockbit % 8;
                    if block[byte] & (1 << bit) != 0 {
                        img.push(side, track, (zeroes + 1) * 16);
                        zeroes = 0;
                    } else {
                        zeroes += 1;
                    }
                }
                if zeroes > 0 {
                    img.stitch(side, track, zeroes * 16);
                }
                img.origtracktype[side][track] = OriginalTrackType::Bitstream;
            }
        }

        Ok(img)
    }
}
