use std::fs::File;
use std::io::Read;

use anyhow::{bail, Result};
use binrw::io::NoSeek;
use binrw::{binrw, BinRead, BinWrite, NullString};

use crate::emulator::EmulatorConfig;
use crate::mac::scsi::controller::ScsiController;

#[cfg(not(feature = "mmap"))]
compile_error!("feature \"savestates\" requires the \"mmap\" feature");

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
pub struct SaveHeader {
    /// Header/file version
    pub version: u16,
    /// Type of compression used
    pub compression: SaveCompression,
    /// Level of compression
    pub compression_level: u8,
    /// RFC3339 timestamp when the file was written
    pub timestamp: NullString,
    /// Model as string
    pub model: NullString,
    /// Snow version (short hash)
    pub snow_version: NullString,
    /// Image sizes, per SCSI target. 0 is no image.
    pub scsi_imgs: [u64; ScsiController::MAX_TARGETS],
    /// Optional screenshot (PNG), length.
    /// No screenshot if 0.
    #[bw(try_calc = screenshot.len().try_into())]
    pub screenshot_len: u32,
    /// Optional screenshot (PNG)
    #[br(count = screenshot_len)]
    pub screenshot: Vec<u8>,
}

const END_OF_CHUNK: &[u8] = b"EOFC";

/// Writes a save state to the given writer
pub(super) fn save_state_to<W: std::io::Write + std::io::Seek>(
    mut writer: W,
    config: &EmulatorConfig,
    screenshot: Option<Vec<u8>>,
) -> Result<()> {
    let compression_level = 0; // library default

    let header = SaveHeader {
        version: 1,
        compression: SaveCompression::Zstd,
        compression_level,
        model: config.model().to_string().into(),
        snow_version: crate::build_version().into(),
        timestamp: chrono::Local::now().to_rfc3339().into(),
        scsi_imgs: core::array::from_fn(|id| {
            config
                .scsi()
                .get_disk_capacity(id)
                .map(|n| n as u64)
                .unwrap_or(0)
        }),
        screenshot: screenshot.unwrap_or_default(),
    };
    header.write(&mut writer)?;

    let compressor =
        NoSeek::new(zstd::stream::Encoder::new(writer, compression_level.into())?.auto_finish());
    let mut compressor = postcard::to_io(config, compressor)?;
    END_OF_CHUNK.write(&mut compressor)?;

    for id in 0..ScsiController::MAX_TARGETS {
        if config.scsi().get_disk_capacity(id).is_none() {
            continue;
        }

        config.scsi().targets[id]
            .as_ref()
            .unwrap()
            .media()
            .unwrap()
            .write(&mut compressor)?;
        END_OF_CHUNK.write(&mut compressor)?;
    }

    Ok(())
}

/// Loads a save state into an EmulatorConfig from a given reader
pub(super) fn load_state_from<R: std::io::Read + std::io::Seek, P: AsRef<std::path::Path>>(
    mut reader: R,
    tmpdir: P,
) -> Result<EmulatorConfig> {
    let header = SaveHeader::read(&mut reader)?;

    if header.version != 1 {
        bail!("Invalid state file version {}", header.version);
    }

    if header.compression != SaveCompression::Zstd {
        bail!("Unsupported compression method {:?}", header.compression);
    }

    let decompressor = NoSeek::new(zstd::stream::Decoder::new(reader)?);

    // TODO remove static buffer once postcard supports it, tracking issue:
    // https://github.com/jamesmunns/postcard/issues/162
    let mut buf = [0; 1024];
    let (mut config, (mut decompressor, _)) =
        postcard::from_io::<EmulatorConfig, _>((decompressor, &mut buf))?;

    let mut eofcbuf = [0; END_OF_CHUNK.len()];
    decompressor.read_exact(&mut eofcbuf)?;
    if eofcbuf != END_OF_CHUNK {
        bail!("Expected end of chunk but did not find it");
    }

    for (id, &sz) in header.scsi_imgs.iter().enumerate() {
        if sz == 0 {
            continue;
        }

        // Write the image out to a temporary file we can then continue
        // working out of. The file is lost on shutdown.
        let mut filename = tmpdir.as_ref().to_path_buf();
        filename.push(format!("snow_state_{}_{}.img", std::process::id(), id));
        {
            let mut outfile = File::create(&filename)?;
            let mut img_reader = decompressor.take(sz);
            std::io::copy(&mut img_reader, &mut outfile)?;
            decompressor = img_reader.into_inner();
            // Drop 'outfile'
        }

        // Attach temporary image on this location
        config.scsi_mut().targets[id]
            .as_mut()
            .unwrap()
            .after_deserialize(&filename)?;

        let mut eofcbuf = [0; END_OF_CHUNK.len()];
        decompressor.read_exact(&mut eofcbuf)?;
        if eofcbuf != END_OF_CHUNK {
            bail!("Expected end of chunk but did not find it");
        }
    }

    #[allow(clippy::unbuffered_bytes)]
    if decompressor.bytes().next().is_some() {
        bail!("Expected EOF but found more data");
    }

    Ok(config)
}

/// Returns only the header from given state file
pub fn load_state_header<R: std::io::Read + std::io::Seek>(reader: &mut R) -> Result<SaveHeader> {
    Ok(SaveHeader::read(reader)?)
}
