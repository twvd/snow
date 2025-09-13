use anyhow::{bail, Result};
use binrw::{binrw, BinRead, BinWrite, NullString};

use crate::emulator::EmulatorConfig;

#[binrw]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SaveCompression {
    #[brw(magic = 1u8)]
    None,
    #[brw(magic = 2u8)]
    Zstd,
}

#[binrw]
#[brw(little, magic = b"SNOWS")]
struct SaveHeader {
    /// Header/file version
    pub version: u16,
    /// Type of compression used
    pub compression: SaveCompression,
    /// Level of compression
    pub compression_level: u8,
    /// Model as string
    pub model: NullString,
    /// Snow version (short hash)
    pub snow_version: NullString,
}

/// Writes a save state to the given writer
pub fn save_state_to<W: std::io::Write + std::io::Seek>(
    mut writer: W,
    config: &EmulatorConfig,
) -> Result<()> {
    let compression_level = 0; // library default

    let header = SaveHeader {
        version: 1,
        compression: SaveCompression::Zstd,
        compression_level,
        model: config.model().to_string().into(),
        snow_version: crate::build_version().into(),
    };
    header.write(&mut writer)?;

    let compressor = zstd::stream::Encoder::new(writer, compression_level.into())?.auto_finish();
    postcard::to_io(config, compressor)?;

    Ok(())
}

/// Loads a save state into an EmulatorConfig from a given reader
pub fn load_state_from<R: std::io::Read + std::io::Seek>(mut reader: R) -> Result<EmulatorConfig> {
    let header = SaveHeader::read(&mut reader)?;

    if header.version != 1 {
        bail!("Invalid state file version {}", header.version);
    }

    if header.compression != SaveCompression::Zstd {
        bail!("Unsupported compression method {:?}", header.compression);
    }

    let decompressor = zstd::stream::Decoder::new(reader)?;

    // TODO remove static buffer once postcard supports it, tracking issue:
    // https://github.com/jamesmunns/postcard/issues/162
    let mut buf = [0; 1024];
    let config: EmulatorConfig = postcard::from_io((decompressor, &mut buf))?.0;

    Ok(config)
}
