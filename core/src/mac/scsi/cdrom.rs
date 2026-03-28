//! SCSI CD-ROM drive (block device)

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use std::fs::File;
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::debuggable::Debuggable;
use crate::types::LatchingEvent;

use super::disk_image::{DiskImage, FileDiskImage};
use super::target::ScsiTarget;
use super::target::ScsiTargetEvent;
use super::target::ScsiTargetType;
use super::ScsiCmdResult;
use super::ASC_INVALID_FIELD_IN_CDB;
use super::ASC_MEDIUM_NOT_PRESENT;
use super::CC_KEY_ILLEGAL_REQUEST;
use super::CC_KEY_MEDIUM_ERROR;
use super::STATUS_CHECK_CONDITION;
use super::STATUS_GOOD;

// Reference documentation:
//
// [PIONEER]: <https://bitsavers.trailing-edge.com/pdf/pioneer/cdrom/OB-U0077C_CD-ROM_SCSI-2_Command_Set_V3.1_19970626.pdf>
// [UNI-MAINZ]: <https://www.staff.uni-mainz.de/tacke/scsi/SCSI2-14.html>

const RAW_SECTOR_LEN: usize = 2352;
const TRACK_LEADOUT: u8 = 0xAA;

// Audio status codes
const AUDIO_COMPLETED: u8 = 0x13;

// Track ADR/Control field codes
const AUDIO_TRACK: u8 = 0x10; // FIXME: correct?
const DATA_TRACK: u8 = 0x14;


pub struct TrackInfo {
    /// The track number. Note that CD tracks don't necessarily start at number 1.
    number: u8,
    /// ADR and Control fields indicating track format
    adr_control: u8,
    /// Sector number where the track begins
    sector: u32, // For pete's sake, just use the sector number... forget about this MSF/LBA nonsense.
}

pub trait CdromBackend: Send {
    fn byte_len(&self) -> usize;
    fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8>;
    fn image_path(&self) -> Option<&Path>;
    fn audio_status(&self) -> u8;
    fn tracks(&self) -> Option<&[TrackInfo]>;
}

struct IsoCdromBackend {
    image: Box<dyn DiskImage>,
}

impl IsoCdromBackend {
    fn new(image: Box<dyn DiskImage>) -> Self {
        Self {
            image
        }
    }
}

impl CdromBackend for IsoCdromBackend {
    fn byte_len(&self) -> usize {
        self.image.byte_len()
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8> {
        self.image.read_bytes(offset, length)
    }

    fn image_path(&self) -> Option<&Path> {
        self.image.image_path()
    }

    fn audio_status(&self) -> u8 {
        // ISO's do not support audio playback.
        AUDIO_COMPLETED
    }

    fn tracks(&self) -> Option<&[TrackInfo]> {
        Some(&[
            TrackInfo {
                number: 1,
                adr_control: DATA_TRACK,
                sector: 0,
            },
        ])
    }
}

struct CuesheetCdromBackend {
    cue_path: PathBuf,
    data_file: File,
}

impl CuesheetCdromBackend {
    fn new(path: &Path) -> Result<Self> {
        let data_file = File::open(r"F:\Playroom\Macintosh\Marathon CD.bin")?;

        Ok(Self {
            cue_path: path.into(),
            data_file,
        })
    }

