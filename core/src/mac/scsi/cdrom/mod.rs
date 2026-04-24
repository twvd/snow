//! SCSI CD-ROM drive (block device)

pub mod backends;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::emulator::comm::EmulatorSpeed;
use crate::mac::macii::bus::CLOCK_SPEED;
use crate::mac::scsi::cdrom::backends::cuesheet::CuesheetCdromBackend;
use crate::mac::scsi::cdrom::backends::iso::IsoCdromBackend;
use crate::mac::scsi::cdrom::backends::{
    is_physical_cdrom_drive_path, new_physical_cdrom_drive_backend,
};
use crate::mac::scsi::target::ScsiTargetCommon;
use crate::mac::scsi::{
    ASC_ILLEGAL_MODE_FOR_THIS_TRACK, ASC_INVALID_COMMAND, ASC_LOGICAL_BLOCK_ADDRESS_OUT_OF_RANGE,
    ASC_UNRECOVERED_READ_ERROR,
};
use crate::renderer::{AUDIO_BUFFER_SAMPLES, AudioProvider, AudioSink};
use crate::tickable::Ticks;
use crate::types::LatchingEvent;

use super::ASC_INVALID_FIELD_IN_CDB;
use super::ASC_MEDIUM_NOT_PRESENT;
use super::CC_KEY_ILLEGAL_REQUEST;
use super::CC_KEY_MEDIUM_ERROR;
use super::STATUS_CHECK_CONDITION;
use super::STATUS_GOOD;
use super::ScsiCmdResult;
use super::disk_image::{DiskImage, FileDiskImage};
use super::target::ScsiTarget;
use super::target::ScsiTargetEvent;
use super::target::ScsiTargetType;

// CD-ROM protocol Documentation:
//
// [MMC3]: <https://13thmonkey.org/documentation/SCSI/mmc3r10g.pdf>
// [MMC4]: <https://13thmonkey.org/documentation/SCSI/mmc4r05a.pdf>
// [MMC6]: <https://13thmonkey.org/documentation/SCSI/mmc6r02g.pdf>
// [PIONEER]: <https://bitsavers.trailing-edge.com/pdf/pioneer/cdrom/OB-U0077C_CD-ROM_SCSI-2_Command_Set_V3.1_19970626.pdf>
// [MBWIKI]: <https://wiki.musicbrainz.org/Disc_ID_Calculation>

const RAW_SECTOR_LEN: usize = 2352;
const TRACK_LEADOUT: u8 = 0xAA;

// Audio status codes
//
// [PIONEER] Table 2-27C: Audio Status
const AUDIO_PLAYING: u8 = 0x11;
const AUDIO_PAUSED: u8 = 0x12;
const AUDIO_COMPLETED: u8 = 0x13;

// Track Control field codes
const AUDIO_TRACK: u8 = 0x0;
const DATA_TRACK: u8 = 0x4;

/// MSF timecode of Logical Block Address 0
const LBA_START_MSF: Msf = Msf::new(0, 2, 0);
/// Absolute sector number of Logical Block Address 0
const LBA_START_SECTOR: u32 = LBA_START_MSF.to_sector();

/// Number of sectors per second of audio
const AUDIO_SECTORS_PER_SEC: u32 = 75;

fn bin_to_bcd(bin: u8) -> u8 {
    ((bin / 10) << 4) | (bin % 10)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Msf {
    m: u8,
    s: u8,
    f: u8,
}

impl Msf {
    const fn new(m: u8, s: u8, f: u8) -> Self {
        Self { m, s, f }
    }

    fn from_bytes(bytes: [u8; 3]) -> Self {
        Self::new(bytes[0], bytes[1], bytes[2])
    }

    fn to_bytes(self) -> [u8; 3] {
        [self.m, self.s, self.f]
    }

    fn to_bcd_bytes(self) -> [u8; 3] {
        [self.m, self.s, self.f].map(bin_to_bcd)
    }

    fn from_sector(mut sector: u32) -> Result<Self> {
        let f = sector % AUDIO_SECTORS_PER_SEC;
        sector /= AUDIO_SECTORS_PER_SEC;
        let s = sector % 60;
        sector /= 60;
        let m = sector;
        Ok(Self::new(m.try_into()?, s.try_into()?, f.try_into()?))
    }

    const fn to_sector(self) -> u32 {
        (self.m as u32 * 60 + self.s as u32) * AUDIO_SECTORS_PER_SEC + self.f as u32
    }
}

impl std::fmt::Display for Msf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.m, self.s, self.f)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TrackInfo {
    /// The track number. Note that tracks don't necessarily start at number 1.
    tno: u8,
    /// The session number where this track resides
    session: u8,
    /// Control field indicating track format
    control: u8,
    /// Absolute sector number where the track begins
    sector: u32, // Forget about MSF/LBA; use absolute sector numbers wherever possible.
}

