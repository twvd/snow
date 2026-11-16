use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use binrw::io::NoSeek;
use binrw::{BinRead, BinWrite, NullString, binrw};

use crate::devices::{FunctionMut, bus_id};
use crate::emulator::EmulatorConfig;
use crate::mac::scsi::disk_image::FileDiskImage;

#[cfg(not(feature = "mmap"))]
compile_error!("feature \"savestates\" requires the \"mmap\" feature");

const HEADER_VERSION: u16 = 2;

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
    /// The number of images
    #[bw(try_calc = images.len().try_into())]
    pub images_len: u16,
    /// The images that follow the machine's state, in their order
    #[br(count = images_len)]
    pub images: Vec<SavedImage>,
    /// Optional screenshot (PNG), length.
    /// No screenshot if 0.
    #[bw(try_calc = screenshot.len().try_into())]
    pub screenshot_len: u32,
    /// Optional screenshot (PNG)
    #[br(count = screenshot_len)]
    pub screenshot: Vec<u8>,
}

/// An image saved after the machine's state: that of a hard disk, or the page a printer is
/// printing. It belongs to the device in slot `slot` of bus `bus`.
#[binrw]
#[brw(little)]
#[derive(Debug)]
pub struct SavedImage {
    pub bus: NullString,
    pub slot: NullString,
    /// Its length, in bytes
    pub len: u64,
}

const END_OF_CHUNK: &[u8] = b"EOFC";

/// The image saved for the device in slot `slot` of bus `bus`: a hard disk's, whatever its bus,
/// or what a SCSI target saves of itself, as a printer its page
fn saved_image<'a>(config: &'a mut EmulatorConfig, bus: &str, slot: &str) -> Option<&'a [u8]> {
    if bus == bus_id::SCSI {
        let target = config.scsi_mut().targets[slot.parse::<usize>().ok()?].as_deref_mut()?;
        let own = target.savestate_img_len().is_some();
        if own {
            target.savestate_img_data()
        } else if let FunctionMut::HardDisk(disk) = target.function() {
            disk.savestate_image()
        } else {
            None
        }
    } else if let FunctionMut::HardDisk(disk) = config.bus_ops(bus)?.device(slot)?.function() {
        disk.savestate_image()
    } else {
        None
    }
}

/// The slots of the machine, as (bus id, slot id), in the order of its list of buses
fn slots(config: &EmulatorConfig) -> Vec<(String, String)> {
    config
        .model()
        .buses()
        .into_iter()
        .flat_map(|bus| {
            bus.slots
                .into_iter()
                .map(move |slot| (bus.id.clone(), slot.id))
        })
        .collect()
}

/// Gives the device in `slot` of `bus` its image from a restored state, in the file `filename`
fn restore_image(
    config: &mut EmulatorConfig,
    bus: &str,
    slot: &str,
    filename: &Path,
) -> Result<()> {
    let device = config
        .bus_ops(bus)
        .and_then(|ops| ops.device(slot))
        .with_context(|| {
            format!(
                "The state has an image for {} #{}, which has no device",
                bus, slot
            )
        })?;
    if let FunctionMut::HardDisk(disk) = device.function() {
        return disk.load_image(Box::new(FileDiskImage::open(filename, true)?));
    }
    // What a SCSI target saved of itself
    let target = slot
        .parse::<usize>()
        .ok()
        .filter(|_| bus == bus_id::SCSI)
        .and_then(|id| config.scsi_mut().targets[id].as_mut())
        .context("Only a SCSI target saves an image of its own")?;
    target.after_deserialize(filename)
}

/// Writes a save state to the given writer
pub(super) fn save_state_to<W: std::io::Write + std::io::Seek>(
    mut writer: W,
    config: &mut EmulatorConfig,
    screenshot: Option<Vec<u8>>,
) -> Result<()> {
    let compression_level = 0; // library default

    let slots = slots(config);
    let images = slots
        .iter()
        .filter_map(|(bus, slot)| {
            let len = saved_image(config, bus, slot)?.len();
            Some(SavedImage {
                bus: bus.as_str().into(),
                slot: slot.as_str().into(),
                len: len as u64,
            })
        })
        .collect();
    let header = SaveHeader {
        version: HEADER_VERSION,
        compression: SaveCompression::Zstd,
        compression_level,
        model: config.model().to_string().into(),
        snow_version: crate::build_version().into(),
        timestamp: chrono::Local::now().to_rfc3339().into(),
        images,
        screenshot: screenshot.unwrap_or_default(),
    };
    header.write(&mut writer)?;

    let compressor =
        NoSeek::new(zstd::stream::Encoder::new(writer, compression_level.into())?.auto_finish());
    let mut compressor = postcard::to_io(config, compressor)?;
    END_OF_CHUNK.write(&mut compressor)?;

    for (bus, slot) in &slots {
        if let Some(image) = saved_image(config, bus, slot) {
            image.write(&mut compressor)?;
            END_OF_CHUNK.write(&mut compressor)?;
        }
    }

    Ok(())
}

/// Loads a save state into an EmulatorConfig from a given reader. Each image of the state is
/// written to a temporary file in `tmpdir`, which its device carries on from: the second result
/// lists those files.
pub(super) fn load_state_from<R: std::io::Read + std::io::Seek, P: AsRef<Path>>(
    mut reader: R,
    tmpdir: P,
) -> Result<(EmulatorConfig, Vec<PathBuf>)> {
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

    let mut temp_files = vec![];
    for (index, image) in header.images.iter().enumerate() {
        // Write the image out to a temporary file we can then continue
        // working out of. The file is lost on shutdown.
        let filename =
            tmpdir
                .as_ref()
                .join(format!("snow_state_{}_{}.img", std::process::id(), index));
        {
            let mut outfile = File::create(&filename)?;
            let mut img_reader = decompressor.take(image.len);
            std::io::copy(&mut img_reader, &mut outfile)?;
            decompressor = img_reader.into_inner();
            // Drop 'outfile'
        }
        restore_image(
            &mut config,
            &image.bus.to_string(),
            &image.slot.to_string(),
            &filename,
        )?;
        temp_files.push(filename);

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

    Ok((config, temp_files))
}

/// Returns only the header from given state file
pub fn load_state_header<R: std::io::Read + std::io::Seek>(reader: &mut R) -> Result<SaveHeader> {
    Ok(SaveHeader::read(reader)?)
}
