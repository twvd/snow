//! Applesauce A2R version 3 file format
//! Raw capture and solved flux format
//! https://applesaucefdc.com/a2r/

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use super::FloppyImageLoader;
use crate::{FloppyImage, FloppyType};

use anyhow::{Context, Result};
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
#[derive(Debug, Copy, Clone)]
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
    fn load(data: &[u8]) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);
        let _header = A2RHeader::read(&mut cursor)?;

        let mut info = None;
        let mut meta = String::new();

        // Parse chunks from file
        while let Ok(chunk) = A2RChunkHeader::read(&mut cursor) {
            let startpos = cursor.position();

            match &chunk.id {
                b"INFO" => info = Some(A2RChunkInfo::read(&mut cursor)?),
                b"RWCP" => {
                    let _rwcp = A2RChunkRwcp::read(&mut cursor)?;
                    loop {
                        let A2RCaptureEntry::Capture(capture) = A2RCaptureEntry::read(&mut cursor)?
                        else {
                            // End of chunk
                            break;
                        };

                        if capture.capture_type != A2RCaptureType::Timing {
                            continue;
                        }

                        info!(
                            "Capture type {:?} side {} track {}, len {}",
                            capture.capture_type,
                            capture.get_side(),
                            capture.get_track(),
                            capture.capture.len()
                        );
                    }
                }
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

        let _info = info.context("No INFO chunk in file")?;
        let metadata = Self::parse_meta(&meta);
        let title = metadata.get("title").copied().unwrap_or("?");

        let mut img = FloppyImage::new_empty(FloppyType::Mac800K, title);

        // Fill metadata
        for (k, v) in metadata {
            img.set_metadata(k, v);
        }

        Ok(img)
    }
}