#[derive(Debug)]
pub struct SessionInfo {
    /// Session number. Always starts at 1.
    number: u8,
    /// Value to put in Disc Type field ([MMC4] Table 448)
    disc_type: u8,
    /// Absolute sector number of lead-in
    leadin: u32,
    /// Absolute sector number of lead-out
    leadout: u32,
}

pub enum CdromError {
    /// SCSI read error with CC, ASC/ASCQ
    CheckCondition(u8, u16),
    /// Any other type of error
    Other(anyhow::Error),
}

impl From<anyhow::Error> for CdromError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(value)
    }
}

pub struct RawSector {
    data: [u8; RAW_SECTOR_LEN],
    /// CONTROL field of sub-channel data. Indicates audio or data track.
    control: u8,
    // TODO: add fields for other sub-channel data (position, track/index number, etc.)
}

pub trait CdromBackend: Send {
    /// Tells the backend to check whether a disc is still in the drive and reload TOC's
    /// if necessary. Returns Ok(true) if a disc is present, Ok(false) if a disc is
    /// not present, or Err(e) if an error occurred.
    fn check_media(&mut self) -> Result<bool>;

    fn byte_len(&self) -> usize;

    /// The SCSI CD-ROM protocol allows reading blocks with standard READ commands.
    ///
    /// Each sector accessed by this method is expected to be a Mode 1 or Mode 2 Form 1
    /// sector containing 2048 bytes of user data. If the wrong type of sector is accessed,
    /// it fails with an ILLEGAL MODE FOR THIS TRACK error.
    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError>;

    fn image_path(&self) -> Option<&Path>;

    fn sessions(&self) -> Option<&[SessionInfo]>;
    fn tracks(&self) -> Option<&[TrackInfo]>;

    /// Read a raw 2352-byte sector. Currently used only for CD audio. Other data is read via read_bytes.
    fn read_raw_sector(&self, sector: u32) -> Result<RawSector, CdromError>;
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
enum AudioState {
    Stopped,
    Paused,
    Playing,
}

#[derive(Serialize, Deserialize)]
struct AudioPort {
    channel: u8,
    volume: u8,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ScsiTargetCdrom {
    common: ScsiTargetCommon,

    /// Disk contents
    #[serde(skip)]
    pub(super) backend: Option<Box<dyn CdromBackend>>,

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

    /// Audio ports: Left, Right, Rear Left, Rear Right.
    /// Quadraphonic CD's are extremely rare but apparently existed.
    audio_ports: [AudioPort; 4],

    /// Audio clock (counts bus ticks)
    audio_clock: Ticks,

    /// Audio sink for CD audio
    #[serde(skip)]
    audio_sink: Option<Box<dyn AudioSink>>,
}

impl ScsiTargetCdrom {
    const VALID_BLOCKSIZES: [usize; 2] = [512, 2048];

