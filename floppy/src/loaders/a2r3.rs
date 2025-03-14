//! Applesauce A2R version 3 file format
//! Raw capture and solved flux format
//! https://applesaucefdc.com/a2r/

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use super::FloppyImageLoader;
use crate::{FloppyImage, FloppyType, OriginalTrackType};

use anyhow::{bail, Context, Result};
use binrw::io::Cursor;
use binrw::{binrw, BinRead};
use log::*;

/// Initial A2R file header
#[binrw]
#[brw(little, magic = b"A2R3\xFF\n\r\n")]
struct A2RHeader {}

/// Standardized chunk header
#[binrw]
#[brw(little)]
#[derive(Debug)]
struct A2RChunkHeader {
    /// ASCII chunk identifier
    pub id: [u8; 4],

    /// Chunk size in bytes
    pub size: u32,
}

/// Disk type
#[binrw]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum A2RDriveType {
    /// 1 = 5.25″ SS 40trk 0.25 step
    #[brw(magic = 1u8)]
    FiveQSSSD,
    /// 2 = 3.5″ DS 80trk Apple CLV
    #[brw(magic = 2u8)]
    ThreeHDSSDCLV,
    /// 3 = 5.25″ DS 80trk
    #[brw(magic = 3u8)]
    FiveQDS80,
    /// 4 = 5.25″ DS 40trk
    #[brw(magic = 4u8)]
    FiveQDS40,
    /// 5 = 3.5″ DS 80trk
    #[brw(magic = 5u8)]
    ThreeHDSSD,
    /// 6 = 8″ DS
    #[brw(magic = 6u8)]
    EightDS,
    /// 7 = 3″ DS 80trk
    #[brw(magic = 7u8)]
    ThreeDS80,
    /// 8 = 3″ DS 40trk
    #[brw(magic = 8u8)]
    ThreeDS40,
}

/// INFO chunk (minus header)
#[binrw]
#[brw(little)]
#[derive(Debug)]
struct A2RChunkInfo {
    pub version: u8,
    #[br(args { count: 32 }, map = |s: Vec<u8>| String::from_utf8_lossy(&s).to_string())]
    #[bw(map = |s: &String| s.as_bytes().to_vec(), pad_size_to = 32)]
    pub creator: String,
    pub drivetype: A2RDriveType,
    pub writeprotect: u8,
    pub synchronized: u8,
    pub hard_sectors: u8,
}

/// RWCP chunk (minus header)
#[binrw]
#[brw(little)]
struct A2RChunkRwcp {
    pub version: u8,
    pub resolution: u32,
    pub padding: [u8; 11],
    // Captures follow
}

#[binrw]
#[derive(Debug, PartialEq, Eq)]
enum A2RCaptureType {
    #[brw(magic = 1u8)]
    Timing,
    #[brw(magic = 2u8)]
    Bits,
    #[brw(magic = 3u8)]
    XTiming,
}

#[binrw]
#[brw(little)]
enum A2RCaptureEntry {
    #[brw(magic = b"C")]
    Capture(A2RCapture),
    #[brw(magic = b"X")]
    End,
}

#[binrw]
#[brw(little)]
struct A2RCapture {
    capture_type: A2RCaptureType,
    location: u16,
    num_indices: u8,
    #[br(count = num_indices)]
    indices: Vec<u32>,
    capture_size: u32,
    #[br(count = capture_size)]
    capture: Vec<u8>,
}

impl A2RCapture {
    pub(super) fn get_track(&self) -> usize {
        usize::from(self.location >> 1)
    }

    pub(super) fn get_side(&self) -> usize {
        usize::from(self.location & 1)
    }
}

/// Applesauce A2R v3.x image file loader
pub struct A2Rv3 {}

impl A2Rv3 {
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

impl FloppyImageLoader for A2Rv3 {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);
        let _header = A2RHeader::read(&mut cursor)?;

        let mut info = None;
        let mut meta = String::new();
        let mut captures = vec![];
        let mut resolution = None;

        // Parse chunks from file
        while let Ok(chunk) = A2RChunkHeader::read(&mut cursor) {
            let startpos = cursor.position();

            match &chunk.id {
                b"INFO" => info = Some(A2RChunkInfo::read(&mut cursor)?),
                b"RWCP" => {
                    let rwcp = A2RChunkRwcp::read(&mut cursor)?;
                    if rwcp.resolution != 125000 {
                        debug!("Converting resolution: {} -> 125000", rwcp.resolution);
                        resolution = Some(rwcp.resolution);
                    }
                    while let A2RCaptureEntry::Capture(capture) =
                        A2RCaptureEntry::read(&mut cursor)?
                    {
                        if capture.capture_type != A2RCaptureType::Timing {
                            continue;
                        }

                        captures.push(capture);
                    }
                }
                b"META" => {
                    let mut metaraw = vec![0u8; chunk.size as usize];
                    cursor.read_exact(&mut metaraw)?;
                    meta = String::from_utf8_lossy(&metaraw).to_string();
                }
                _ => {
                    warn!(
                        "Found unsupported chunk '{}', skipping",
                        String::from_utf8_lossy(&chunk.id)
                    );
                }
            }

            // Always consume the amount of bytes the chunk header reports
            cursor.seek(SeekFrom::Start(startpos + u64::from(chunk.size)))?;
        }

        let info = info.context("No INFO chunk in file")?;
        if info.drivetype != A2RDriveType::ThreeHDSSDCLV {
            bail!("Image is not of a Mac 3.5 inch CLV disk");
        }
        let metadata = Self::parse_meta(&meta);
        let title = metadata
            .get("title")
            .copied()
            .unwrap_or_else(|| filename.unwrap_or_default());

        let mut img = FloppyImage::new_empty(
            if captures.iter().any(|c| c.get_side() > 0) {
                FloppyType::Mac800K
            } else {
                FloppyType::Mac400K
            },
            title,
        );

        // Fill metadata
        for (k, v) in metadata {
            img.set_metadata(k, v);
        }

        for capture in captures {
            let side = capture.get_side();
            let track = capture.get_track();
            img.origtracktype[side][track] = OriginalTrackType::RawFlux;
            let mut last = 0;
            for &b in &capture.capture {
                last += b as i16;
                if b == 255 {
                    continue;
                }
                if let Some(res) = resolution {
                    // Perform conversion
                    let converted = last as u64 * res as u64 / 125000u64;
                    last = converted.try_into()?;
                }
                img.push_flux(side, track, last);
                last = 0;
            }
            if last > 0 {
                img.push_flux(side, track, last);
            }
        }

        Ok(img)
    }
}
