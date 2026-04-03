//! SCSI CD-ROM drive (block device)

mod backends;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::EmuContext;
use crate::mac::macii::bus::CLOCK_SPEED;
use crate::mac::scsi::cdrom::backends::cuesheet::CuesheetCdromBackend;
use crate::mac::scsi::cdrom::backends::iso::IsoCdromBackend;
use crate::mac::scsi::ASC_UNRECOVERED_READ_ERROR;
use crate::renderer::{AudioProvider, AudioSink, AUDIO_BUFFER_SAMPLES};
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
const AUDIO_SECTORS_PER_SEC: u64 = 75;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Msf {
    m: u8,
    s: u8,
    f: u8,
}

impl Msf {
    const fn new(m: u8, s: u8, f: u8) -> Msf {
        Msf { m, s, f }
    }

    fn from_sector(sector: u32) -> Result<Msf> {
        let m = sector / 75 / 60;
        let s = (sector / 75) % 60;
        let f = sector % 75;
        Ok(Msf::new(m.try_into()?, s.try_into()?, f.try_into()?))
    }

    const fn to_sector(self) -> u32 {
        (self.m as u32 * 60 + self.s as u32) * 75 + self.f as u32
    }
}

impl std::fmt::Display for Msf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.m, self.s, self.f)
    }
}

#[derive(Debug)]
pub struct TrackInfo {
    /// The track number. Note that tracks don't necessarily start at number 1.
    tno: u8,
    /// Control field indicating track format
    control: u8,
    /// Absolute sector number where the track begins
    sector: u32, // Forget about MSF/LBA; use absolute sector numbers wherever possible.
}

pub struct SessionInfo {
    /// Absolute sector number of leadout
    leadout: u32,
    tracks: Vec<TrackInfo>,
}

pub trait CdromBackend: Send {
    fn byte_len(&self) -> usize;
    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>>;
    fn image_path(&self) -> Option<&Path>;

    /// Return a list of sessions, each containing a list of tracks.
    /// Unlike tracks, sessions are always numbered starting at 1.
    fn sessions(&self) -> Option<&[SessionInfo]>;

    /// Read a raw 2352-byte sector. Currently used only for CD audio. Other data is read via read_bytes.
    fn read_raw_sector(&self, sector: u32) -> Result<[u8; RAW_SECTOR_LEN]>;
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

    audio_volume: u8,

    /// Audio sink for CD audio
    #[serde(skip)]
    audio_sink: Option<Box<dyn AudioSink>>,
}

impl ScsiTargetCdrom {
    const VALID_BLOCKSIZES: [usize; 2] = [512, 2048];