    fn read_raw_sector(&self, sector: u32) -> Result<[u8; RAW_SECTOR_LEN]> {
        let mut result = [0; RAW_SECTOR_LEN];
        self.data_file.seek_read(&mut result, (sector * RAW_SECTOR_LEN as u32).into())?;
        Ok(result)
    }
}

impl CdromBackend for CuesheetCdromBackend {
    fn byte_len(&self) -> usize {
        // FIXME: What's the correct value here? Let's just say 333,000 * 2048-byte sectors.
        333_000 * 2048
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8> {
        println!("Reading {} bytes from offset 0x{:X}", length, offset);
        let mut result = Vec::<u8>::with_capacity(length);

        let mut sector = (offset / 2048).try_into().unwrap();
        while result.len() < length {
            // TODO: Better error robustness if read fails
            let raw_sector = self.read_raw_sector(sector).unwrap();
            sector += 1;
            // TODO: Check sync, mode and error detection data?
            let sector_data = &raw_sector[16..][..2048];
            result.extend_from_slice(sector_data);
        }

        result.truncate(length);
        result
    }

    fn image_path(&self) -> Option<&Path> {
        Some(&self.cue_path)
    }

    fn audio_status(&self) -> u8 {
        // TODO: implement audio playback
        AUDIO_COMPLETED
    }

    // TODO: read from cuesheet
    fn tracks(&self) -> Option<&[TrackInfo]> {
        Some(&[
            TrackInfo {
                number: 1,
                adr_control: DATA_TRACK,
                sector: 0,
            },
            TrackInfo {
                number: 2,
                adr_control: AUDIO_TRACK,
                sector: 20_000,
            },
            TrackInfo {
                number: 3,
                adr_control: AUDIO_TRACK,
                sector: 20_000,
            },
            TrackInfo {
                number: 4,
                adr_control: AUDIO_TRACK,
                sector: 40_000,
            },
            TrackInfo {
                number: 5,
                adr_control: AUDIO_TRACK,
                sector: 50_000,
            },
            TrackInfo {
                number: 6,
                adr_control: AUDIO_TRACK,
                sector: 60_000,
            },
        ])
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct ScsiTargetCdrom {
    /// Disk contents
    #[serde(skip)]
    pub(super) backend: Option<Box<dyn CdromBackend>>,

    /// Check condition code
    cc_code: u8,

    /// Check condition ASC
    cc_asc: u16,

    /// Media eject event
    event_eject: LatchingEvent,

    /// Block size
    blocksize: usize,
}

impl Default for ScsiTargetCdrom {
    fn default() -> Self {
        Self {
            backend: None,
            cc_code: 0,
            cc_asc: 0,
            event_eject: Default::default(),
            blocksize: 2048,
        }
    }
}

impl ScsiTargetCdrom {
    const VALID_BLOCKSIZES: [usize; 2] = [512, 2048];

    fn sector_to_address_field(&self, sector: u32, msf: bool) -> [u8; 4] {
        // FIXME: is 00:02:00 pre-gap involved here?
        if msf {
            // A sector is also known as a "frame" in CD parlance.
            let m = sector / 75 / 60;
            let s = (sector / 75) % 60;
            let f = sector % 75;
            // [UNI-MAINZ] Table 237: MSF address format
            [
                0, // Reserved
                m.try_into().unwrap(), // M field
                s.try_into().unwrap(), // S field
                f.try_into().unwrap(), // F field
            ]
        } else {
            // FIXME: is this correct? I can't find any software that sets a non-2048 blocksize.
            let lba = sector * 2048 / self.blocksize as u32;
            u32::to_be_bytes(lba)
        }
    }

    fn read_toc(&mut self, msf: bool, format: u8, track: u8, alloc_len: usize) -> Result<ScsiCmdResult> {
        let Some(backend) = &self.backend else {
            // No CD inserted
            self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        };

        let Some(tracks) = backend.tracks() else {
            // Media does not support tracks
            //
            // [PIONEER]:
            //
            // If the Start Track field is not valid for the currently installed medium, the command shall be
            // terminated with Check Condition status. The sense key shall be set to ILLEGAL REQUEST and
            // the additional sense code set to INVALID FIELD IN CDB.
            self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        };

        match format {
            0 => {
                // SCSI-2 TOC
                let mut result = Vec::<u8>::with_capacity(alloc_len);

                result.push(0); // TOC Data Length (will be set later)
                result.push(0);

                // FIXME: avoid unwrap
                result.push(tracks.first().unwrap().number); // First Track Number
                result.push(tracks.last().unwrap().number); // Last Track Number

                // Start at the given track or the next available track
                let track_iter = tracks.iter().skip_while(|t| t.number < track);

                // Emit track descriptors
                for t in track_iter {
                    result.push(0); // Reserved
                    result.push(t.adr_control); // ADR/Control
                    result.push(t.number); // Track Number
                    result.push(0); // Reserved
                    result.extend_from_slice(&self.sector_to_address_field(t.sector, msf)); // Absolute CD-ROM Address
                }

                // Emit lead-out track descriptor (FIXME: Is this correct?)
                result.push(0); // Reserved
                result.push(DATA_TRACK); // ADR/Control (FIXME: is this correct for the lead-out track?)
                result.push(TRACK_LEADOUT); // Track Number
                result.push(0); // Reserved
                result.extend_from_slice(&u32::to_be_bytes(0)); // Absolute CD-ROM Address (FIXME: is this correct?)

                // Set data length field
                let data_length = result.len() - 2;
                result[0..2].copy_from_slice(&u16::to_be_bytes(data_length.try_into()?));

                result.truncate(alloc_len);
                Ok(ScsiCmdResult::DataIn(result))
            }
            1 => {
                // Session TOC
                let mut result = Vec::<u8>::with_capacity(alloc_len);

                // [PIONEER] Table 2-28C: TOC Data with Format=01B
                result.push(0); // TOC Data Length (will be set later)
                result.push(0);

                // TODO: support multi-session discs?
                result.push(1); // First Session Number
                result.push(1); // Last Session Number

                // This command queries the "first track in the last session" apparently...
                let first_track = tracks.last().unwrap();

                // [PIONEER] Table 2-28D: Track Descriptors
                result.push(0); // Reserved
                result.push(first_track.adr_control); // ADR/Control
                result.push(first_track.number); // First Track Number in Last Session
                result.push(0); // Reserved
                result.extend_from_slice(&self.sector_to_address_field(first_track.sector, msf)); // Absolute CD-ROM Address of the First Track in the Last Session

                let data_length = result.len() - 2;
                result[0..2].copy_from_slice(&u16::to_be_bytes(data_length.try_into()?));

                result.truncate(alloc_len);
                Ok(ScsiCmdResult::DataIn(result))
            }
            // TODO: implement format 2 (queried by System 7.5 upon mounting a CD)
            _ => {
                log::error!("Unknown READ TOC format: {}", format);

                self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }

    fn eject_media(&mut self) {
        self.event_eject.set();
        self.backend = None;
    }
}

impl ScsiTargetCdrom {
    fn load_cue(&mut self, path: &Path) -> Result<()> {
        self.backend = Some(Box::new(CuesheetCdromBackend::new(path)?));
        self.cc_code = 0;
        self.cc_asc = 0;
        self.event_eject.get_clear();
        Ok(())
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetCdrom {
    /// Try to load a disk image, given the filename of the image.
    ///
    /// This locks the file on disk and memory maps the file for use by
    /// the emulator for fast access and automatic writes back to disk,
    /// at the discretion of the operating system.
    fn load_media(&mut self, path: &Path) -> Result<()> {
        if path.extension().map(|ext| ext.eq_ignore_ascii_case("cue")).unwrap_or(false) {
            self.load_cue(path)
        } else {
            // Assume image is iso or toast
            self.load_image(Box::new(FileDiskImage::open(path, false)?))
        }
    }

    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()> {
        self.backend = Some(Box::new(IsoCdromBackend::new(image)));
        self.cc_code = 0;
        self.cc_asc = 0;
        self.event_eject.get_clear();
        Ok(())
    }

    fn media(&self) -> Option<&[u8]> {
        unreachable!("Can't call media() on a CD-ROM");
        // self.backend
        //     .as_ref()
        //     .and_then(|backend| backend.media_bytes())
    }

    fn take_event(&mut self) -> Option<ScsiTargetEvent> {
        if self.event_eject.get_clear() {
            Some(ScsiTargetEvent::MediaEjected)
        } else {
            None
        }
    }

    fn target_type(&self) -> ScsiTargetType {
        ScsiTargetType::Cdrom
    }

    fn unit_ready(&mut self) -> Result<ScsiCmdResult> {
        if self.backend.is_some() {
            // CD inserted, ready
            Ok(ScsiCmdResult::Status(STATUS_GOOD))
        } else {
            // No CD inserted
            self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
            Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
        }
    }

    fn req_sense(&mut self) -> (u8, u16) {
        (
            std::mem::take(&mut self.cc_code),
            std::mem::take(&mut self.cc_asc),
        )
    }

    fn inquiry(&mut self, _cmd: &[u8]) -> Result<ScsiCmdResult> {
        let mut result = vec![0; 36];

        // 0 Peripheral qualifier (5-7), peripheral device type (4-0)
        result[0] = 5; // CD-ROM drive
        result[1] = 0x80; // Media removable
        result[2] = 0x02; // ANSI-2
        result[3] = 0x02; // ANSI-2

        // 4 Additional length (N-4), min. 32
        result[4] = result.len() as u8 - 4;

        // 8..16 Vendor identification
        result[8..16].copy_from_slice(b"SNOW    ");

        // 16..32 Product identification
        result[16..32].copy_from_slice(b"CD-ROM CDU-55S  ");
        // 32..36 Revision
        result[32..36].copy_from_slice(b"1.9a");
        Ok(ScsiCmdResult::DataIn(result))
    }

    fn mode_sense(&mut self, page: u8) -> Option<Vec<u8>> {
        match page {
            0x01 => {
                // Read/write recovery page

                // Error recovery stuff, can remain at 0.

                Some(vec![0; 6])
            }
            0x03 => {
                // Format device page

                Some(vec![0; 0x16])
            }
            0x0e => {
                // CD-ROM audio control parameters page

                // TODO: Return info about port volumes, etc.
                Some(vec![0; 0xe])
            }
            0x30 => {
                // ? Non-standard mode page

                let mut result = vec![0; 0x16];
                result[0..0x16].copy_from_slice(b"APPLE COMPUTER, INC   ");
                Some(result)
            }
            _ => None,
        }
    }

    fn blocksize(&self) -> Option<usize> {
        Some(self.blocksize)
    }

    fn blocks(&self) -> Option<usize> {
        Some(self.backend.as_ref()?.byte_len().div_ceil(self.blocksize))
    }

    fn read(&self, block_offset: usize, block_count: usize) -> Vec<u8> {
        // If blocks() returns None this will never be called by
        // ScsiTarget::cmd
        let blocksize = self.blocksize;
        let backend = self.backend.as_ref().expect("read() but no media inserted");
        let start_offset = block_offset * blocksize;
        let image_end_offset =
            std::cmp::min((block_offset + block_count) * blocksize, backend.byte_len());

        let mut result = backend.read_bytes(start_offset, image_end_offset - start_offset);
        // CD-ROM images may not be exactly aligned on block size
        // Pad the end to a full block size
        result.resize(block_count * blocksize, 0);
        result
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {
        log::error!("Write command to CD-ROM");
    }

    fn image_fn(&self) -> Option<&Path> {
        self.backend
            .as_ref()
            .and_then(|backend| backend.image_path())
    }

    fn specific_cmd(&mut self, cmd: &[u8], _outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        match cmd[0] {
            // READ(6) (no media)
            0x08 => {
                self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
            // START/STOP UNIT
            0x1B => {
                // LoEj + !start = eject
                let eject = cmd[4] & 0b11 == 0b10;

                if eject {
                    self.eject_media();
                }

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            // PREVENT/ALLOW MEDIA REMOVAL
            0x1E => Ok(ScsiCmdResult::Status(STATUS_GOOD)),
            // READ SUB-CHANNEL
            0x42 => {
                let Some(backend) = &self.backend else {
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                let sub_q = (cmd[2] >> 5) & 0x1;
                let format = cmd[3];
                let track = cmd[6];
                let alloc_len = u16::from_be_bytes(cmd[7..=8].try_into()?) as usize;

                log::warn!("READ SUB-CHANNEL sub_q {} format {}, track {}, alloc_len {}", sub_q, format, track, alloc_len);

                let mut result = vec![];

                // Sub-channel data header (common to all formats)
                result.push(0); // Reserved
                result.push(backend.audio_status());
                result.push(0); // Sub-channel data length (filled later)
                result.push(0);

                if sub_q != 0 {
                    match format {
                        _ => {
                            log::warn!("Reading unknown sub-channel format {}", format);
                            self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                        }
                    }
                }

                let data_len = result.len() - 4;
                result[2..4].copy_from_slice(&u16::to_be_bytes(data_len.try_into()?));
                result.truncate(alloc_len);

                Ok(ScsiCmdResult::DataIn(result))

            }
            // READ TOC
            0x43 => {
                let msf = (cmd[1] >> 1) & 0x1;
                let format = cmd[9] >> 6;
                let control = cmd[9] & 0x3f;
                let track = cmd[6];
                let alloc_len = u16::from_be_bytes(cmd[7..=8].try_into()?) as usize;

                log::warn!("READ TOC msf {} format {} control {} track {} alloc_len {}", msf, format, control, track, alloc_len);

                self.read_toc(msf != 0, format, track, alloc_len)
            }
            // VENDOR SPECIFIC (EJECT)
            0xC0 => {
                self.eject_media();
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            _ => {
                log::error!("Unknown command {:02X}", cmd[0]);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }

    fn ms_density(&self) -> u8 {
        1 // User data only, 2048 bytes
    }

    fn ms_media_type(&self) -> u8 {
        2 // 120mm CD-ROM
    }

    fn ms_device_specific(&self) -> u8 {
        0
    }

    fn set_cc(&mut self, code: u8, asc: u16) {
        self.cc_code = code;
        self.cc_asc = asc;
    }

    fn set_blocksize(&mut self, blocksize: usize) -> bool {
        // FIXME: Do CD-ROM drives really allow the block size to be modified by software?
        //
        // [PIONEER]:
        //
        // The value of Logical Block Length returned depends on the block length set with a MODE
        // SELECT command. The default value of the block length is 2048 bytes. The CD-ROM drives
        // allow values of 2048 or 512 bytes to be set with an external switch on the drive.

        if Self::VALID_BLOCKSIZES.contains(&blocksize) {
            self.blocksize = blocksize;
            return true;
        }
        false
    }

    #[cfg(feature = "savestates")]
    fn after_deserialize(&mut self, imgfn: &Path) -> Result<()> {
        self.load_media(imgfn)?;
        Ok(())
    }

    fn branch_media(&mut self, _path: &Path) -> Result<()> {
        bail!("Unsupported on CD-ROM");
    }

    #[cfg(feature = "ethernet")]
    fn eth_set_link(&mut self, _link: super::ethernet::EthernetLinkType) -> Result<()> {
        unreachable!()
    }

    #[cfg(feature = "ethernet")]
    fn eth_link(&self) -> Option<super::ethernet::EthernetLinkType> {
        None
    }
}

impl Debuggable for ScsiTargetCdrom {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        vec![]
    }
}