    pub fn new(audio_provider: Option<&mut (dyn AudioProvider + '_)>) -> Self {
        let mut self_ = Self {
            common: Default::default(),
            backend: None,
            event_eject: Default::default(),
            blocksize: 2048,
            audio_state: AudioState::Stopped,
            audio_pos: LBA_START_SECTOR,
            audio_stop: 0,
            audio_clock: 0,
            audio_ports: [
                AudioPort {
                    volume: 255,
                    channel: 0b0001,
                },
                AudioPort {
                    volume: 255,
                    channel: 0b0010,
                },
                AudioPort {
                    volume: 0,
                    channel: 0b0100,
                },
                AudioPort {
                    volume: 0,
                    channel: 0b1000,
                },
            ],
            audio_sink: None,
        };
        if let Some(audio_provider) = audio_provider {
            self_.set_audio_provider(audio_provider).unwrap(); // FIXME: avoid unwrap
        }
        self_
    }

    fn msf_to_address_field(&self, msf: Msf, msf_format: bool) -> [u8; 4] {
        if msf_format {
            // [UNI-MAINZ] Table 237: MSF address format
            [0, msf.m, msf.s, msf.f]
        } else {
            // FIXME: is this correct? I can't find any software that sets a non-2048 blocksize.
            let lba =
                (msf.to_sector() as i32 - LBA_START_SECTOR as i32) * 2048 / self.blocksize as i32;
            // [PIONEER] seems to imply that LBA numbers can be a negative signed integer.
            // 2.15 (on Track Relative Logical Block Address field):
            // Negative values indicate a starting location within the audio pause area at the
            // beginning of the requested track.
            i32::to_be_bytes(lba)
        }
    }

    fn read_toc(
        &mut self,
        msf: bool,
        format: u8,
        track_or_session: u8, // Interpretation depends on format
        alloc_len: usize,
    ) -> Result<ScsiCmdResult> {
        let Some(backend) = &mut self.backend else {
            // No CD inserted
            self.common
                .set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        };

        if !backend.check_media()? {
            // No CD inserted
            self.common
                .set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        }

        // Get rid of mutable reference, we only needed it for check_media
        let backend = self.backend.as_ref().unwrap();

        let Some(tracks) = backend.tracks() else {
            // Media does not support tracks
            //
            // [PIONEER] 2.28:
            // If the Start Track field is not valid for the currently installed medium, the command shall be
            // terminated with Check Condition status. The sense key shall be set to ILLEGAL REQUEST and
            // the additional sense code set to INVALID FIELD IN CDB.
            self.common
                .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        };

        let sessions = backend.sessions().unwrap();

        match format {
            0 => {
                // Formatted TOC
                let mut result = Vec::<u8>::with_capacity(alloc_len);

                result.push(0); // TOC Data Length (will be set later)
                result.push(0);

                result.push(
                    tracks
                        .first()
                        .ok_or_else(|| anyhow!("Track not found"))?
                        .tno,
                ); // First Track Number
                result.push(tracks.last().ok_or_else(|| anyhow!("Track not found"))?.tno); // Last Track Number

                // Start at the given track or the next available track
                let track_iter = tracks.iter().skip_while(|t| t.tno < track_or_session);

                // Emit track descriptors
                for t in track_iter {
                    result.push(0); // Reserved
                    result.push((1 << 4) | t.control); // ADR/Control
                    result.push(t.tno); // Track Number
                    result.push(0); // Reserved
                    result.extend_from_slice(
                        &self.msf_to_address_field(Msf::from_sector(t.sector)?, msf),
                    );
                    // Absolute CD-ROM Address
                }

                // Emit leadout descriptor
                result.push(0); // Reserved
                result.push((1 << 4) | tracks.last().unwrap().control); // ADR/Control
                result.push(TRACK_LEADOUT); // Track Number
                result.push(0); // Reserved
                result.extend_from_slice(&self.msf_to_address_field(
                    Msf::from_sector(sessions.last().unwrap().leadout)?,
                    msf,
                ));

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

                result.push(sessions.first().unwrap().number); // First Session Number (always 1)
                result.push(sessions.last().unwrap().number); // Last Session Number

                // This command queries the first track in the last session.
                let last_session_no = sessions.last().unwrap().number;
                let first_track_of_last_session = tracks
                    .iter()
                    .find(|t| t.session >= last_session_no)
                    .ok_or_else(|| anyhow!("No tracks in last session"))?;

                // [PIONEER] Table 2-28D: Track Descriptors
                result.push(0); // Reserved
                result.push((0x1 << 4) | first_track_of_last_session.control); // ADR/Control
                result.push(first_track_of_last_session.tno); // First Track Number in Last Session
                result.push(0); // Reserved
                result.extend_from_slice(&self.msf_to_address_field(
                    Msf::from_sector(first_track_of_last_session.sector)?,
                    msf,
                )); // Absolute CD-ROM Address of the First Track in the Last Session

                let data_length = result.len() - 2;
                result[0..2].copy_from_slice(&u16::to_be_bytes(data_length.try_into()?));

                result.truncate(alloc_len);
                Ok(ScsiCmdResult::DataIn(result))
            }
            2 => {
                // Raw TOC
                let mut result = Vec::<u8>::with_capacity(alloc_len);

                result.push(0); // TOC Data Length (will be filled later)
                result.push(0);

                result.push(sessions.first().unwrap().number); // First Complete Session Number (always 1)
                result.push(sessions.last().unwrap().number); // Last Complete Session Number

                // track_or_session argument is the session number to start at.
                let track_iter = tracks.iter().skip_while(|t| t.session < track_or_session);
                let mut session_no = 0;

                let get_session = |num: u8| {
                    (num as usize)
                        .checked_sub(1)
                        .and_then(|num| sessions.get(num))
                };

                for track in track_iter {
                    if track.session != session_no {
                        if session_no != 0 {
                            // Emit descriptors for session gap
                            // XXX: info based on Weird Al - Running with Scissors; other discs may have different session gap descriptors.

                            // Start time of next possible program
                            let next_session = get_session(session_no + 1).unwrap();
                            result.push(session_no); // Session Number
                            result.push(5 << 4); // ADR; Control
                            result.push(0); // TNO (0 for the lead-in area)
                            result.push(0xB0); // POINT (Start time of next possible program)
                            result.extend_from_slice(
                                // FIXME: It's unclear whether this should be BCD or binary.
                                // It probably makes no difference.
                                &Msf::from_sector(next_session.leadin)?.to_bytes(),
                            ); // Start time of next possible program
                            result.push(2); // # of pointers in Mode 5
                            result.extend_from_slice(
                                &Msf::from_sector(sessions.last().unwrap().leadout)?.to_bcd_bytes(),
                            ); // Maximum start time of outer-most Lead-out area

                            // Start time of the first Lead-in Area of the disc
                            result.push(session_no); // Session Number
                            result.push(5 << 4); // ADR; Control
                            result.push(0); // TNO (0 for the lead-in area)
                            result.push(0xC0); // POINT (Start time of the first Lead-in Area of the disc)
                            result.push(0); // ATIME (0:0:0 for the lead-in area)
                            result.push(0);
                            result.push(0);
                            result.push(0); // Zero
                            // FIXME: Weird Al actually puts 95:00:00 here? where does that come from?
                            result.extend_from_slice(
                                &Msf::from_sector(sessions.first().unwrap().leadin)?.to_bcd_bytes(),
                            ); // Start time of the first Lead-in Area of the disc)
                        }

                        session_no = track.session;
                        let session = get_session(session_no).unwrap();

                        // First track number in the program area
                        result.push(session_no); // Session Number
                        result.push((1 << 4) | track.control);
                        result.push(0); // TNO (0 for the lead-in area)
                        result.push(0xA0); // POINT (First Track number in the program area)
                        result.push(0); // ATIME (0:0:0 for the lead-in area)
                        result.push(0);
                        result.push(0);
                        result.push(0); // Zero
                        result.push(bin_to_bcd(track.tno)); // First Track Number
                        result.push(bin_to_bcd(session.disc_type)); // Disc Type
                        result.push(0);

                        // Last track number in the program area
                        result.push(session_no); // Session Number
                        result.push((1 << 4) | track.control);
                        result.push(0); // TNO (0 for the lead-in area)
                        result.push(0xA1); // POINT (Last Track number in the program area)
                        result.push(0); // ATIME (0:0:0 for the lead-in area)
                        result.push(0);
                        result.push(0);
                        result.push(0); // Zero
                        let last_track_in_session = tracks
                            .iter()
                            .rev()
                            .find(|t| t.session == session_no)
                            .unwrap();
                        result.push(bin_to_bcd(last_track_in_session.tno)); // Last Track Number
                        result.push(0);
                        result.push(0);

                        // Start location of the Lead-out area
                        result.push(session_no); // Session Number
                        result.push((1 << 4) | track.control);
                        result.push(0); // TNO (0 for the lead-in area)
                        result.push(0xA2); // POINT (Start location of the Lead-out area)
                        result.push(0); // ATIME (0:0:0 for the lead-in area)
                        result.push(0);
                        result.push(0);
                        result.push(0); // Zero
                        let leadout = Msf::from_sector(get_session(session_no).unwrap().leadout)?;
                        result.extend_from_slice(&leadout.to_bcd_bytes()); // Start position of Lead-out
                    }

                    result.push(track.session); // Session Number
                    result.push((1 << 4) | track.control); // ADR/Control
                    result.push(0); // TNO (0 for the lead-in area)
                    result.push(track.tno); // POINT (0-99 for tracks)
                    result.push(0); // ATIME (0:0:0 for the lead-in area)
                    result.push(0);
                    result.push(0);
                    result.push(0); // Zero
                    // [MMC4] 6.40.3.4.1:
                    // Entries in bytes 2 through 7 of the descriptors (TNO, POINT, MIN, SEC, FRAME, ZERO) shall be
                    // converted to binary by the Logical Unit when the media contains a value between 0 and 99bcd.
                    // [...] Otherwise, the value is returned with no modification.
                    // Note that this does NOT include the PMIN/PSEC/PFRAME fields! Meaning, these fields must
                    // be represented in binary-coded decimal.
                    result.extend_from_slice(&Msf::from_sector(track.sector)?.to_bcd_bytes()); // Start position of track
                }

                let data_length = result.len() - 2;
                result[0..2].copy_from_slice(&u16::to_be_bytes(data_length.try_into()?));

                result.truncate(alloc_len);
                Ok(ScsiCmdResult::DataIn(result))
            }
            _ => {
                log::error!("Unknown READ TOC format: {}", format);

                self.common
                    .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }

    fn eject_media(&mut self) {
        self.event_eject.set();
        self.backend = None;
        self.audio_state = AudioState::Stopped;
        self.audio_pos = LBA_START_SECTOR;
    }

    /// Read a frame of CD audio and send it to the audio sink.
    fn pump_audio(&mut self) -> Result<()> {
        let audio_sink = self.audio_sink.as_ref().unwrap();

        if audio_sink.is_full() {
            return Ok(());
        }

        let Some(backend) = &self.backend else {
            return Ok(());
        };

        // Keep audio clock as 0 if real audio is active
        self.audio_clock = 0;

        if self.audio_state != AudioState::Playing {
            return Ok(());
        }

        if self.audio_pos >= self.audio_stop {
            self.audio_state = AudioState::Stopped;
            return Ok(());
        }

        let make_out_sample = |port: &AudioPort, channel_samples: &[i16; 4]| -> f32 {
            let sample = match port.channel {
                0b0001 => channel_samples[0],
                0b0010 => channel_samples[1],
                0b0100 => channel_samples[2],
                0b1000 => channel_samples[3],
                _ => 0,
            };
            sample as f32 / 32768.0 * port.volume as f32 / 255.0
        };

        let samples = backend.read_raw_sector(self.audio_pos);
        match samples {
            Ok(samples) if samples.control == AUDIO_TRACK => {
                let mut samples = samples
                    .data
                    .chunks_exact(2)
                    .map(|s| i16::from_le_bytes(s.try_into().unwrap()));
                // FIXME: can we avoid converting to float by setting up a signed 16-bit PCM audio sink?
                let mut out_samples = [0.0; RAW_SECTOR_LEN / 2]; // 16-bit samples
                for out_samples in out_samples.chunks_exact_mut(2) {
                    let left = samples.next().unwrap();
                    let right = samples.next().unwrap();
                    // FIXME: support rear-left/rear-right? Four-channel audio is indicated
                    // by a flag in the track form field.
                    let channel_samples = [left, right, 0, 0];
                    out_samples[0] = make_out_sample(&self.audio_ports[0], &channel_samples);
                    out_samples[1] = make_out_sample(&self.audio_ports[1], &channel_samples);
                }

                audio_sink.send(Box::new(out_samples))?;
            }
            Ok(_) => {
                log::warn!("Tried to play from non-audio track");
                self.audio_state = AudioState::Stopped;
            }
            Err(CdromError::CheckCondition(_, _)) => {
                // CD-ROM error while playing; disable playback
                self.audio_state = AudioState::Stopped;
            }
            Err(CdromError::Other(e)) => {
                log::warn!(
                    "Failed to read raw samples from sector {}: {}",
                    self.audio_pos,
                    e
                );
                self.audio_state = AudioState::Stopped;
            }
        }

        self.audio_pos += 1;

        Ok(())
    }

    /// Read a frame of CD audio and send it to the audio sink.
    /// Returns None if audio is disabled (such as by running at Uncapped speed).
    ///
    /// To ensure smooth audio playback, we run CD audio semi-independently
    /// of the emulated Mac. If audio is disabled, playback is simulated by
    /// counting bus ticks.
    fn try_pump_audio(&mut self, ctx: &dyn EmuContext) -> Option<Result<()>> {
        self.audio_sink.as_ref()?;

        match ctx.speed() {
            EmulatorSpeed::Accurate | EmulatorSpeed::Dynamic => (),
            _ => {
                // Don't pump audio in these speed modes
                return None;
            }
        }

        Some(self.pump_audio())
    }
}

fn get_track_at_sector(tracks: &[TrackInfo], sector: u32) -> Option<&TrackInfo> {
    tracks
        .iter()
        .rev()
        .find(|t| t.sector <= sector)
        // XXX: if sector is before the first track, just return the first track.
        // This occurs if Track 1 begins with an Index 0 pregap.
        .or_else(|| tracks.first())
}

impl ScsiTargetCdrom {
    fn load_cue(&mut self, path: &Path) -> Result<()> {
        self.backend = Some(Box::new(CuesheetCdromBackend::new(path)?));
        self.common.set_cc(0, 0);
        self.event_eject.get_clear();
        Ok(())
    }

    fn load_physical_drive(&mut self, path: &Path) -> Result<()> {
        self.backend = Some(new_physical_cdrom_drive_backend(path)?);
        self.common.set_cc(0, 0);
        self.event_eject.get_clear();
        Ok(())
    }

    fn get_track_at_sector(&self, sector: u32) -> Option<&TrackInfo> {
        let tracks = self.backend.as_ref()?.tracks()?;
        get_track_at_sector(tracks, sector)
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetCdrom {
    fn common(&mut self) -> &mut ScsiTargetCommon {
        &mut self.common
    }

    /// Try to load a disk image, given the filename of the image.
    ///
    /// This locks the file on disk and memory maps the file for use by
    /// the emulator for fast access and automatic writes back to disk,
    /// at the discretion of the operating system.
    fn load_media(&mut self, path: &Path) -> Result<()> {
        if is_physical_cdrom_drive_path(path) {
            self.load_physical_drive(path)
        } else if path
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
        self.common.set_cc(0, 0);
        self.event_eject.get_clear();
        Ok(())
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
            self.common
                .set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
            Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
        }
    }

    fn inquiry(&mut self, cmd: &[u8]) -> Result<ScsiCmdResult> {
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

        // Honour the initiator's allocation length (cmd[4] for 6-byte INQUIRY).
        let alloc = cmd.get(4).copied().unwrap_or(0) as usize;
        if alloc > 0 && alloc < result.len() {
            result.truncate(alloc);
        }
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
                // [MMC4] 7.6.2: CD Audio Control Page

                let sotc = 0;

                Some(vec![
                    (1 << 2) | (sotc << 1),      // IMMED (always 1), SOTC
                    0,                           // Reserved
                    0,                           // Reserved
                    0,                           // Reserved
                    75,                          // Obsolete (75)
                    75,                          // Obsolete (75)
                    self.audio_ports[0].channel, // CDDA Output Port 0 Channel Selection (attach audio channel 0)
                    self.audio_ports[0].volume,  // Output Port 0 Volume Default FFh
                    self.audio_ports[1].channel, // CDDA Output Port 1 Channel Selection (attach audio channel 1)
                    self.audio_ports[1].volume,  // Output Port 1 Volume Default FFh
                    self.audio_ports[2].channel, // CDDA Output Port 2 Channel Selection (attach audio channel 2)
                    self.audio_ports[2].volume,  // Output Port 2 Volume Default 00h
                    self.audio_ports[3].channel, // CDDA Output Port 3 Channel Selection (attach audio channel 3)
                    self.audio_ports[3].volume,  // Output Port 3 Volume Default 00h
                ])
            }
            0x2A => {
                // [MMC4] E.3.3: MM Capabilities and Mechanical Status Page

                // The page data, not including the first two bytes (page code and length).
                // [MMC4] E.3.3:
                // If a Logical Unit does not support high speed CD-R/RW recording, the Logical Unit should not
                // return mode page data after byte 26.
                let mut data = vec![0; 0x18];
                data[2] = (1 << 6) | (1 << 5) | (1 << 4) | 1; // Multi Session; Mode 2 Form 2; Mode 2 Form 1; Audio Play
                data[4] = (0b001 << 5) | (1 << 3); // Tray type loading mechanism; Eject
                data[5] = (1 << 1) | 1; // Separate Channel Mute; Separate volume levels
                data[8..=9].copy_from_slice(&256u16.to_be_bytes()); // Number of Volume Levels Supported

                Some(data)
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

    fn mode_select(&mut self, page: u8, data: &[u8]) -> Result<()> {
        match page {
            0x0e => {
                // CD Audio Control Page

                log::debug!("CD Audio Control {:?}", data);

                // TODO: Implement Stop On Track Crossing mode.
                // I can't find any software that enables SOTC mode.
                let _sotc = (data[0] >> 1) & 0x1;
                self.audio_ports[0] = AudioPort {
                    channel: data[6],
                    volume: data[7],
                };
                self.audio_ports[1] = AudioPort {
                    channel: data[8],
                    volume: data[9],
                };
                self.audio_ports[2] = AudioPort {
                    channel: data[10],
                    volume: data[11],
                };
                self.audio_ports[3] = AudioPort {
                    channel: data[12],
                    volume: data[13],
                };

                Ok(())
            }
            // TODO: Myst sends MODE SELECT page 0x31
            // (vendor page; purpose unknown)
            _ => bail!("MODE SELECT page 0x{:X} not implemented", page),
        }
    }

    fn blocksize(&self) -> Option<usize> {
        Some(self.blocksize)
    }

    fn blocks(&self) -> Option<usize> {
        Some(self.backend.as_ref()?.byte_len().div_ceil(self.blocksize))
    }

    fn read(&mut self, block_offset: usize, block_count: usize) -> Result<ScsiCmdResult> {
        // If blocks() returns None this will never be called by
        // ScsiTarget::cmd
        let blocksize = self.blocksize;
        let backend = self.backend.as_ref().expect("read() but no media inserted");
        let start_offset = block_offset * blocksize;
        let image_end_offset =
            std::cmp::min((block_offset + block_count) * blocksize, backend.byte_len());

        match backend.read_bytes(start_offset, image_end_offset - start_offset) {
            Ok(mut result) => {
                // CD-ROM images may not be exactly aligned on block size
                // Pad the end to a full block size
                result.resize(block_count * blocksize, 0);
                Ok(ScsiCmdResult::DataIn(result))
            }
            Err(CdromError::CheckCondition(cc, asc)) => {
                self.common.set_cc(cc, asc);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
            Err(CdromError::Other(e)) => {
                log::error!(
                    "Error reading CD-ROM block at 0x{:X}, length 0x{:X}: {}",
                    start_offset,
                    image_end_offset - start_offset,
                    e
                );
                self.common
                    .set_cc(CC_KEY_MEDIUM_ERROR, ASC_UNRECOVERED_READ_ERROR);
                Err(e)
            }
        }
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
                log::debug!("REZERO UNIT");
                self.audio_state = AudioState::Stopped;
                self.audio_pos = LBA_START_SECTOR;
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            // READ(6) (no media)
            0x08 => {
                self.common
                    .set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
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
                let msf = (cmd[1] >> 1) & 0x1;
                let sub_q = (cmd[2] >> 5) & 0x1;
                let format = cmd[3];
                let _track = cmd[6];
                let alloc_len = u16::from_be_bytes(cmd[7..=8].try_into()?) as usize;

                // log::debug!(
                //     "READ SUB-CHANNEL msf {} sub_q {} format {} track {} alloc_len {}",
                //     msf,
                //     sub_q,
                //     format,
                //     track,
                //     alloc_len
                // );

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
                    self.common
                        .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                match format {
                    0x01 => {
                        // CD-ROM Current Position
                        // Find current track at playback position
                        let Some(track) = self.get_track_at_sector(self.audio_pos) else {
                            // No tracks available
                            // FIXME: is this correct?
                            self.common
                                .set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                        };

                        // log::debug!(
                        //     "audio pos is at sector {} (in track #{} at sector {})",
                        //     self.audio_pos,
                        //     track.tno,
                        //     track.sector
                        // );

                        // FIXME: This code tries to guess the subchannel info based on the TOC.
                        // Instead, the backend should return subchannel info along with
                        // read_raw_sector.

                        // [PIONEER] Table 2-27F: CD-ROM Current Position Data Block
                        result.push(0x01); // Sub Channel Data Format code
                        result.push((1 << 4) | track.control); // ADR/Control
                        result.push(track.tno); // Track Number
                        result.push(1); // Index Number (TODO: Find correct index number)
                        result.extend_from_slice(
                            &self.msf_to_address_field(Msf::from_sector(self.audio_pos)?, msf != 0),
                        );
                        // Track relative position can be negative if the audio position is in a track pregap.
                        let track_relative = self.audio_pos as i32 - track.sector as i32;
                        if let Ok(track_relative) = TryInto::<u32>::try_into(track_relative) {
                            // Track relative position is positive.
                            result.extend_from_slice(
                                &self.msf_to_address_field(
                                    Msf::from_sector(track_relative)?,
                                    msf != 0,
                                ),
                            );
                        } else if msf != 0 {
                            // Track relative position is negative (MSF).
                            // [MMC4] 6.29.3.3:
                            // If the
                            // TIME bit is set to one, this field is the relative TIME address from the Q Sub-channel formatted
                            // according to Table 29.
                            // FIXME: It isn't clear how to express a negative MSF code. Put zeros here for now.
                            result.extend_from_slice(&[0, 0, 0, 0]);
                        } else {
                            // Track relative position is negative (LBA).
                            // [MMC4] 6.29.3.3:
                            // If the CDB TIME bit is zero, this field is a track relative LBA. If the current block is in
                            // the pre-gap area of a track, this is a negative value, expressed as a twoís-complement number.
                            result.extend_from_slice(&track_relative.to_be_bytes());
                        }
                    }
                    _ => {
                        // TODO: Implement sub-channel data block stuff
                        log::warn!("Reading unknown sub-channel format {}", format);
                        self.common
                            .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
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

                // log::debug!(
                //     "READ TOC msf {} format {} control {} track {} alloc_len {}",
                //     msf,
                //     format,
                //     control,
                //     track,
                //     alloc_len
                // );

                if control != 0 {
                    log::warn!("Unimplemented READ TOC control 0x{:X}", control);
                }

                self.read_toc(msf != 0, format, track, alloc_len)
            }
            // PLAY AUDIO MSF
            0x47 => {
                let start_msf = Msf::from_bytes(cmd[3..=5].try_into().unwrap());
                let end_msf = Msf::from_bytes(cmd[6..=8].try_into().unwrap());

                log::debug!("PLAY AUDIO MSF start {} end {}", start_msf, end_msf);

                let Some(backend) = &self.backend else {
                    self.common
                        .set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                let Some(sessions) = backend.sessions() else {
                    self.common
                        .set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                // [MMC4] 6.17.2.3:
                // If the Starting Minutes, Seconds, and Frame Fields are set to FFh, the Starting address is taken from
                // the Current Optical Head location. This allows the Audio Ending address to be changed without
                // interrupting the current playback operation.
                let start_sector = if start_msf == Msf::new(255, 255, 255) {
                    self.audio_pos
                } else {
                    start_msf.to_sector()
                };

                let end_sector = end_msf.to_sector();
                if start_sector > end_sector {
                    // [MMC4] 6.17.2.3:
                    // If the starting MSF address is greater than the ending MSF
                    // address, the command shall be terminated with CHECK CONDITION status and SK/ASC/ASCQ
                    // values shall be set to ILLEGAL REQUEST/INVALID FIELD IN CDB.
                    log::error!(
                        "Tried to play audio with start sector {} > end sector {}",
                        start_sector,
                        end_sector
                    );
                    self.audio_state = AudioState::Stopped;
                    self.common
                        .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                let final_leadout = sessions.last().unwrap().leadout;
                if start_sector >= final_leadout {
                    // [MMC4] 6.17.2.3:
                    // If the starting address is not found the command shall be terminated with CHECK CONDITION status
                    // and SK/ASC/ASCQ values shall be set to ILLEGAL REQUEST/LOGICAL BLOCK ADDRESS OUT
                    // OF RANGE.
                    log::error!("Tried to play audio from invalid location {}", start_msf);
                    self.common.set_cc(
                        CC_KEY_ILLEGAL_REQUEST,
                        ASC_LOGICAL_BLOCK_ADDRESS_OUT_OF_RANGE,
                    );
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                let start_track = self.get_track_at_sector(start_sector);
                if start_track
                    .map(|t| t.control != AUDIO_TRACK)
                    .unwrap_or(false)
                {
                    // [MMC4] 6.17.2.3:
                    // If the address is not within an audio track the command shall be terminated with
                    // CHECK CONDITION status and SK/ASC/ASCQ values shall be set to ILLEGAL REQUEST/ILLEGAL
                    // MODE FOR THIS TRACK or ILLEGAL REQUEST/INCOMPATIBLE MEDIUM INSTALLED.
                    log::error!("Tried to play audio from non-audio track at {}", start_msf);
                    self.common
                        .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_ILLEGAL_MODE_FOR_THIS_TRACK);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                self.audio_pos = start_sector;
                self.audio_stop = end_sector;

                // [MMC4] 6.17.2.3:
                // A starting MSF address equal to an ending MSF address causes no audio play operation to occur.
                // This shall not be considered an error.
                // (Apple Audio CD Player issues such a command to set the audio cursor
                // without initiating playback.)
                if start_sector != end_sector && self.audio_state != AudioState::Playing {
                    self.audio_state = AudioState::Playing;
                    self.audio_clock = 0;
                }

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            // PAUSE/RESUME
            0x4B => {
                let resume = cmd[8] & 0x1;

                log::debug!("PAUSE/RESUME resume {} cmd {:?}", resume, cmd);

                // FIXME: What happens if pause/resume is activated while no track is playing?
                if resume != 0 {
                    self.audio_state = AudioState::Playing;
                } else {
                    self.audio_state = AudioState::Paused;
                }
                self.audio_clock = 0;

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
                    self.common
                        .set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                let Some(tracks) = backend.tracks() else {
                    self.common
                        .set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
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
                let _start_sector = match addr_type {
                    // Logical Block Address
                    0b00 => LBA_START_SECTOR + u32::from_be_bytes(start_addr.try_into()?),
                    // CD absolute time
                    0b01 => Msf::new(start_addr[1], start_addr[2], start_addr[3]).to_sector(),
                    // Track Number
                    0b10 => {
                        // Start at the given track or the next available track
                        // FIXME: what should happen if specified track is not available?
                        let track = tracks
                            .iter()
                            .find(|t| t.tno >= start_addr[3])
                            .ok_or_else(|| anyhow!("Failed to find track"))?;
                        track.sector
                    }
                    // Reserved
                    _ => {
                        self.common
                            .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                        return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                    }
                };

                // TODO: implement audio scan
                //
                // [PIONEER] 2.1:
                //
                // When AUDIO SCAN (1) is executed, the drive begins a high-speed scan from the Scan Start
                // Address. The drive plays a block as it crosses each track. Each scan is approximately 15 seconds.
                log::warn!("CD audio fast-forward/rewind not implemented");

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            _ => {
                log::error!("Unknown command {:02X}h", cmd[0]);
                self.common
                    .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_COMMAND);
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

    fn set_blocksize(&mut self, blocksize: usize) -> bool {
        // FIXME: Do CD-ROM drives really allow the block size to be modified by software?
        //
        // [PIONEER] 2.21:
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

    fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> {
        self.audio_sink = Some(provider.create_stream(44100, 2, AUDIO_BUFFER_SAMPLES as u16)?);
        Ok(())
    }

    #[cfg(feature = "ethernet")]
    fn eth_set_link(&mut self, _link: super::ethernet::EthernetLinkType) -> Result<()> {
        unreachable!()
    }

    #[cfg(feature = "ethernet")]
    fn eth_link(&self) -> Option<super::ethernet::EthernetLinkType> {
        None
    }

    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<()> {
        if self.audio_state == AudioState::Playing {
            match self.try_pump_audio(ctx) {
                Some(result) => result,
                None => {
                    // Real audio is disabled. Advance the audio position by counting bus ticks.
                    self.audio_clock += ticks;
                    if self.audio_clock >= CLOCK_SPEED / AUDIO_SECTORS_PER_SEC as u64 {
                        self.audio_clock -= CLOCK_SPEED / AUDIO_SECTORS_PER_SEC as u64;

                        if self.audio_pos >= self.audio_stop {
                            self.audio_state = AudioState::Stopped;
                            self.audio_clock = 0;
                        } else {
                            self.audio_pos += 1;
                        }
                    }

                    Ok(())
                }
            }?;
        }

        Ok(())
    }
}

impl Debuggable for ScsiTargetCdrom {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        vec![]
    }
}