    pub fn new(audio_provider: Option<&mut (dyn AudioProvider + '_)>) -> Self {
        let mut self_ = Self {
            backend: None,
            cc_code: 0,
            cc_asc: 0,
            event_eject: Default::default(),
            blocksize: 2048,
            audio_state: AudioState::Stopped,
            audio_pos: LBA_START_SECTOR,
            audio_stop: 0,
            audio_clock: 0,
            audio_volume: u8::MAX,
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
            // TODO: find citation
            i32::to_be_bytes(lba)
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

        let Some(sessions) = backend.sessions() else {
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

                // TODO: support multisession discs (All sessions TOC's must be combined)
                let session = &sessions[0];

                // FIXME: avoid unwrap
                result.push(session.tracks.first().unwrap().tno); // First Track Number
                result.push(session.tracks.last().unwrap().tno); // Last Track Number

                // Start at the given track or the next available track
                let track_iter = session.tracks.iter().skip_while(|t| t.tno < track);

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

                // Emit leadout track descriptor
                result.push(0); // Reserved
                result.push((1 << 4) | session.tracks.last().unwrap().control); // ADR/Control
                result.push(TRACK_LEADOUT); // Track Number
                result.push(0); // Reserved
                result.extend_from_slice(
                    &self.msf_to_address_field(Msf::from_sector(sessions[0].leadout)?, msf),
                );

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

                result.push(1); // First Session Number (always 1)
                result.push(sessions.len() as u8); // Last Session Number

                // This command queries the "first track in the last session" apparently...
                let first_track = sessions.last().unwrap().tracks.first().unwrap();

                // [PIONEER] Table 2-28D: Track Descriptors
                result.push(0); // Reserved
                result.push((0x1 << 4) | first_track.control); // ADR/Control
                result.push(first_track.tno); // First Track Number in Last Session
                result.push(0); // Reserved
                result.extend_from_slice(
                    &self.msf_to_address_field(Msf::from_sector(first_track.sector)?, msf),
                ); // Absolute CD-ROM Address of the First Track in the Last Session

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

                result.push(1); // First Complete Session Number (always 1)
                result.push(sessions.len() as u8); // Last Complete Session Number

                for (session_num, session) in sessions.iter().enumerate() {
                    let session_num = session_num + 1; // Session Numbers start at 1

                    // First track number in the program area
                    result.push(session_num as u8); // Session Number
                    result.push((1 << 4) | session.tracks.first().unwrap().control);
                    result.push(0); // TNO (0 for the lead-in area)
                    result.push(0xA0); // POINT (First Track number in the program area)
                    result.push(0); // ATIME (0:0:0 for the lead-in area)
                    result.push(0);
                    result.push(0);
                    result.push(0); // Zero
                    result.push(session.tracks.first().unwrap().tno); // First Track Number
                    result.push(0); // Disc Type
                    result.push(0);

                    // Last track number in the program area
                    result.push(session_num as u8); // Session Number
                    result.push((1 << 4) | session.tracks.first().unwrap().control);
                    result.push(0); // TNO (0 for the lead-in area)
                    result.push(0xA1); // POINT (Last Track number in the program area)
                    result.push(0); // ATIME (0:0:0 for the lead-in area)
                    result.push(0);
                    result.push(0);
                    result.push(0); // Zero
                    result.push(session.tracks.last().unwrap().tno); // Last Track Number
                    result.push(0);
                    result.push(0);

                    // Start location of the Lead-out area
                    result.push(session_num as u8); // Session Number
                    result.push((1 << 4) | session.tracks.first().unwrap().control);
                    result.push(0); // TNO (0 for the lead-in area)
                    result.push(0xA2); // POINT (Start location of the Lead-out area)
                    result.push(0); // ATIME (0:0:0 for the lead-in area)
                    result.push(0);
                    result.push(0);
                    result.push(0); // Zero
                    let leadout = Msf::from_sector(session.leadout)?;
                    result.push(leadout.m); // Start position of Lead-out
                    result.push(leadout.s);
                    result.push(leadout.f);

                    for track in &session.tracks {
                        result.push(session_num as u8); // Session Number
                        result.push((1 << 4) | track.control); // ADR/Control
                        result.push(0); // TNO (0 for the lead-in area)
                        result.push(track.tno); // POINT (0-99 for tracks)
                        result.push(0); // ATIME (0:0:0 for the lead-in area)
                        result.push(0);
                        result.push(0);
                        result.push(0); // Zero
                        let track_msf = Msf::from_sector(track.sector)?;
                        result.push(track_msf.m); // Start position of track
                        result.push(track_msf.s);
                        result.push(track_msf.f);
                    }

                    // TODO: emit POINT's 0xB0 and 0xC0 for multisession discs
                }

                let data_length = result.len() - 2;
                result[0..2].copy_from_slice(&u16::to_be_bytes(data_length.try_into()?));

                result.truncate(alloc_len);
                Ok(ScsiCmdResult::DataIn(result))
            }
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

    /// Read a frame of CD audio and send it to the audio sink.
    fn pump_audio(&mut self) -> Result<()> {
        let audio_sink = self.audio_sink.as_ref().unwrap();

        // TODO: check if real audio should be enabled, return None if not

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

        if audio_sink.is_full() {
            return Ok(());
        }

        let samples = backend.read_raw_sector(self.audio_pos);
        if let Ok(samples) = samples {
            // FIXME: can we avoid converting to float by setting up a signed 16-bit PCM audio sink?
            let mut out_samples = [0.0; RAW_SECTOR_LEN / 2]; // 16-bit samples
            for i in 0..RAW_SECTOR_LEN / 2 {
                let sample = i16::from_le_bytes(samples[2 * i..][..2].try_into().unwrap());
                out_samples[i] = sample as f32 / 32768.0 * self.audio_volume as f32 / 255.0;
            }

            audio_sink.send(Box::new(out_samples))?;

            self.audio_pos += 1;
        }

        Ok(())
    }

    /// Read a frame of CD audio and send it to the audio sink.
    /// Returns None if audio is disabled (such as by running at Uncapped speed).
    fn try_pump_audio(&mut self, ctx: &dyn EmuContext) -> Option<Result<()>> {
        if self.audio_sink.is_none() {
            return None;
        }

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

impl ScsiTargetCdrom {
    fn load_cue(&mut self, path: &Path) -> Result<()> {
        self.backend = Some(Box::new(CuesheetCdromBackend::new(path)?));
        self.cc_code = 0;
        self.cc_asc = 0;
        self.event_eject.get_clear();
        Ok(())
    }

    fn get_track_at_sector(&self, sector: u32) -> Option<&TrackInfo> {
        self.backend.as_ref()?.sessions()?[0] // TODO: support multi-session discs
            .tracks
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
                // [MMC3] 6.3.7: CD Audio Control Page
                // (for some reason, this documentation went missing in later versions of MMC)

                let sotc = 0;

                Some(vec![
                    (1 << 2) | (sotc << 1), // IMMED (always 1), SOTC
                    0,                      // Reserved
                    0,                      // Reserved
                    0,                      // Reserved
                    75,                     // Obsolete (75)
                    75,                     // Obsolete (75)
                    0b0001, // CDDA Output Port 0 Channel Selection (attach audio channel 0)
                    self.audio_volume, // Output Port 0 Volume Default FFh
                    0b0010, // CDDA Output Port 1 Channel Selection (attach audio channel 1)
                    self.audio_volume, // Output Port 1 Volume Default FFh
                    0b0100, // CDDA Output Port 2 Channel Selection (attach audio channel 2)
                    0x00,   // Output Port 2 Volume Default 00h
                    0b1000, // CDDA Output Port 3 Channel Selection (attach audio channel 3)
                    0x00,   // Output Port 3 Volume Default 00h
                ])
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

                // TODO: Implement Stop On Track Crossing mode.
                // I can't find any software that enables SOTC mode.
                let sotc = (data[0] >> 1) & 0x1;
                // XXX: Only Output Port 0 Volume is used. I can't find any software that
                // adjusts port volumes independently (i.e. left/right channels).
                let volume = data[7];

                log::debug!("CD Audio Control sotc {} volume {}", sotc, volume);

                self.audio_volume = volume;
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

    fn read(&mut self, block_offset: usize, block_count: usize) -> Result<Vec<u8>> {
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
                Ok(result)
            }
            Err(e) => {
                self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_UNRECOVERED_READ_ERROR);
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
                log::warn!("REZERO UNIT");
                self.audio_state = AudioState::Stopped;
                self.audio_pos = LBA_START_SECTOR;
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

                        // log::debug!(
                        //     "audio pos is at sector {} (in track #{} at sector {})",
                        //     self.audio_pos,
                        //     track.tno,
                        //     track.sector
                        // );

                        // [PIONEER] Table 2-27F: CD-ROM Current Position Data Block
                        result.push(0x01); // Sub Channel Data Format code
                        result.push((1 << 4) | track.control); // ADR/Control
                        result.push(track.tno); // Track Number
                        result.push(1); // Index Number (TODO: Find correct index number)
                        result.extend_from_slice(
                            &self.msf_to_address_field(Msf::from_sector(self.audio_pos)?, msf != 0),
                        );
                        let track_relative = self.audio_pos - track.sector;
                        result.extend_from_slice(
                            &self.msf_to_address_field(Msf::from_sector(track_relative)?, msf != 0),
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
                let start_msf = Msf::new(cmd[3], cmd[4], cmd[5]);
                let end_msf = Msf::new(cmd[6], cmd[7], cmd[8]);

                log::info!("PLAY AUDIO MSF start {} end {}", start_msf, end_msf);

                let Some(backend) = &self.backend else {
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                let Some(sessions) = backend.sessions() else {
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                // [PIONEER]: 2.13:
                // If the starting address is not found, or if the address is not within an audio track, or if a not ready
                // condition exists, the drive will terminate with a Check Condition status.
                let session = &sessions[0];
                // TODO: support multisession discs

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
                    self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                if start_sector >= session.leadout {
                    // TODO: find citation for this
                    log::warn!(
                        "Tried to play audio from invalid location {}; command ignored",
                        start_msf
                    );
                    self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                let start_track = self.get_track_at_sector(start_sector);
                if start_track
                    .map(|t| t.control != AUDIO_TRACK)
                    .unwrap_or(false)
                {
                    // TODO: find citation for this
                    log::warn!(
                        "Tried to play audio from non-audio track at {}; command ignored",
                        start_msf
                    );
                    self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                self.audio_pos = start_sector;
                self.audio_stop = end_sector;
                if self.audio_state != AudioState::Playing {
                    self.audio_state = AudioState::Playing;
                    self.audio_clock = 0;
                }

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            // PAUSE/RESUME
            0x4B => {
                let resume = cmd[8] & 0x1;

                log::info!("PAUSE/RESUME resume {}", resume);

                // FIXME: What happens if pause/resume is activated while no track is playing?
                if resume != 0 {
                    self.audio_state = AudioState::Playing;
                    self.audio_clock = 0;
                } else {
                    self.audio_state = AudioState::Paused;
                    self.audio_clock = 0;
                }

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

                let Some(sessions) = backend.sessions() else {
                    self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };

                // TODO: support multi-session discs
                let mut tracks = sessions[0].tracks.iter();

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
                    0b01 => Msf::new(start_addr[1], start_addr[2], start_addr[3]).to_sector(),
                    // Track Number
                    0b10 => {
                        // Start at the given track or the next available track
                        // FIXME: what should happen if specified track is not available?
                        // TODO: avoid unwrap
                        let track = tracks.find(|t| t.tno >= start_addr[3]).unwrap();
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
                log::warn!("CD audio fast-forward/rewind not implemented");

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
                    if self.audio_clock >= CLOCK_SPEED / AUDIO_SECTORS_PER_SEC {
                        self.audio_clock -= CLOCK_SPEED / AUDIO_SECTORS_PER_SEC;

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
