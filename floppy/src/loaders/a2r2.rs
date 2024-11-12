//! Applesauce A2R version 2 file format
//! Raw capture and solved flux format
//! https://applesaucefdc.com/a2r/

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use super::FloppyImageLoader;
use crate::{Floppy, FloppyImage, FloppyType, OriginalTrackType, TrackLength};

use anyhow::{bail, Context, Result};
use binrw::io::Cursor;
use binrw::{binrw, BinRead};
use log::*;

/// Initial A2R file header
#[binrw]
#[brw(little, magic = b"A2R2\xFF\n\r\n")]
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
enum A2RDiskType {
    /// 1 = 5.25″
    #[brw(magic = 1u8)]
    FiveQ,
    /// 2 = 3.5"
    #[brw(magic = 2u8)]
    ThreeH,
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
    pub disktype: A2RDiskType,
    pub writeprotect: u8,
    pub synchronized: u8,
    pub hard_sectors: u8,
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
    Capture(A2RCapture),
    #[brw(magic = b"\xFF")]
    End,
}

#[binrw]
#[brw(little)]
struct A2RCapture {
    location: u8,
    capture_type: A2RCaptureType,
    capture_size: u32,
    loop_point: u32,
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

/// Applesauce A2R v2.x image file loader
pub struct A2Rv2 {}

impl A2Rv2 {
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

impl FloppyImageLoader for A2Rv2 {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);
        let _header = A2RHeader::read(&mut cursor)?;

        let mut info = None;
        let mut meta = String::new();
        let mut captures = vec![];

        // Parse chunks from file
        while let Ok(chunk) = A2RChunkHeader::read(&mut cursor) {
            let startpos = cursor.position();

            match &chunk.id {
                b"INFO" => info = Some(A2RChunkInfo::read(&mut cursor)?),
                b"STRM" => {
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
        if info.disktype != A2RDiskType::ThreeH {
            bail!("Image is not of a 3.5 inch disk");
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
            if let TrackLength::Transitions(t) = img.get_track_length(side, track) {
                if t > 0 {
                    // Multiple captures encountered, we just use the first and hope it's good.
                    continue;
                }
            }
            img.origtracktype[side][track] = OriginalTrackType::RawFlux;
            let mut last = 0;
            for &b in &capture.capture {
                last += b as i16;
                if b == 255 {
                    continue;
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
