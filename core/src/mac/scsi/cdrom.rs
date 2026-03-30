//! SCSI CD-ROM drive (block device)

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::iter::Peekable;
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::str::Chars;

use crate::debuggable::Debuggable;
use crate::mac::macii::bus::CLOCK_SPEED;
use crate::tickable::Ticks;
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

// The CD-ROM SCSI protocol is often confusing. Here is some useful documentation:
//
// [PIONEER]: <https://bitsavers.trailing-edge.com/pdf/pioneer/cdrom/OB-U0077C_CD-ROM_SCSI-2_Command_Set_V3.1_19970626.pdf>
// [UNI-MAINZ]: <https://www.staff.uni-mainz.de/tacke/scsi/SCSI2-14.html>
// [MBWIKI]: <https://wiki.musicbrainz.org/Disc_ID_Calculation>
// [LIBODRAW]: <https://github.com/libyal/libodraw/blob/main/documentation/CUE%20sheet%20format.asciidoc>

const RAW_SECTOR_LEN: usize = 2352;
const TRACK_LEADOUT: u8 = 0xAA;

// Audio status codes
//
// [PIONEER] Table 2-27C: Audio Status
const AUDIO_PLAYING: u8 = 0x11;
const AUDIO_PAUSED: u8 = 0x12;
const AUDIO_COMPLETED: u8 = 0x13;

// Track ADR/Control field codes
const AUDIO_TRACK: u8 = 0x00; // FIXME: correct?
const DATA_TRACK: u8 = 0x14;

const AUDIO_SECTORS_PER_SEC: u64 = 75;

fn msf_to_sector(m: u8, s: u8, f: u8) -> u32 {
    // FIXME: Is 00:02:00 pregap involved here?
    (m as u32 * 60 + s as u32) * 75 + f as u32
}

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

    /// Return a list of tracks in the table of contents.
    ///
    /// Track numbers are not required to start at 1, but they must increase during iteration.
    /// The final track is the leadout track, numbered 0xAA.
    fn tracks(&self) -> Option<&[TrackInfo]>;
}

struct IsoCdromBackend {
    image: Box<dyn DiskImage>,
    tracks: [TrackInfo; 2],
}

impl IsoCdromBackend {
    fn new(image: Box<dyn DiskImage>) -> Result<Self> {
        let leadout_sector = image.byte_len().div_ceil(2048).try_into()?;
        Ok(Self {
            image,
            tracks: [
                TrackInfo {
                    number: 1,
                    adr_control: DATA_TRACK,
                    sector: 0,
                },
                TrackInfo {
                    number: TRACK_LEADOUT,
                    adr_control: DATA_TRACK,
                    sector: leadout_sector,
                },
            ],
        })
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

    fn tracks(&self) -> Option<&[TrackInfo]> {
        Some(&self.tracks)
    }
}

fn skip_whitespace(reader: &mut Peekable<Chars>) {
    while reader.peek().map(|c| c.is_whitespace()).unwrap_or(false) {
        reader.next();
    }
}

fn read_cue_word(reader: &mut Peekable<Chars>) -> Option<String> {
    skip_whitespace(reader);

    reader.peek()?;

    let mut result = String::new();

    while reader.peek().map(|c| !c.is_whitespace()).unwrap_or(false) {
        result.push(reader.next().unwrap());
    }

    Some(result)
}

fn read_cue_path(reader: &mut Peekable<Chars>) -> Option<String> {
    skip_whitespace(reader);

    reader.peek()?;

    if *reader.peek().unwrap() == '"' {
        reader.next();

        let mut result = String::new();

        while reader.peek().map(|c| *c != '"').unwrap_or(false) {
            result.push(reader.next().unwrap());
        }

        // Skip final '"' if present
        if reader.peek().is_some() {
            reader.next();
        }

        Some(result)
    } else {
        read_cue_word(reader)
    }
}

struct CuesheetCdromBackend {
    cue_path: PathBuf,
    data_files: Vec<File>,
}

impl CuesheetCdromBackend {
    fn new(path: &Path) -> Result<Self> {
        let cue_dir = path.parent().unwrap();
        let cue_file = BufReader::new(File::open(path)?);
        let mut data_files = vec![];

        // FIXME: I believe cue files have one command per line and never split commands across multiple lines. Is this true?
        for line in cue_file.lines() {
            let line = line?;
            let mut chars = line.chars().peekable();

            if let Some(command) = read_cue_word(&mut chars) {
                match command.as_str() {
                    "FILE" => {
                        let data_file_path = read_cue_path(&mut chars)
                            .ok_or_else(|| anyhow!("Failed to parse FILE command"))?;
                        let data_file_path = cue_dir.join(Path::new(&data_file_path));
                        println!("Loading datafile from {}", data_file_path.to_string_lossy());
                        let data_file = File::open(data_file_path)?;
                        data_files.push(data_file);

                        let file_type = read_cue_word(&mut chars)
                            .ok_or_else(|| anyhow!("Failed to parse FILE command"))?;
                        if file_type != "BINARY" {
                            bail!("Unsupported data file type in cuesheet");
                        }
                    }
                    _ => log::warn!("Unknown cuesheet command {} ignored", command),
                }
            }
        }

        Ok(Self {
            cue_path: path.into(),
            data_files,
        })
    }

