use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Result, bail};
use binrw::io::NoSeek;
use binrw::{BinRead, BinWrite, NullString, binrw};
use serde::{Deserialize, Serialize};

use crate::emulator::EmulatorConfig;

#[cfg(not(feature = "mmap"))]
compile_error!("feature \"savestates\" requires the \"mmap\" feature");

const HEADER_VERSION: u16 = 3;

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
    /// The machine's devices, as a workspace lists them, in JSON
    pub devices: NullString,
    /// Optional screenshot (PNG), length.
    /// No screenshot if 0.
    #[bw(try_calc = screenshot.len().try_into())]
    pub screenshot_len: u32,
    /// Optional screenshot (PNG)
    #[br(count = screenshot_len)]
    pub screenshot: Vec<u8>,
}

const END_OF_CHUNK: &[u8] = b"EOFC";

/// A device saved on its own, under the ids of its slot. The devices of the slots that can
/// change are saved this way, the fixed ones with the machine.
#[derive(Serialize, Deserialize)]
pub(super) struct SlotState {
    pub(super) bus: String,
    pub(super) slot: String,
    /// The device, serialized by itself
    pub(super) state: Vec<u8>,
    /// The size of its media, which follows the list in the file, if it saved any
    pub(super) media_len: Option<u64>,
}

/// A slot's state to save, with its media
pub(super) type SavedSlot<'a> = (SlotState, Option<&'a [u8]>);

/// A slot's state read back, with its media in a temporary file
pub(super) struct RestoredSlot {
    pub(super) state: SlotState,
    pub(super) media: Option<PathBuf>,
}

/// Writes a save state to the given writer
pub(super) fn save_state_to<W: std::io::Write + std::io::Seek>(
    mut writer: W,
    config: &EmulatorConfig,
    screenshot: Option<Vec<u8>>,
    devices: String,
    slots: &[SavedSlot],
) -> Result<()> {
    let compression_level = 0; // library default

    let header = SaveHeader {
        version: HEADER_VERSION,
        compression: SaveCompression::Zstd,
        compression_level,
        model: config.model().to_string().into(),
        snow_version: crate::build_version().into(),
        devices: devices.into(),
        timestamp: chrono::Local::now().to_rfc3339().into(),
        screenshot: screenshot.unwrap_or_default(),
    };
    header.write(&mut writer)?;

    let compressor =
        NoSeek::new(zstd::stream::Encoder::new(writer, compression_level.into())?.auto_finish());
    let mut compressor = postcard::to_io(config, compressor)?;
    END_OF_CHUNK.write(&mut compressor)?;

    let states: Vec<&SlotState> = slots.iter().map(|(state, _)| state).collect();
    let mut compressor = postcard::to_io(&states, compressor)?;
    END_OF_CHUNK.write(&mut compressor)?;

    for (_, media) in slots {
        if let Some(data) = media {
            std::io::Write::write_all(&mut compressor, data)?;
            END_OF_CHUNK.write(&mut compressor)?;
        }
    }

    // HD20 images follow, their sizes are in the serialized config
    for dcd in &config.swim().dcds {
        let Some(data) = dcd.savestate_img_data() else {
            bail!("HD20 image cannot be saved in a save state");
        };
        data.write(&mut compressor)?;
        END_OF_CHUNK.write(&mut compressor)?;
    }

    Ok(())
}

/// Loads a save state into an EmulatorConfig from a given reader
pub(super) fn load_state_from<R: std::io::Read + std::io::Seek, P: AsRef<std::path::Path>>(
    mut reader: R,
    tmpdir: P,
) -> Result<(EmulatorConfig, String, Vec<RestoredSlot>)> {
    let header = SaveHeader::read(&mut reader)?;

    if header.version != HEADER_VERSION {
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

    let (states, (mut decompressor, _)) =
        postcard::from_io::<Vec<SlotState>, _>((decompressor, &mut buf))?;
    decompressor.read_exact(&mut eofcbuf)?;
    if eofcbuf != END_OF_CHUNK {
        bail!("Expected end of chunk but did not find it");
    }

    let mut slots = vec![];
    for (idx, state) in states.into_iter().enumerate() {
        let Some(len) = state.media_len else {
            slots.push(RestoredSlot { state, media: None });
            continue;
        };
        // Write the media out to a temporary file we can then continue
        // working out of. The file is lost on shutdown.
        let mut filename = tmpdir.as_ref().to_path_buf();
        filename.push(format!("snow_state_{}_{}.img", std::process::id(), idx));
        {
            let mut outfile = File::create(&filename)?;
            let mut img_reader = decompressor.take(len);
            std::io::copy(&mut img_reader, &mut outfile)?;
            decompressor = img_reader.into_inner();
            // Drop 'outfile'
        }
        decompressor.read_exact(&mut eofcbuf)?;
        if eofcbuf != END_OF_CHUNK {
            bail!("Expected end of chunk but did not find it");
        }
        slots.push(RestoredSlot {
            state,
            media: Some(filename),
        });
    }

    for (idx, dcd) in config.swim_mut().dcds.iter_mut().enumerate() {
        let mut filename = tmpdir.as_ref().to_path_buf();
        filename.push(format!(
            "snow_state_{}_hd20_{}.img",
            std::process::id(),
            idx
        ));
        {
            let mut outfile = File::create(&filename)?;
            let mut img_reader = decompressor.take(dcd.savestate_img_len() as u64);
            std::io::copy(&mut img_reader, &mut outfile)?;
            decompressor = img_reader.into_inner();
        }
        dcd.after_deserialize(&filename)?;

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

    Ok((config, header.devices.to_string(), slots))
}

/// Returns only the header from given state file
pub fn load_state_header<R: std::io::Read + std::io::Seek>(reader: &mut R) -> Result<SaveHeader> {
    Ok(SaveHeader::read(reader)?)
}
