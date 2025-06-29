//! Applesauce MOOF file format
//! Combined bitstream and flux image format
//! https://applesaucefdc.com/moof-reference/

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

use super::{FloppyImageLoader, FloppyImageSaver};
use crate::{
    built_info, Floppy, FloppyImage, FloppyType, OriginalTrackType, TrackLength, TrackType,
    FLOPPY_MAX_SIDES, FLOPPY_MAX_TRACKS,
};

use anyhow::{bail, Context, Result};
use binrw::io::Cursor;
use binrw::{binrw, BinRead, BinWrite};
use itertools::Itertools;
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

impl TryFrom<MoofDiskType> for FloppyType {
    type Error = anyhow::Error;

    fn try_from(value: MoofDiskType) -> std::result::Result<Self, Self::Error> {
        match value {
            MoofDiskType::SSDDGCR400k => Ok(Self::Mac400K),
            MoofDiskType::DSDDGCR800k => Ok(Self::Mac800K),
            MoofDiskType::DSHDMFM144Mb => Ok(Self::Mfm144M),
            _ => bail!("Unsupported MOOF disk type {:?}", value),
        }
    }
}

impl std::convert::From<FloppyType> for MoofDiskType {
    fn from(value: FloppyType) -> Self {
        match value {
            FloppyType::Mac400K => Self::SSDDGCR400k,
            FloppyType::Mac800K => Self::DSDDGCR800k,
            FloppyType::Mfm144M => Self::DSHDMFM144Mb,
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
    const CHECKSUM: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

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
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);
        let header = MoofHeader::read(&mut cursor)?;
        let checksum = Self::CHECKSUM.checksum(&data[12..]);
        if checksum != header.crc {
            bail!(
                "Checksum verification failed - calculated: {:08X}, file: {:08X}",
                checksum,
                header.crc
            );
        }

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
        let title = metadata
            .get("title")
            .copied()
            .unwrap_or_else(|| filename.unwrap_or_default());

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
                    img.origtracktype[side][track] = OriginalTrackType::Flux;

                    let entry_idx = flux.tracks[track][side] as usize;
                    let trk = &trks.entries[entry_idx];
                    let mut last = 0;
                    for &b in &data[trk.flux_range()] {
                        last += b as i16;
                        if b == 255 {
                            continue;
                        }
                        if last == 0 {
                            warn!("{:?} transition of 0!", filename);
                        }
                        img.push_flux(side, track, last);
                        last = 0;
                    }
                    if last > 0 {
                        img.push_flux(side, track, last);
                    }
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

                img.set_actual_track_length(side, track, trk.bits_bytes as usize);

                // Fill track
                let block = &data[trk.data_range()];
                for blockbit in trk.bit_range() {
                    let byte = blockbit / 8;
                    let bit = 7 - blockbit % 8;
                    img.push_track_bit(side, track, blockbit, block[byte] & (1 << bit) != 0);
                }
                img.origtracktype[side][track] = OriginalTrackType::Bitstream;
            }
        }

        Ok(img)
    }
}

impl FloppyImageSaver for Moof {
    fn write(img: &FloppyImage, w: &mut impl std::io::Write) -> Result<()> {
        let mut out = vec![];
        let mut cursor = Cursor::new(&mut out);

        if (0..FLOPPY_MAX_SIDES)
            .cartesian_product(0..FLOPPY_MAX_TRACKS)
            .any(|(s, t)| img.get_track_type(s, t) != TrackType::Bitstream)
        {
            bail!("Unsupported track type present in image");
        }

        // Header
        MoofHeader { crc: 0xAAAA }.write(&mut cursor)?;

        // Info chunk
        MoofChunkHeader {
            id: *b"INFO",
            size: 60,
        }
        .write(&mut cursor)?;
        MoofChunkInfo {
            version: 1,
            disktype: img.get_type().into(),
            writeprotect: 0,
            synchronized: 0,
            optimal_bit_timing: if img.get_type() == FloppyType::Mfm144M {
                8
            } else {
                16
            },
            creator: format!("Snow {}", built_info::PKG_VERSION),
            zero: 0,
            largest_track: img
                .trackdata
                .iter()
                .flat_map(|s| s.iter())
                .map(|t| t.len())
                .max()
                .unwrap_or(0)
                .try_into()?,
            flux_block: 0, // TODO for flux
            flux_longest_track: 0,
        }
        .write(&mut cursor)?;

        // Padding
        cursor.write_all(&vec![0; 60 - (size_of::<MoofChunkInfo>() + 4)])?;

        MoofChunkHeader {
            id: *b"TMAP",
            size: 160,
        }
        .write(&mut cursor)?;
        MoofChunkTmap {
            tracks: (0..80)
                .map(|i| {
                    let start = (i * 2) as u8;
                    [start, start.wrapping_add(1)]
                })
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
        }
        .write(&mut cursor)?;

        // Write the tracks
        let mut tracks = vec![];
        let mut block_offsets = [[0; FLOPPY_MAX_TRACKS]; FLOPPY_MAX_SIDES];
        #[allow(clippy::needless_range_loop)]
        for track in 0..FLOPPY_MAX_TRACKS {
            for side in 0..FLOPPY_MAX_SIDES {
                assert_eq!(tracks.len() % 512, 0);
                block_offsets[side][track] = tracks.len() / 512;
                tracks.extend(&img.trackdata[side][track]);

                // Padding to a block
                tracks.resize(tracks.len() + 512 - tracks.len() % 512, 0);
            }
        }

        // Calculate base block offset, size of TRKS plus padding to align to 512
        let base_offset_aligned = (cursor.position() as usize)
            + size_of::<MoofChunkHeader>()
            + size_of::<MoofChunkTrks>();
        assert_eq!(base_offset_aligned % 512, 0);
        let base_block_offset = base_offset_aligned / 512;

        MoofChunkHeader {
            id: *b"TRKS",
            size: (size_of::<MoofChunkTrksEntry>() * FLOPPY_MAX_SIDES * FLOPPY_MAX_TRACKS
                + tracks.len()) as u32,
        }
        .write(&mut cursor)?;
        MoofChunkTrks {
            entries: core::array::from_fn(|i| {
                let side = i % 2;
                let track = i / 2;
                let TrackLength::Bits(tracklen) = img.get_track_length(side, track) else {
                    unreachable!()
                };
                MoofChunkTrksEntry {
                    start_blk: (base_block_offset + block_offsets[side][track]) as u16,
                    blocks: ((tracklen / 8 / 512) + 1).try_into().unwrap(),
                    bits_bytes: tracklen.try_into().unwrap(),
                }
            }),
        }
        .write(&mut cursor)?;
        while cursor.position() % 512 != 0 {
            cursor.write_all(&[0])?;
        }
        cursor.write_all(&tracks)?;

        let metadata = img
            .get_metadata()
            .into_iter()
            .fold(String::new(), |s, (k, v)| format!("{}{}\t{}\n", s, k, v));
        MoofChunkHeader {
            id: *b"META",
            size: metadata.len() as u32,
        }
        .write(&mut cursor)?;
        cursor.write_all(metadata.as_bytes())?;

        // Insert checksum
        let checksum = Self::CHECKSUM.checksum(&out[12..]);
        out[8..12].copy_from_slice(&checksum.to_le_bytes());

        w.write_all(&out)?;
        Ok(())
    }
}