    fn read_raw_sector(&self, sector: u32) -> Result<[u8; RAW_SECTOR_LEN]> {
        let mut result = [0; RAW_SECTOR_LEN];
        // FIXME: Implement multiple data files
        self.data_files
            .first()
            .ok_or_else(|| anyhow!("No data files loaded"))?
            .seek_read(&mut result, (sector * RAW_SECTOR_LEN as u32).into())?;
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
                sector: 300_000,
            },
            TrackInfo {
                number: 3,
                adr_control: AUDIO_TRACK,
                sector: 305_000,
            },
            TrackInfo {
                number: 4,
                adr_control: AUDIO_TRACK,
                sector: 310_000,
            },
            TrackInfo {
                number: 5,
                adr_control: AUDIO_TRACK,
                sector: 315_000,
            },
            TrackInfo {
                number: 6,
                adr_control: AUDIO_TRACK,
                sector: 320_000,
            },
            TrackInfo {
                number: TRACK_LEADOUT,
                adr_control: DATA_TRACK,
                sector: 330_000,
            },
        ])
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
enum AudioState {
    Stopped,
    Paused,
    Playing,
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

    /// Audio state
    audio_state: AudioState,

    /// Current audio sector
    audio_pos: u32,

    /// Audio stop sector
    audio_stop: u32,

    /// Audio clock (counts ticks up 75 audio CD frames per second)
    audio_clock: Ticks,
}

