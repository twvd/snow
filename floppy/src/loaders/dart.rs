//! Apple Disk Archive / Retrieval Tool ('DART') format
//! Sector-based image format with compression
//! https://ciderpress2.com/formatdoc/DART-notes.html

use std::io::Read;

#[cfg(feature = "fluxfox")]
use super::fluxfox::Fluxfox;
use super::FloppyImageLoader;
use crate::macformat::MacFormatEncoder;
use crate::{FloppyImage, FloppyType};

use anyhow::{anyhow, bail, Result};
use binrw::io::Cursor;
use binrw::{binrw, BinRead, BinReaderExt};
use fluxfox::io::ReadBytesExt;
use retrocompressor::lzss_huff;

const DECOMPRESSED_CHUNK_SIZE: usize = 20960;
const LZHUF_OPTIONS: lzss_huff::Options = lzss_huff::Options {
    header: false,
    in_offset: 0,
    out_offset: 0,
    window_size: 4096,
    threshold: 2,
    lookahead: 60,
    precursor: 0,
    max_file_size: DECOMPRESSED_CHUNK_SIZE as u64,
};

#[binrw]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DartCompressionType {
    /// 0 = fast (RLE)
    #[brw(magic = 0u8)]
    Rle,
    /// 1 = best (LZHUF)
    #[brw(magic = 1u8)]
    Lzhuf,
    /// 2 = none
    #[brw(magic = 2u8)]
    None,
}

#[binrw]
#[derive(Debug, Clone, Copy)]
enum DartDiskType {
    /// $01 = Macintosh GCR
    #[brw(magic = 0x01u8)]
    MacintoshGCR,
    /// $02 = Lisa
    #[brw(magic = 0x02u8)]
    Lisa,
    /// $03 = Apple II
    #[brw(magic = 0x03u8)]
    AppleII,
    /// $10 = Macintosh MFM
    #[brw(magic = 0x10u8)]
    MacintoshMFM,
    /// $11 = MS-DOS 720K
    #[brw(magic = 0x11u8)]
    Dos720K,
    /// $12 = MS-DOS 1440K
    #[brw(magic = 0x12u8)]
    Dos144M,
}

impl DartDiskType {
    pub fn chunks(self) -> usize {
        match self {
            Self::MacintoshGCR | Self::Lisa | Self::AppleII => 40,
            Self::MacintoshMFM | Self::Dos720K | Self::Dos144M => 72,
        }
    }
}

#[binrw]
#[brw(big)]
#[derive(Debug)]
struct DartHeader {
    pub compression: DartCompressionType,
    pub disktype: DartDiskType,
    pub disksize: u16,
    #[br(count = disktype.chunks())]
    pub chunk_sizes: Vec<u16>,
}

impl DartHeader {
    pub fn get_type(&self) -> Result<FloppyType> {
        match self.disksize {
            400 => Ok(FloppyType::Mac400K),
            800 => Ok(FloppyType::Mac800K),
            1440 => Ok(FloppyType::Mfm144M),
            _ => bail!("Unsupported DART disk size: {}", self.disksize),
        }
    }
}

/// Apple DART image loader
pub struct Dart {}

impl FloppyImageLoader for Dart {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);
        let header = DartHeader::read(&mut cursor)?;

        let floppytype = header.get_type()?;

        // Read/decompress blocks
        let mut data = vec![];
        let mut tags = vec![];
        for (chunk, &chunk_len) in header.chunk_sizes.iter().enumerate() {
            if chunk_len == 0 {
                continue;
            }

            let decompressed = if chunk_len == 0xFFFF
                || usize::from(chunk_len) == DECOMPRESSED_CHUNK_SIZE
                || header.compression == DartCompressionType::None
            {
                let mut out = vec![0; DECOMPRESSED_CHUNK_SIZE];
                cursor.read_exact(&mut out)?;
                out
            } else if header.compression == DartCompressionType::Rle {
                let mut out: Vec<u8> = vec![];
                let mut done = 0u16;
                // chunk_len counts 16-bit words here

                loop {
                    let count = cursor.read_be::<u16>()? as i16;
                    done += 1;
                    if count > 0 {
                        for _ in 0..(count * 2) {
                            out.push(cursor.read_u8()?);
                        }
                        done += count as u16;
                    } else {
                        let pattern = cursor.read_be::<u16>()?.to_be_bytes();
                        out.extend(
                            pattern
                                .iter()
                                .cycle()
                                .take((count.unsigned_abs() as usize) * 2)
                                .cloned(),
                        );
                        done += 1;
                    }

                    if done == chunk_len {
                        break;
                    } else if done > chunk_len {
                        bail!("done > blen");
                    }
                }
                out
            } else {
                // LZHUF
                let mut compressed = vec![0; chunk_len.into()];
                cursor.read_exact(&mut compressed)?;
                lzss_huff::expand_slice(&compressed, &LZHUF_OPTIONS)
                    .map_err(|e| anyhow!("Decompression failed: {}", e))?
            };

            if decompressed.len() < DECOMPRESSED_CHUNK_SIZE {
                // Sometimes LZHA output is +1?
                bail!(
                    "Chunk {} bad output length after decompression: {}",
                    chunk,
                    decompressed.len()
                );
            }

            data.extend_from_slice(&decompressed[0..(512 * 40)]);
            tags.extend_from_slice(&decompressed[(512 * 40)..((512 * 40) + (12 * 40))]);
        }

        if floppytype == FloppyType::Mfm144M {
            #[cfg(feature = "fluxfox")]
            {
                // Hand-off to Fluxfox
                Fluxfox::load(&data, filename)
            }
            #[cfg(not(feature = "fluxfox"))]
            {
                bail!("Requires fluxfox feature");
            }
        } else {
            MacFormatEncoder::encode(floppytype, &data, Some(&tags), filename.unwrap_or_default())
        }
    }
}