impl Default for ScsiTargetCdrom {
    fn default() -> Self {
        Self {
            backend: None,
            cc_code: 0,
            cc_asc: 0,
            event_eject: Default::default(),
            blocksize: 2048,
            audio_state: AudioState::Stopped,
            audio_pos: 0,
            audio_stop: 0,
            audio_clock: 0,
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
                0,                     // Reserved
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

    fn read_toc(
        &mut self,
        msf: bool,
        format: u8,
        track: u8,
        alloc_len: usize,
    ) -> Result<ScsiCmdResult> {
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
                result.push(tracks.iter().rev().nth(1).unwrap().number); // Last Track Number

                // Start at the given track or the next available track
                let track_iter = tracks.iter().skip_while(|t| t.number < track);

                // Emit track descriptors
                for t in track_iter {
                    result.push(0); // Reserved
                    result.push(t.adr_control); // ADR/Control
                    result.push(t.number); // Track Number
                    result.push(0); // Reserved
                    result.extend_from_slice(&self.sector_to_address_field(t.sector, msf));
                    // Absolute CD-ROM Address
                }

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
                let first_track = tracks.first().unwrap();

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

    fn get_track_at_sector(&self, sector: u32) -> Option<&TrackInfo> {
        self.backend
            .as_ref()?
            .tracks()?
            .iter()
            .rev()
            .find(|t| t.sector <= sector)
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
        if path
            .extension()
            .map(|ext| ext.eq_ignore_ascii_case("cue"))
            .unwrap_or(false)
        {
            self.load_cue(path)
        } else {
            // Assume image is iso or toast
            self.load_image(Box::new(FileDiskImage::open(path, false)?))
        }
    }

    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()> {
        self.backend = Some(Box::new(IsoCdromBackend::new(image)?));
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
            // REZERO UNIT
            0x01 => {
                // Used by Apple Audio CD Player when user presses Stop.
                //
                // [PIONEER] 2.33:
                //
                // The drive loads the specified logical unit (if necessary), spins up the disc (if stopped), moves the
                // head to the start track of the disc, and holds it there until an inactivity time-out occurs. If the
                // initiator requests a disconnect, the drive disconnects from it during load and seek operations.
                // This command does not affect modes specified by the MODE SELECT command.
                log::warn!("REZERO UNIT");
                self.audio_state = AudioState::Stopped;
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
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
                // Used by Apple Audio CD Player to query playback status.
                let Some(backend) = &self.backend else {
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                let Some(tracks) = backend.tracks() else {
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                let msf = (cmd[1] >> 1) & 0x1;
                let sub_q = (cmd[2] >> 5) & 0x1;
                let format = cmd[3];
                let track = cmd[6];
                let alloc_len = u16::from_be_bytes(cmd[7..=8].try_into()?) as usize;

                log::warn!(
                    "READ SUB-CHANNEL msf {} sub_q {} format {} track {} alloc_len {}",
                    msf,
                    sub_q,
                    format,
                    track,
                    alloc_len
                );

                let audio_status = match self.audio_state {
                    AudioState::Stopped => AUDIO_COMPLETED,
                    AudioState::Paused => AUDIO_PAUSED,
                    AudioState::Playing => AUDIO_PLAYING,
                };

                let mut result = vec![
                    // [PIONEER] Table 2-27A: Sub-channel data header (common to all formats)
                    0, // Reserved
                    audio_status,
                    0, // Sub-channel data length (will be set later)
                    0,
                ];

                if sub_q != 0 {
                    // TODO: Implement sub-channel data block stuff
                    log::warn!("Reading unknown sub_q stuff, format {}", format);
                    self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                match format {
                    0x01 => {
                        // CD-ROM Current Position
                        // Find current track at playback position
                        let Some(track) = self.get_track_at_sector(self.audio_pos) else {
                            // FIXME: correct?
                            self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                        };

                        log::warn!(
                            "audio pos is at sector {} (in track #{} at sector {})",
                            self.audio_pos,
                            track.number,
                            track.sector
                        );

                        // [PIONEER] Table 2-27F: CD-ROM Current Position Data Block
                        result.push(0x01); // Sub Channel Data Format code
                        result.push(track.adr_control); // ADR/Control
                        result.push(track.number); // Track Number
                        result.push(1); // Index Number (TODO: Find correct index number)
                        result.extend_from_slice(
                            &self.sector_to_address_field(self.audio_pos, msf != 0),
                        );
                        let track_relative = self.audio_pos - track.sector;
                        result.extend_from_slice(
                            &self.sector_to_address_field(track_relative, msf != 0),
                        );
                    }
                    _ => {
                        // TODO: Implement sub-channel data block stuff
                        log::warn!("Reading unknown sub-channel format {}", format);
                        self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                        return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
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

                if control != 0 {
                    log::warn!("Unimplemented READ TOC control 0x{:X}", control);
                }

                self.read_toc(msf != 0, format, track, alloc_len)
            }
            // PLAY AUDIO MSF
            0x47 => {
                let (start_m, start_s, start_f) = (cmd[3], cmd[4], cmd[5]);
                let (end_m, end_s, end_f) = (cmd[6], cmd[7], cmd[8]);

                log::warn!(
                    "PLAY AUDIO MSF start {}:{}:{} end {}:{}:{}",
                    start_m,
                    start_s,
                    start_f,
                    end_m,
                    end_s,
                    end_f
                );

                let start_sector = msf_to_sector(start_m, start_s, start_f);
                let stop_sector = msf_to_sector(end_m, end_s, end_f);

                // [PIONEER]: 2.13:
                // If the starting address is not found, or if the address is not within an audio track, or if a not ready
                // condition exists, the drive will terminate with a Check Condition status.
                let start_track = self.get_track_at_sector(start_sector);
                if start_track
                    .map(|t| t.adr_control != AUDIO_TRACK)
                    .unwrap_or(false)
                {
                    self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                self.audio_pos = start_sector;
                self.audio_stop = stop_sector;
                self.audio_clock = 0;
                self.audio_state = AudioState::Playing;

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            // PAUSE/RESUME
            0x4B => {
                let resume = cmd[8] & 0x1;

                log::warn!("PAUSE/RESUME resume {}", resume);

                // FIXME: What happens if pause/resume is activated while no track is playing?
                self.audio_state = if resume != 0 {
                    AudioState::Playing
                } else {
                    AudioState::Paused
                };

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            // VENDOR SPECIFIC (EJECT)
            0xC0 => {
                self.eject_media();
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            // AUDIO SCAN (1)
            0xCD => {
                // Also known as fast-forward or rewind.

                let Some(backend) = &self.backend else {
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                let Some(tracks) = backend.tracks() else {
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                let direct = (cmd[1] >> 4) & 0x1; // Scan forwards if set, backwards if unset.
                let addr_type = (cmd[9] >> 6) & 0x3;
                let start_addr = &cmd[2..=5];
                log::warn!(
                    "AUDIO SCAN (1) direct {} addr_type {} start_addr {:?}",
                    direct,
                    addr_type,
                    start_addr
                );

                // Convert start address to sector
                let _start_addr = match addr_type {
                    // Logical Block Address
                    0b00 => u32::from_be_bytes(start_addr.try_into()?),
                    // CD absolute time
                    0b01 => msf_to_sector(start_addr[1], start_addr[2], start_addr[3]),
                    // Track Number
                    0b10 => {
                        // Start at the given track or the next available track
                        // FIXME: what should happen if specified track is not available?
                        // TODO: avoid unwrap
                        let track = tracks.iter().find(|t| t.number >= start_addr[3]).unwrap();
                        track.sector
                    }
                    // Reserved
                    _ => {
                        self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                        return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                    }
                };

                // TODO: implement audio scan
                //
                // [PIONEER] 2.1:
                //
                // When AUDIO SCAN (1) is executed, the drive begins a high-speed scan from the Scan Start
                // Address. The drive plays a block as it crosses each track. Each scan is approximately 15 seconds.

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

    fn tick(&mut self, ticks: Ticks) -> Result<()> {
        if self.audio_state == AudioState::Playing {
            self.audio_clock += ticks;
            if self.audio_clock >= CLOCK_SPEED / AUDIO_SECTORS_PER_SEC {
                self.audio_clock -= CLOCK_SPEED / AUDIO_SECTORS_PER_SEC;

                self.audio_pos += 1;
                if self.audio_pos >= self.audio_stop {
                    self.audio_state = AudioState::Stopped;
                }
            }
        }

        Ok(())
    }
}

impl Debuggable for ScsiTargetCdrom {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        vec![]
    }
}
