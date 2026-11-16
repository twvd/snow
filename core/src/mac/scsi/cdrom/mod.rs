//! SCSI CD-ROM drive (block device)

pub mod backends;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    debuggable::Debuggable,
    emulator::{EmuContext, comm::EmulatorSpeed},
    mac::scsi::{
        ASC_ILLEGAL_MODE_FOR_THIS_TRACK, ASC_INVALID_COMMAND,
        ASC_LOGICAL_BLOCK_ADDRESS_OUT_OF_RANGE, ASC_LOGICAL_UNIT_BECOMING_READY,
        ASC_UNRECOVERED_READ_ERROR, CC_KEY_NOT_READY, ScsiCheckCondition,
        cdrom::backends::{
            cuesheet::CuesheetCdromBackend, drive_thread::Reply, is_physical_cdrom_drive_path,
            iso::IsoCdromBackend, new_physical_cdrom_drive_backend,
        },
        target::ScsiTargetCommon,
    },
    renderer::{AUDIO_BUFFER_SAMPLES, AudioProvider, AudioSink},
    tickable::Ticks,
    types::LatchingEvent,
};

use super::{
    ASC_INVALID_FIELD_IN_CDB, ASC_MEDIUM_NOT_PRESENT, ASC_NOT_READY_TO_READY_CHANGE,
    ASC_TOO_MANY_FILES, CC_KEY_ILLEGAL_REQUEST, CC_KEY_MEDIUM_ERROR, CC_KEY_UNIT_ATTENTION,
    STATUS_CHECK_CONDITION, STATUS_GOOD, ScsiCmdResult,
    disk_image::{DiskImage, FileDiskImage},
    target::{ScsiTarget, ScsiTargetEvent, ScsiTargetType},
    toolbox::{MAX_FILE_LISTING_FILES, toolbox_file_entry},
};

// CD-ROM protocol Documentation:
//
// [MMC4]: <https://13thmonkey.org/documentation/SCSI/mmc4r05a.pdf>
// [CDU541]: <https://bitsavers.trailing-edge.com/pdf/sony/cdrom/CDU541-25_AppleCD_150/Sony_CDU-541_SCSI_Interface_Manual_Mar1990.pdf>

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

/// Number of audio frames per second. A frame is comprised of two samples (left and right).
///
/// This is often referred to as the sample rate, but technically there are two samples per
/// frame.
const AUDIO_FRAMES_PER_SEC: u32 = 44100;
/// Number of sectors per second of audio
const AUDIO_SECTORS_PER_SEC: u32 = 75;
/// Number of audio frames per sector
const AUDIO_FRAMES_PER_SECTOR: usize =
    AUDIO_FRAMES_PER_SEC as usize / AUDIO_SECTORS_PER_SEC as usize;

fn bin_to_bcd(bin: u8) -> u8 {
    ((bin / 10) << 4) | (bin % 10)
}

fn bcd_to_bin(bcd: u8) -> u8 {
    (bcd >> 4) * 10 + (bcd & 0xf)
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

#[derive(Clone, Debug)]
pub struct SessionInfo {
    /// Session number. Always starts at 1.
    number: u8,
    /// Value to put in Disc Type field ([MMC4] Table 448)
    disc_type: u8,
    /// Absolute sector number of the start of the program area
    start: u32,
    /// Absolute sector number of lead-out
    leadout: u32,
}

/// Disc information. Changes each time a new disc is inserted.
#[derive(Clone, Debug)]
pub struct DiscInfo {
    /// Byte capacity, see `CdromBackend::byte_len`
    capacity: usize,
    sessions: Vec<SessionInfo>,
    tracks: Vec<TrackInfo>,
}

pub enum CdromError {
    /// SCSI read error with CC, ASC/ASCQ
    CheckCondition(u8, u16),
    /// The backend is dead (e.g. an external CD-ROM drive was unplugged) and is
    /// no longer usable.
    BackendDead,
    /// Any other type of error
    Other(anyhow::Error),
}

impl From<anyhow::Error> for CdromError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(value)
    }
}

/// 12 bytes of Q subchannel data.
///
/// The first byte always contains Control/ADR. If ADR is 1, the remainder of QSub
/// contains Track and Index, relative time and absolute time. Note that times are
/// in BCD format.
///
/// See [MMC4] Table 354: Formatted Q-Subchannel Data
/// and [MMC4] 4.2.4.5.1: Types of Q.
#[derive(Serialize, Deserialize)]
pub struct QSub([u8; 12]);

impl QSub {
    /// Create a Mode-1 Q field with the given information
    fn new_mode1(control: u8, tno: u8, index: u8, abs_sector: u32, track_start: u32) -> Self {
        let mut result = [0; 12];

        result[0] = (control << 4) | 1; // Control/ADR; this is reversed from other ADR/Control fields in the standard
        result[1] = bin_to_bcd(tno);
        result[2] = bin_to_bcd(index);

        // [MMC4] 4.2.4.5.2:
        // Is the relative time within the track encoded as 6 BCD digits. This is
        // 00:00:00 at track start and advances through the track. During the
        // pre-gap the time decreases.
        // (in other words, if abs_sector is before track_start, the relative time counts down)
        let rel_time = abs_sector as i64 - track_start as i64;
        let rel_time_msf = if rel_time < 0 {
            Msf::from_sector(u32::try_from(-rel_time).unwrap()).unwrap()
        } else {
            Msf::from_sector(u32::try_from(rel_time).unwrap()).unwrap()
        };
        result[3..=5].copy_from_slice(&rel_time_msf.to_bcd_bytes()); // MIN, SEC, FRAME

        // result[6] is ZERO

        result[7..=9].copy_from_slice(&Msf::from_sector(abs_sector).unwrap().to_bcd_bytes()); // AMIN, ASEC, AFRAME

        // result[10..=11] is an optional CRC; we leave it out

        Self(result)
    }

    /// Get ADR field
    fn adr(&self) -> u8 {
        self.0[0] & 0xf
    }

    /// Get CONTROL field (indicates audio/data; see [MMC4] Table 12: Q Sub-channel record format)
    fn control(&self) -> u8 {
        self.0[0] >> 4
    }

    fn is_audio(&self) -> bool {
        self.control() & 0b0100 == 0
    }

    fn is_data(&self) -> bool {
        !self.is_audio()
    }

    fn track(&self) -> u8 {
        assert_eq!(self.adr(), 1);
        bcd_to_bin(self.0[1])
    }

    fn index(&self) -> u8 {
        assert_eq!(self.adr(), 1);
        bcd_to_bin(self.0[2])
    }

    fn rel_time_in_sectors(&self) -> i32 {
        assert_eq!(self.adr(), 1);
        let rel_time_bytes: [u8; 3] = self.0[3..=5].try_into().unwrap();
        let rel_time_sectors = Msf::from_bytes(rel_time_bytes.map(bcd_to_bin)).to_sector();
        if self.index() == 0 {
            // Rel time is negative and counts down to 0
            -i32::try_from(rel_time_sectors).unwrap()
        } else {
            // Rel time is positive
            i32::try_from(rel_time_sectors).unwrap()
        }
    }

    fn abs_sector(&self) -> u32 {
        assert_eq!(self.adr(), 1);
        let abs_time_bytes: [u8; 3] = self.0[7..=9].try_into().unwrap();
        Msf::from_bytes(abs_time_bytes.map(bcd_to_bin)).to_sector()
    }
}

impl Default for QSub {
    fn default() -> Self {
        Self::new_mode1(0, 1, 1, LBA_START_SECTOR, LBA_START_SECTOR)
    }
}

pub struct RawSector {
    /// 2352 bytes of data (equivalent to 1/75th seconds of audio)
    data: [u8; RAW_SECTOR_LEN],
    /// Q subchannel data. Contains various metadata about the sector.
    qsub: QSub,
}

/// How a backend starts an operation, such as a read
pub enum Started<T> {
    /// The operation is done
    Done(Result<T, CdromError>),
    /// The operation goes on in the background
    Pending(Reply<T>),
}

/// The outcome of a media check, for `CdromBackend::apply_media_check`
///
/// The check has its own `result`, apart from the `Result` a `Reply` adds for
/// a dead drive thread: a check that fails still reports disc information. A
/// check that finds no disc fails with NOT READY, and its `disc_info` of None
/// tells the backend the disc is gone.
pub struct MediaCheck {
    /// The result of the check, see `CdromBackend::check_media`
    result: Result<(), CdromError>,
    /// The disc information after the check, as a physical drive reports it.
    /// None for image backends, which ignore it.
    disc_info: Option<DiscInfo>,
}

pub trait CdromBackend: Send {
    /// Check whether a disc is still in the drive and reload TOC's if necessary. Its
    /// result is Ok(()) if the drive is plugged in and a disc is mounted, or a CdromError if
    /// an error occurred.
    ///
    /// The check may finish at once (`Started::Done`) or in the background
    /// (`Started::Pending`). Either way, its outcome goes to `apply_media_check`.
    fn check_media(&mut self) -> Started<MediaCheck>;

    /// Takes in the outcome of a media check, and returns its result. Called
    /// once after each `check_media`, when its outcome is ready. A backend whose
    /// `check_media` returns `Started::Pending` should save the new disc
    /// information so it can be queried by `sessions` and `tracks`; others can
    /// keep this default.
    fn apply_media_check(&mut self, check: MediaCheck) -> Result<(), CdromError> {
        check.result
    }

    /// Whether the backend is still loading its media in the background, as a
    /// physical drive while it opens
    fn is_loading(&self) -> bool {
        false
    }

    /// The error loading the media failed with, for a backend that loads it
    /// in the background. Reported once, and the backend is unusable after it.
    fn take_load_error(&mut self) -> Option<anyhow::Error> {
        None
    }

    /// Get the byte capacity of the CD, where each sector starting at #150 contains 2048
    /// bytes of user data.
    ///
    /// Every sector up to the lead-out is counted toward the capacity; however, only Data
    /// sectors in Mode 1 or Mode 2 Form 1 can be accessed via READ commands.
    fn byte_len(&self) -> usize;

    /// Read user data from the CD.
    ///
    /// The SCSI CD-ROM protocol presents the CD as block device that can be read via standard
    /// READ commands. Blocks begin at sector 150. Each sector contains 2048 bytes of user
    /// data.
    ///
    /// Each sector accessed by this method is expected to be a Mode 1 or Mode 2 Form 1
    /// sector containing 2048 bytes of user data. If the wrong type of sector is accessed,
    /// it fails with an ILLEGAL MODE FOR THIS TRACK error.
    ///
    /// The read may finish at once (`Started::Done`) or in the background
    /// (`Started::Pending`).
    fn read_bytes(&self, offset: usize, length: usize) -> Started<Vec<u8>>;

    fn image_path(&self) -> Option<&Path>;

    fn sessions(&self) -> Option<&[SessionInfo]>;
    fn tracks(&self) -> Option<&[TrackInfo]>;

    /// Read a raw CD Digital Audio sector. Other types of sectors are read via `read_bytes`.
    ///
    /// The read may finish at once (`Started::Done`) or in the background
    /// (`Started::Pending`).
    fn read_cdda_sector(&self, sector: u32) -> Started<RawSector>;
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

    /// Folder of CD images for a "Toolbox managed" folder-backed drive.
    /// Enumerated on demand for the BlueSCSI Toolbox CD commands. None for a
    /// regular single-image / empty CD-ROM drive.
    #[serde(default)]
    image_dir: Option<PathBuf>,

    /// Set after a Toolbox CD swap so the next TEST UNIT READY reports a
    /// UNIT ATTENTION (medium may have changed), prompting the guest to remount.
    #[serde(default)]
    pending_unit_attention: bool,

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

    /// Last Q-subchannel data with ADR=1; used to report current playback position
    curr_mode1_qsub: QSub,

    /// Audio clock (counts bus ticks)
    audio_clock: Ticks,

    /// Audio sink for CD audio
    #[serde(skip)]
    audio_sink: Option<Box<dyn AudioSink>>,

    /// A command going on in the background
    #[serde(skip)]
    pending: Option<PendingCmd>,

    /// The audio sector being read for playback, and its reply
    #[serde(skip)]
    audio_read: Option<(u32, Reply<RawSector>)>,

    /// The error loading the media failed with in the background, taken from
    /// the backend (see `CdromBackend::take_load_error`) and kept here until
    /// `take_event` reports it, since ejecting drops the backend
    #[serde(skip)]
    load_error: Option<String>,
}

/// A command going on in the background
enum PendingCmd {
    /// READ(6) or READ(10)
    Read(Reply<Vec<u8>>, ReadRange),
    /// TEST UNIT READY, waiting for its media check
    UnitReady(Reply<MediaCheck>),
    /// READ TOC, waiting for its media check
    ///
    /// The `MediaCheck` contains the TOC data (its disc information), which
    /// is what READ TOC waits for: the check re-reads the TOC from the disc
    /// when the disc has changed. Once the backend has taken it in, the
    /// response is built from it without touching the drive again.
    ReadToc(Reply<MediaCheck>, TocRequest),
    /// PLAY AUDIO MSF, waiting for the first sector to be scanned
    PlayAudio(Reply<RawSector>, PlayRequest),
    /// The disc was inserted or ejected while a command was going on. The
    /// command was dropped, and ends with a medium change at the next poll.
    MediaChanged,
}

/// Where PLAY AUDIO MSF plays
#[derive(Clone, Copy)]
struct PlayRequest {
    /// The starting address in the command
    start_msf: Msf,
    /// Absolute sectors to play from and to
    start_sector: u32,
    end_sector: u32,
}

/// The arguments of READ TOC
#[derive(Clone, Copy)]
struct TocRequest {
    /// Addresses in MSF format, LBA if false
    msf: bool,
    format: u8,
    /// Interpretation depends on format
    track_or_session: u8,
    alloc_len: usize,
}

/// What a READ reads from the backend
struct ReadRange {
    /// Byte offset
    offset: usize,
    /// Byte length, up to the end of the disc
    length: usize,
    /// The length of the data the READ returns, in whole blocks
    padded_length: usize,
}

impl ScsiTargetCdrom {
    const VALID_BLOCKSIZES: [usize; 2] = [512, 2048];

    pub fn new(audio_provider: Option<&mut (dyn AudioProvider + '_)>) -> Self {
        let mut self_ = Self {
            common: Default::default(),
            backend: None,
            image_dir: None,
            pending_unit_attention: false,
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
            curr_mode1_qsub: Default::default(),
            audio_sink: None,
            pending: None,
            audio_read: None,
            load_error: None,
        };
        if let Some(audio_provider) = audio_provider
            && let Err(e) = self_.set_audio_provider(audio_provider)
        {
            log::error!("Failed setting CD audio provider: {}", e);
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

    /// READ TOC: checks the media, possibly in the background, then reports
    /// the TOC
    fn read_toc(&mut self, request: TocRequest) -> Result<ScsiCmdResult> {
        if let Some(result) = self.unit_ready_without_backend() {
            return Ok(result);
        }
        let backend = self.backend.as_mut().unwrap();
        match backend.check_media() {
            Started::Done(check) => self.toc_after_check(check, &request),
            Started::Pending(reply) => {
                self.pending = Some(PendingCmd::ReadToc(reply, request));
                Ok(ScsiCmdResult::Pending)
            }
        }
    }

    /// Finishes READ TOC after its media check
    fn toc_after_check(
        &mut self,
        check: Result<MediaCheck, CdromError>,
        request: &TocRequest,
    ) -> Result<ScsiCmdResult> {
        let result = self.apply_media_check(check);
        let check_result = self.media_checked(result)?;
        if !matches!(check_result, ScsiCmdResult::Status(STATUS_GOOD)) {
            return Ok(check_result);
        }
        self.toc_data(request)
    }

    /// The data of READ TOC, once the media check is GOOD
    fn toc_data(&mut self, request: &TocRequest) -> Result<ScsiCmdResult> {
        let TocRequest {
            msf,
            format,
            track_or_session,
            alloc_len,
        } = *request;

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
                                &Msf::from_sector(next_session.start)?.to_bytes(),
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
                            // Weird Al (and other discs) put timecode 95:00:00 here, indicating the Start of the pre-groove.
                            // (see [MMC4] 4.2.4.9.2 ATIP Time Codes)
                            result.extend_from_slice(&Msf::new(95, 0, 0).to_bcd_bytes()); // Start time of the first Lead-in Area of the disc)
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
        self.set_backend(None);
        self.audio_state = AudioState::Stopped;
        self.audio_pos = LBA_START_SECTOR;
    }

    /// Check audio position and make sure it is in a playable sector.
    ///
    /// If audio position is past the stop point, stop playback.
    /// If audio position is within a session gap, skip to the next session.
    fn resolve_audio_pos(&mut self) {
        let Some(backend) = &self.backend else {
            self.audio_state = AudioState::Stopped;
            return;
        };

        let Some(sessions) = backend.sessions() else {
            self.audio_state = AudioState::Stopped;
            return;
        };

        let curr_session = sessions
            .iter()
            .rev()
            .find(|s| s.start <= self.audio_pos)
            .unwrap();
        if self.audio_pos < curr_session.start + LBA_START_SECTOR {
            // Audio position is in the session pregap; skip to the program
            self.audio_pos = curr_session.start + LBA_START_SECTOR;
        } else if self.audio_pos >= curr_session.leadout {
            // Audio position is past the leadout; skip to the next session
            let next_session = sessions.get((curr_session.number as usize + 1).saturating_sub(1));
            if let Some(next_session) = next_session {
                self.audio_pos = next_session.start + LBA_START_SECTOR;
            } else {
                // There is no next session; stop the audio
                self.audio_pos = self.audio_stop;
            }
        }

        if self.audio_pos >= self.audio_stop {
            self.audio_state = AudioState::Stopped;
        }
    }

    /// Read a frame of CD audio and send it to the audio sink.
    fn pump_audio(&mut self) -> Result<()> {
        let audio_sink = self.audio_sink.as_ref().unwrap();

        if audio_sink.is_full() {
            return Ok(());
        }

        // Keep audio clock as 0 if real audio is active
        self.audio_clock = 0;

        self.resolve_audio_pos();

        if self.audio_state != AudioState::Playing {
            return Ok(());
        }

        let audio_sink = self.audio_sink.as_ref().unwrap();

        let Some(backend) = &self.backend else {
            return Ok(());
        };

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

        let samples = match self.audio_read.take() {
            // The sector we're waiting for
            Some((sector, reply)) if sector == self.audio_pos => match reply.poll() {
                Some(samples) => samples,
                None => {
                    // Still reading: try again on the next tick
                    self.audio_read = Some((sector, reply));
                    return Ok(());
                }
            },
            // Nothing in flight, or a read for another sector after a seek,
            // which is dropped here
            _ => match backend.read_cdda_sector(self.audio_pos) {
                Started::Done(samples) => samples,
                Started::Pending(reply) => {
                    self.audio_read = Some((self.audio_pos, reply));
                    return Ok(());
                }
            },
        };
        match samples {
            Ok(samples) => {
                // XXX: Do not require QSub CONTROL field to say audio here.
                // For some reason, some discs report CONTROL=4 (Data track) within
                // the sectors of audio tracks.
                // Example: Sector 51848 (11:31:23) of Weird Al - Bad Hair Day.
                if samples.qsub.adr() == 1 {
                    self.curr_mode1_qsub = samples.qsub;
                }

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
            Err(CdromError::CheckCondition(_, _)) => {
                // CD-ROM error while playing; disable playback
                self.audio_state = AudioState::Stopped;
            }
            Err(CdromError::BackendDead) => {
                log::warn!("The CD-ROM drive became unusable");
                self.eject_media();
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

impl ScsiTargetCdrom {
    /// Inserts or ejects a disc. A command going on in the background is
    /// dropped, as it was for the previous disc.
    fn set_backend(&mut self, backend: Option<Box<dyn CdromBackend>>) {
        // Keep an error loading the previous disc, to report it
        if let Some(old) = self.backend.as_mut()
            && let Some(e) = old.take_load_error()
        {
            log::error!("Loading the CD-ROM failed: {:#}", e);
            self.load_error = Some(format!("{:#}", e));
        }
        self.backend = backend;
        // An audio sector being read is from the previous disc
        self.audio_read = None;
        if self.pending.is_some() {
            // The bus still waits for the command to end
            self.pending = Some(PendingCmd::MediaChanged);
        }
    }

    /// Ends a command that was dropped when the disc was inserted or ejected,
    /// as a drive reports a medium change
    fn media_changed_during_command(&mut self) -> Result<ScsiCmdResult> {
        if self.backend.is_some() {
            self.common
                .set_cc(CC_KEY_UNIT_ATTENTION, ASC_NOT_READY_TO_READY_CHANGE);
        } else {
            self.common.set_cc(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
        }
        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
    }

    /// The answers to TEST UNIT READY that don't need the backend: a pending
    /// unit attention, or no disc. None if the backend has to check the media.
    fn unit_ready_without_backend(&mut self) -> Option<ScsiCmdResult> {
        // Report a pending medium-change (from a Toolbox CD swap) exactly once so
        // the guest re-reads the TOC and remounts the newly-selected disc.
        if self.pending_unit_attention {
            self.pending_unit_attention = false;
            self.common
                .set_cc(CC_KEY_UNIT_ATTENTION, ASC_NOT_READY_TO_READY_CHANGE);
            return Some(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        }

        if self.backend.is_none() {
            // No CD inserted
            self.common.set_cc(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
            return Some(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        }

        None
    }

    /// Reports the result of a media check, as TEST UNIT READY does
    fn media_checked(&mut self, result: Result<(), CdromError>) -> Result<ScsiCmdResult> {
        match result {
            Ok(()) => Ok(ScsiCmdResult::Status(STATUS_GOOD)),
            Err(e) => self.disc_error(e),
        }
    }

    /// Answers a command whose disc access failed: CHECK CONDITION with the
    /// error's sense, or for a drive that became unusable, an eject and NOT
    /// READY / MEDIUM NOT PRESENT. Other errors pass through.
    fn disc_error(&mut self, error: CdromError) -> Result<ScsiCmdResult> {
        match error {
            CdromError::CheckCondition(key, asc) => {
                self.common.set_cc(key, asc);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
            CdromError::BackendDead => {
                log::error!("The CD-ROM drive became unusable");
                self.eject_media();
                self.common.set_cc(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
            CdromError::Other(e) => Err(e),
        }
    }

    /// Gives the backend the outcome of a media check, and returns the
    /// result of the check. A dead drive thread passes through.
    fn apply_media_check(
        &mut self,
        check: Result<MediaCheck, CdromError>,
    ) -> Result<(), CdromError> {
        match self.backend.as_mut() {
            Some(backend) => backend.apply_media_check(check?),
            // A media change turns the command into MediaChanged instead, so
            // this doesn't happen
            None => Err(CdromError::CheckCondition(
                CC_KEY_NOT_READY,
                ASC_MEDIUM_NOT_PRESENT,
            )),
        }
    }

    /// Finishes PLAY AUDIO MSF once its first sector has been scanned
    fn play_audio_from(
        &mut self,
        initial_sector: Result<RawSector, CdromError>,
        request: &PlayRequest,
    ) -> Result<ScsiCmdResult> {
        let PlayRequest {
            start_msf,
            start_sector,
            end_sector,
        } = *request;

        let initial_sector = match initial_sector {
            Ok(s) => s,
            Err(e) => return self.disc_error(e),
        };

        if !initial_sector.qsub.is_audio() {
            // [MMC4] 6.17.2.3:
            // If the address is not within an audio track the command shall be terminated with
            // CHECK CONDITION status and SK/ASC/ASCQ values shall be set to ILLEGAL REQUEST/ILLEGAL
            // MODE FOR THIS TRACK or ILLEGAL REQUEST/INCOMPATIBLE MEDIUM INSTALLED.
            log::error!("Tried to play audio from non-audio track at {}", start_msf);
            self.common
                .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_ILLEGAL_MODE_FOR_THIS_TRACK);
            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        }

        if initial_sector.qsub.adr() == 1 {
            self.curr_mode1_qsub = initial_sector.qsub;
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

    /// Finishes a READ with what the backend read
    fn read_done(
        &mut self,
        result: Result<Vec<u8>, CdromError>,
        read: &ReadRange,
    ) -> Result<ScsiCmdResult> {
        match result {
            Ok(mut result) => {
                // CD-ROM images may not be exactly aligned on block size
                // Pad the end to a full block size
                result.resize(read.padded_length, 0);
                Ok(ScsiCmdResult::DataIn(result))
            }
            Err(CdromError::Other(e)) => {
                log::error!(
                    "Error reading CD-ROM block at 0x{:X}, length 0x{:X}: {}",
                    read.offset,
                    read.length,
                    e
                );
                self.common
                    .set_cc(CC_KEY_MEDIUM_ERROR, ASC_UNRECOVERED_READ_ERROR);
                Err(e)
            }
            Err(e) => self.disc_error(e),
        }
    }
}

/// The backend, if it's ready for a command that needs the disc. Otherwise,
/// why not: NOT READY with MEDIUM NOT PRESENT without a backend, or with
/// LOGICAL UNIT BECOMING READY while it loads its disc.
fn ready_backend(
    backend: Option<&dyn CdromBackend>,
) -> Result<&dyn CdromBackend, ScsiCheckCondition> {
    let Some(backend) = backend else {
        return Err(ScsiCheckCondition {
            key: CC_KEY_NOT_READY,
            asc: ASC_MEDIUM_NOT_PRESENT,
        });
    };
    if backend.is_loading() {
        return Err(ScsiCheckCondition {
            key: CC_KEY_NOT_READY,
            asc: ASC_LOGICAL_UNIT_BECOMING_READY,
        });
    }
    Ok(backend)
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
        self.set_backend(Some(Box::new(CuesheetCdromBackend::new(path)?)));
        self.common.set_cc(0, 0);
        self.event_eject.get_clear();
        Ok(())
    }

    fn load_physical_drive(&mut self, path: &Path) -> Result<()> {
        self.set_backend(Some(new_physical_cdrom_drive_backend(path)?));
        self.common.set_cc(0, 0);
        self.event_eject.get_clear();
        Ok(())
    }

    /// Sets the Toolbox-managed image folder for a folder-backed CD-ROM drive.
    pub(super) fn set_image_dir(&mut self, dir: Option<PathBuf>) {
        self.image_dir = dir;
    }

    /// Returns the mountable CD images in the Toolbox-managed folder, sorted by
    /// name so LIST_CDS ordering matches SET_NEXT_CD indexing. Empty if this is
    /// not a folder-backed drive or the folder cannot be read.
    fn folder_images(&self) -> Vec<PathBuf> {
        const CD_IMAGE_EXTS: [&str; 3] = ["iso", "toast", "cue"];
        let Some(dir) = &self.image_dir else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut images: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let visible = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| !n.starts_with('.'))
                    .unwrap_or(false);
                let is_image = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| CD_IMAGE_EXTS.iter().any(|&ext| e.eq_ignore_ascii_case(ext)))
                    .unwrap_or(false);
                visible && is_image
            })
            .collect();
        images.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        images
    }

    /// Mounts the first available image from the Toolbox-managed folder, if any.
    pub(super) fn mount_first_image(&mut self) -> Result<()> {
        if let Some(path) = self.folder_images().into_iter().next() {
            self.load_media(&path)?;
        }
        Ok(())
    }

    /// Switches the mounted disc to `path` and latches a UNIT ATTENTION so the
    /// next TEST UNIT READY tells the guest the medium may have changed.
    fn switch_media(&mut self, path: &Path) -> Result<()> {
        self.load_media(path)?;
        self.pending_unit_attention = true;
        Ok(())
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
        self.set_backend(Some(Box::new(IsoCdromBackend::new(image)?)));
        self.common.set_cc(0, 0);
        self.event_eject.get_clear();
        Ok(())
    }

    fn take_event(&mut self) -> Option<ScsiTargetEvent> {
        // A backend that failed to load in the background is unusable: eject
        // it, which keeps its error
        if let Some(backend) = self.backend.as_mut()
            && let Some(e) = backend.take_load_error()
        {
            log::error!("Loading the CD-ROM failed: {:#}", e);
            self.load_error = Some(format!("{:#}", e));
            self.eject_media();
        }
        if let Some(msg) = self.load_error.take() {
            return Some(ScsiTargetEvent::LoadFailed(msg));
        }
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
        if let Some(result) = self.unit_ready_without_backend() {
            return Ok(result);
        }
        let backend = self.backend.as_mut().unwrap();
        match backend.check_media() {
            Started::Done(check) => {
                let result = self.apply_media_check(check);
                self.media_checked(result)
            }
            Started::Pending(reply) => {
                self.pending = Some(PendingCmd::UnitReady(reply));
                Ok(ScsiCmdResult::Pending)
            }
        }
    }

    fn inquiry(&mut self, cmd: &[u8]) -> Result<ScsiCmdResult> {
        let evpd = cmd[1] & 0x1;
        let page = cmd[2];
        if evpd != 0 || page != 0 {
            // [SPC-3] 6.4.1:
            // If the PAGE CODE
            // field is not set to zero when the EVPD bit is set to zero, the command shall be terminated with CHECK CONDITION
            // status, with the sense key set to ILLEGAL REQUEST, and the additional sense code set to INVALID FIELD IN
            // CDB.
            self.common
                .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        }

        let alloc_len = u16::from_be_bytes(cmd[3..=4].try_into().unwrap());

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
        // Apple CD-ROM 4.0 only accepts CDU-8001/8002/8003; later drivers also
        // know 8004/8005/CR-8004. 8003 is in every version's table.
        result[16..32].copy_from_slice(b"CD-ROM CDU-8003 ");
        // 32..36 Revision
        result[32..36].copy_from_slice(b"1.9a");

        result.truncate(alloc_len as usize);
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
                // Magic Apple page

                let mut result = vec![0; 0x16];
                result[0..0x16].copy_from_slice(b"APPLE COMPUTER, INC   ");
                Some(result)
            }
            0x31 => {
                // BlueSCSI vendor page. Advertises Toolbox capability so host
                // software recognises this CD-ROM as BlueSCSI-managed.
                let mut result = vec![0; 42];
                result[0..42].copy_from_slice(b"BlueSCSI is the BEST STOLEN FROM BLUESCSI\x00");
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

    fn read_capacity(&mut self) -> Result<usize, ScsiCheckCondition> {
        ready_backend(self.backend.as_deref())?;
        match self.blocks() {
            Some(blocks) if blocks > 0 => Ok(blocks),
            _ => Err(ScsiCheckCondition {
                key: CC_KEY_NOT_READY,
                asc: ASC_MEDIUM_NOT_PRESENT,
            }),
        }
    }

    fn read(&mut self, block_offset: usize, block_count: usize) -> Result<ScsiCmdResult> {
        let backend = match ready_backend(self.backend.as_deref()) {
            Ok(backend) => backend,
            Err(cc) => {
                self.common.set_cc(cc.key, cc.asc);
                return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
            }
        };
        let blocksize = self.blocksize;
        let start_offset = block_offset * blocksize;
        let image_end_offset =
            std::cmp::min((block_offset + block_count) * blocksize, backend.byte_len());
        let read = ReadRange {
            offset: start_offset,
            // Nothing to read past the end, as without a disc
            length: image_end_offset.saturating_sub(start_offset),
            padded_length: block_count * blocksize,
        };

        match backend.read_bytes(read.offset, read.length) {
            Started::Done(result) => self.read_done(result, &read),
            Started::Pending(reply) => {
                self.pending = Some(PendingCmd::Read(reply, read));
                Ok(ScsiCmdResult::Pending)
            }
        }
    }

    fn poll_pending(&mut self) -> Option<Result<ScsiCmdResult>> {
        match self.pending.as_ref()? {
            PendingCmd::Read(reply, _) => {
                let result = reply.poll()?;
                let Some(PendingCmd::Read(_, read)) = self.pending.take() else {
                    unreachable!()
                };
                Some(self.read_done(result, &read))
            }
            PendingCmd::UnitReady(reply) => {
                let check = reply.poll()?;
                self.pending = None;
                let result = self.apply_media_check(check);
                Some(self.media_checked(result))
            }
            PendingCmd::ReadToc(reply, request) => {
                let check = reply.poll()?;
                let request = *request;
                self.pending = None;
                Some(self.toc_after_check(check, &request))
            }
            PendingCmd::PlayAudio(reply, request) => {
                let initial_sector = reply.poll()?;
                let request = *request;
                self.pending = None;
                Some(self.play_audio_from(initial_sector, &request))
            }
            PendingCmd::MediaChanged => {
                self.pending = None;
                Some(self.media_changed_during_command())
            }
        }
    }

    fn wait_pending(&mut self) -> Result<ScsiCmdResult> {
        match self.pending.take() {
            Some(PendingCmd::Read(reply, read)) => self.read_done(reply.wait(), &read),
            Some(PendingCmd::UnitReady(reply)) => {
                let result = self.apply_media_check(reply.wait());
                self.media_checked(result)
            }
            Some(PendingCmd::ReadToc(reply, request)) => {
                self.toc_after_check(reply.wait(), &request)
            }
            Some(PendingCmd::PlayAudio(reply, request)) => {
                self.play_audio_from(reply.wait(), &request)
            }
            Some(PendingCmd::MediaChanged) => self.media_changed_during_command(),
            None => bail!("No command pending"),
        }
    }

    fn abort_pending(&mut self) {
        // The drive finishes the command, and its result is thrown away
        self.pending = None;
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {
        log::error!("Write command to CD-ROM");
    }

    fn image_fn(&self) -> Option<&Path> {
        self.backend
            .as_ref()
            .and_then(|backend| backend.image_path())
    }

    fn cdrom_image_dir(&self) -> Option<&Path> {
        self.image_dir.as_deref()
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
                self.common.set_cc(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
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
                        result.push(0x01); // Sub Channel Data Format code
                        result.push((1 << 4) | self.curr_mode1_qsub.control()); // ADR/Control
                        result.push(self.curr_mode1_qsub.track()); // Track Number
                        result.push(self.curr_mode1_qsub.index()); // Index Number
                        result.extend_from_slice(
                            &self.msf_to_address_field(Msf::from_sector(self.audio_pos)?, msf != 0),
                        );

                        // [MMC4] 6.29.3.3:
                        // When the type of information encoded in the Q Sub-channel of the current sector is
                        // the media catalog number or ISRC, the track, index, and address fields should be extrapolated
                        // from the previous sector.
                        // (Not all audio sectors contain position info in their Q-subchannel. If the current
                        // audio sector doesn't contain position info, extrapolate from the last reported position info.)
                        // Track relative position can be negative if the audio position is in a track pregap.
                        let pos_delta =
                            self.audio_pos as i32 - self.curr_mode1_qsub.abs_sector() as i32;
                        let rel_time = self.curr_mode1_qsub.rel_time_in_sectors() + pos_delta;
                        if let Ok(track_relative) = TryInto::<u32>::try_into(rel_time) {
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
                            // (during the pregap, the relative time will count down to 0)
                            result.extend_from_slice(&self.msf_to_address_field(
                                Msf::from_sector((-rel_time).try_into().unwrap())?,
                                msf != 0,
                            ));
                        } else {
                            // Track relative position is negative (LBA).
                            // [MMC4] 6.29.3.3:
                            // If the CDB TIME bit is zero, this field is a track relative LBA. If the current block is in
                            // the pre-gap area of a track, this is a negative value, expressed as a twoís-complement number.
                            result.extend_from_slice(&rel_time.to_be_bytes());
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

                self.read_toc(TocRequest {
                    msf: msf != 0,
                    format,
                    track_or_session: track,
                    alloc_len,
                })
            }
            // PLAY AUDIO MSF
            0x47 => {
                let start_msf = Msf::from_bytes(cmd[3..=5].try_into().unwrap());
                let end_msf = Msf::from_bytes(cmd[6..=8].try_into().unwrap());

                log::debug!("PLAY AUDIO MSF start {} end {}", start_msf, end_msf);

                let backend = match ready_backend(self.backend.as_deref()) {
                    Ok(backend) => backend,
                    Err(cc) => {
                        self.common.set_cc(cc.key, cc.asc);
                        return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                    }
                };

                let Some(sessions) = backend.sessions() else {
                    self.common.set_cc(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
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

                let request = PlayRequest {
                    start_msf,
                    start_sector,
                    end_sector,
                };
                match backend.read_cdda_sector(start_sector) {
                    Started::Done(initial_sector) => self.play_audio_from(initial_sector, &request),
                    Started::Pending(reply) => {
                        self.pending = Some(PendingCmd::PlayAudio(reply, request));
                        Ok(ScsiCmdResult::Pending)
                    }
                }
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

                let backend = match ready_backend(self.backend.as_deref()) {
                    Ok(backend) => backend,
                    Err(cc) => {
                        self.common.set_cc(cc.key, cc.asc);
                        return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                    }
                };

                let Some(tracks) = backend.tracks() else {
                    self.common.set_cc(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
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
            // BlueSCSI Toolbox: LIST CDS (list the images in the managed folder)
            0xD7 => {
                let images = self.folder_images();
                log::debug!(
                    "Toolbox LIST_CDS: {} image(s) in {:?}",
                    images.len(),
                    self.image_dir
                );
                let mut data = Vec::new();
                // Firmware truncates here; COUNT_CDS reports the overflow.
                for (i, path) in images.iter().enumerate().take(MAX_FILE_LISTING_FILES) {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    log::debug!("  CD[{}] = '{}' ({} bytes)", i, name, size);
                    data.extend_from_slice(&toolbox_file_entry(i as u8, false, name, size));
                }
                Ok(ScsiCmdResult::DataIn(data))
            }
            // BlueSCSI Toolbox: SET NEXT CD (switch to the image at CDB[1])
            0xD8 => {
                let index = cmd[1] as usize;
                let images = self.folder_images();
                let Some(path) = images.get(index).cloned() else {
                    log::warn!(
                        "SET NEXT CD: index {} out of range ({} images)",
                        index,
                        images.len()
                    );
                    self.common
                        .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                };
                match self.switch_media(&path) {
                    Ok(()) => {
                        log::info!("Toolbox CD switch to '{}'", path.display());
                        Ok(ScsiCmdResult::Status(STATUS_GOOD))
                    }
                    Err(e) => {
                        log::error!("Toolbox CD switch failed: {:#}", e);
                        // ASC MEDIUM NOT PRESENT must be paired with the NOT READY
                        // sense key; otherwise early Apple CD extensions crash.
                        self.common.set_cc(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
                        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                    }
                }
            }
            // BlueSCSI Toolbox: COUNT CDS (number of images in the managed folder)
            0xDA => {
                let count = self.folder_images().len();
                if count > MAX_FILE_LISTING_FILES {
                    log::error!(
                        "Toolbox COUNT_CDS: {} images in {:?}, maximum is {}",
                        count,
                        self.image_dir,
                        MAX_FILE_LISTING_FILES
                    );
                    self.common
                        .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_TOO_MANY_FILES);
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }
                log::debug!("Toolbox COUNT_CDS: {}", count);
                Ok(ScsiCmdResult::DataIn(vec![count as u8]))
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
        self.audio_sink = Some(provider.create_stream(
            AUDIO_FRAMES_PER_SEC as i32,
            2,
            AUDIO_BUFFER_SAMPLES as u16,
        )?);
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

    fn ticks(&self) -> bool {
        true
    }

    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<()> {
        if self.audio_state == AudioState::Playing {
            match self.try_pump_audio(ctx) {
                Some(result) => result,
                None => {
                    // Real audio is disabled. Advance the audio position by counting bus ticks.
                    self.audio_clock += ticks;
                    if self.audio_clock >= ctx.bus_frequency() / AUDIO_SECTORS_PER_SEC as u64 {
                        self.audio_clock -= ctx.bus_frequency() / AUDIO_SECTORS_PER_SEC as u64;

                        self.audio_pos += 1;
                        self.resolve_audio_pos();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::scsi::ASC_PARAMETER_LIST_LENGTH_ERROR;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    /// Creates a fresh, uniquely-named temporary directory for a test.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snow_toolbox_cd_{}_{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(dir: &Path, name: &str, size: usize) {
        let mut f = fs::File::create(dir.join(name)).unwrap();
        f.write_all(&vec![0u8; size]).unwrap();
    }

    #[test]
    fn folder_images_filters_and_sorts() {
        let dir = temp_dir("filter");
        // Deliberately out of order, with mixed extensions, a non-image file
        // and a dotfile that must both be skipped.
        write_file(&dir, "b.iso", 10);
        write_file(&dir, "a.iso", 20);
        write_file(&dir, "c.toast", 30);
        write_file(&dir, "disc.cue", 40);
        write_file(&dir, "notes.txt", 5);
        write_file(&dir, ".hidden.iso", 5);

        let mut cd = ScsiTargetCdrom::new(None);
        cd.set_image_dir(Some(dir.clone()));

        let names: Vec<String> = cd
            .folder_images()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.iso", "b.iso", "c.toast", "disc.cue"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn toolbox_count_and_list_cds() {
        let dir = temp_dir("list");
        write_file(&dir, "a.iso", 0x11);
        write_file(&dir, "b.iso", 0x2233);

        let mut cd = ScsiTargetCdrom::new(None);
        cd.set_image_dir(Some(dir.clone()));

        // COUNT_CDS (0xDA) -> a single byte with the image count.
        let count_cmd = [0xDAu8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        match cd.specific_cmd(&count_cmd, None).unwrap() {
            ScsiCmdResult::DataIn(d) => assert_eq!(d, vec![2]),
            _ => panic!("COUNT_CDS did not return data"),
        }

        // LIST_CDS (0xD7) -> two 40-byte entries in the same sorted order that
        // SET_NEXT_CD indexes into.
        let list_cmd = [0xD7u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        match cd.specific_cmd(&list_cmd, None).unwrap() {
            ScsiCmdResult::DataIn(d) => {
                assert_eq!(d.len(), 80);
                let e0 = toolbox_file_entry(0, false, "a.iso", 0x11);
                let e1 = toolbox_file_entry(1, false, "b.iso", 0x2233);
                assert_eq!(&d[0..40], &e0[..]);
                assert_eq!(&d[40..80], &e1[..]);
                // Spot-check the raw wire format of the first entry.
                assert_eq!(d[0], 0); // index
                assert_eq!(d[1], 0x01); // type = file
                assert_eq!(&d[2..7], b"a.iso");
                assert_eq!(d[39], 0x11); // size, low byte (big-endian)
            }
            _ => panic!("LIST_CDS did not return data"),
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn set_next_cd_out_of_range_is_rejected() {
        let dir = temp_dir("range");
        write_file(&dir, "only.iso", 8);

        let mut cd = ScsiTargetCdrom::new(None);
        cd.set_image_dir(Some(dir.clone()));

        // Index 5 does not exist (only one image): expect CHECK CONDITION and no
        // latched unit attention.
        let set_cmd = [0xD8u8, 5, 0, 0, 0, 0, 0, 0, 0, 0];
        match cd.specific_cmd(&set_cmd, None).unwrap() {
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION) => {}
            other => panic!("expected CHECK CONDITION, got {:?}", other_status(&other)),
        }
        assert!(!cd.pending_unit_attention);

        fs::remove_dir_all(&dir).unwrap();
    }

    fn other_status(r: &ScsiCmdResult) -> String {
        match r {
            ScsiCmdResult::Status(s) => format!("Status({})", s),
            ScsiCmdResult::DataIn(d) => format!("DataIn({} bytes)", d.len()),
            ScsiCmdResult::DataOut(n) => format!("DataOut({})", n),
            ScsiCmdResult::Pending => "Pending".to_string(),
        }
    }

    /// Parses our LIST_CDS output exactly the way the reference BlueSCSI Toolbox
    /// client (nielsmh/escsitoolbox `ToolboxGetImageList`) does: COUNT_CDS gives a
    /// single-byte count, LIST_CDS is read as `count * 40` bytes, entries have a
    /// 40-byte stride with a NUL-terminated name at offset 2, stopping at the first
    /// empty name. This proves a correctly-written client recovers the real names.
    #[test]
    fn list_cds_is_parseable_by_reference_client() {
        let dir = temp_dir("client");
        write_file(&dir, "Alpha.iso", 100);
        write_file(&dir, "Beta.toast", 200);
        write_file(&dir, "Gamma.cue", 300);

        let mut cd = ScsiTargetCdrom::new(None);
        cd.set_image_dir(Some(dir.clone()));

        // Step 1: COUNT_CDS -> single byte.
        let count = match cd
            .specific_cmd(&[0xDAu8, 0, 0, 0, 0, 0, 0, 0, 0, 0], None)
            .unwrap()
        {
            ScsiCmdResult::DataIn(d) => {
                assert_eq!(d.len(), 1);
                d[0] as usize
            }
            _ => panic!("COUNT_CDS did not return data"),
        };
        assert_eq!(count, 3);

        // Step 2: LIST_CDS -> the client allocates and reads count * 40 bytes.
        let data = match cd
            .specific_cmd(&[0xD7u8, 0, 0, 0, 0, 0, 0, 0, 0, 0], None)
            .unwrap()
        {
            ScsiCmdResult::DataIn(d) => d,
            _ => panic!("LIST_CDS did not return data"),
        };
        assert_eq!(data.len(), count * 40);

        // Step 3: parse fixed 40-byte stride, name at offset 2, stop at empty name.
        let mut names = Vec::new();
        for i in 0..count {
            let entry = &data[i * 40..(i + 1) * 40];
            if entry[2] == 0 {
                break;
            }
            let end = entry[2..35].iter().position(|&b| b == 0).unwrap_or(33);
            names.push(std::str::from_utf8(&entry[2..2 + end]).unwrap().to_owned());
        }
        assert_eq!(names, ["Alpha.iso", "Beta.toast", "Gamma.cue"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn count_and_list_cds_honor_the_file_listing_limit() {
        for n in [MAX_FILE_LISTING_FILES, MAX_FILE_LISTING_FILES + 1, 300] {
            let dir = temp_dir(&format!("limit_{}", n));
            for i in 0..n {
                write_file(&dir, &format!("cd{:04}.iso", i), 1);
            }

            let mut cd = ScsiTargetCdrom::new(None);
            cd.set_image_dir(Some(dir.clone()));

            let count = cd
                .specific_cmd(&[0xDAu8, 0, 0, 0, 0, 0, 0, 0, 0, 0], None)
                .unwrap();
            if n > MAX_FILE_LISTING_FILES {
                assert!(
                    matches!(count, ScsiCmdResult::Status(STATUS_CHECK_CONDITION)),
                    "{} images should be rejected, got {}",
                    n,
                    other_status(&count)
                );
                assert_eq!(
                    cd.common.req_sense(),
                    (CC_KEY_ILLEGAL_REQUEST, ASC_TOO_MANY_FILES)
                );
            } else {
                match count {
                    ScsiCmdResult::DataIn(d) => assert_eq!(d, vec![n as u8]),
                    other => panic!("expected a count, got {}", other_status(&other)),
                }
            }

            let listed = match cd
                .specific_cmd(&[0xD7u8, 0, 0, 0, 0, 0, 0, 0, 0, 0], None)
                .unwrap()
            {
                ScsiCmdResult::DataIn(d) => d,
                other => panic!("LIST_CDS returned {}", other_status(&other)),
            };
            assert_eq!(listed.len(), MAX_FILE_LISTING_FILES.min(n) * 40);

            fs::remove_dir_all(&dir).unwrap();
        }
    }

    fn mode_select(cd: &mut ScsiTargetCdrom, data: &[u8]) -> u8 {
        let cmd = [0x15, 0, 0, 0, data.len() as u8, 0];
        match cd.cmd(&cmd, Some(data)).unwrap() {
            ScsiCmdResult::Status(status) => status,
            _ => panic!("MODE SELECT(6) did not end in the status phase"),
        }
    }

    #[test]
    fn mode_select_without_block_descriptor() {
        // Vendor specific data after a header with no block descriptor, as the
        // Apple CD-ROM driver sends it
        let mut cd = ScsiTargetCdrom::new(None);
        let data = [0, 0, 0, 0, 0x31, 0x02, 0xFF, 0];
        assert_eq!(mode_select(&mut cd, &data), STATUS_GOOD);
        assert_eq!(cd.blocksize(), Some(2048));
    }

    #[test]
    fn mode_select_block_descriptor_sets_block_size() {
        let mut cd = ScsiTargetCdrom::new(None);
        let data = [0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0x02, 0x00];
        assert_eq!(mode_select(&mut cd, &data), STATUS_GOOD);
        assert_eq!(cd.blocksize(), Some(512));
    }

    #[test]
    fn mode_select_truncated_block_descriptor() {
        let mut cd = ScsiTargetCdrom::new(None);
        let data = [0, 0, 0, 8, 0, 0, 0, 0];
        assert_eq!(mode_select(&mut cd, &data), STATUS_CHECK_CONDITION);
        let sense = (CC_KEY_ILLEGAL_REQUEST, ASC_PARAMETER_LIST_LENGTH_ERROR);
        assert_eq!(cd.common().req_sense(), sense);
    }

    /// A media check that finishes when the test says so
    #[derive(Default)]
    struct CheckGate {
        /// The sender of the check the backend started
        sender: Option<crossbeam_channel::Sender<Result<MediaCheck, CdromError>>>,
        /// The capacity in the disc information `apply_media_check` got, once
        /// it is called
        applied: Option<Option<usize>>,
    }

    type CheckState = Arc<Mutex<CheckGate>>;

    /// A disc of 4 KB whose reads and media checks finish when the test says
    /// so
    struct GatedBackend {
        /// Where the backend puts the sender of the read it started
        read: ReadSender,
        check: CheckState,
        /// A data disc with one track
        disc_info: DiscInfo,
        /// Where the backend puts the sender of the CDDA read it started
        cdda: CddaSender,
    }

    impl CdromBackend for GatedBackend {
        fn check_media(&mut self) -> Started<MediaCheck> {
            let (sender, reply) = Reply::channel();
            self.check.lock().unwrap().sender = Some(sender);
            Started::Pending(reply)
        }

        fn apply_media_check(&mut self, check: MediaCheck) -> Result<(), CdromError> {
            self.check.lock().unwrap().applied =
                Some(check.disc_info.as_ref().map(|info| info.capacity));
            check.result
        }

        fn byte_len(&self) -> usize {
            4096
        }

        fn read_bytes(&self, _offset: usize, _length: usize) -> Started<Vec<u8>> {
            let (sender, reply) = Reply::channel();
            *self.read.lock().unwrap() = Some(sender);
            Started::Pending(reply)
        }

        fn image_path(&self) -> Option<&Path> {
            None
        }

        fn sessions(&self) -> Option<&[SessionInfo]> {
            Some(&self.disc_info.sessions)
        }

        fn tracks(&self) -> Option<&[TrackInfo]> {
            Some(&self.disc_info.tracks)
        }

        fn read_cdda_sector(&self, _sector: u32) -> Started<RawSector> {
            let (sender, reply) = Reply::channel();
            *self.cdda.lock().unwrap() = Some(sender);
            Started::Pending(reply)
        }
    }

    type CddaSender = Arc<Mutex<Option<crossbeam_channel::Sender<Result<RawSector, CdromError>>>>>;

    type ReadSender = Arc<Mutex<Option<crossbeam_channel::Sender<Result<Vec<u8>, CdromError>>>>>;

    /// A CD-ROM drive with a disc whose reads and media checks finish when the
    /// test says so
    fn gated_cdrom() -> (ScsiTargetCdrom, ReadSender, CheckState) {
        let (cd, read, check, _) = gated_cdrom_with_cdda();
        (cd, read, check)
    }

    /// A CD-ROM drive with a disc whose reads, media checks and CDDA reads
    /// finish when the test says so
    fn gated_cdrom_with_cdda() -> (ScsiTargetCdrom, ReadSender, CheckState, CddaSender) {
        let read = ReadSender::default();
        let check = CheckState::default();
        let cdda = CddaSender::default();
        let mut cd = ScsiTargetCdrom::new(None);
        cd.backend = Some(Box::new(GatedBackend {
            read: read.clone(),
            check: check.clone(),
            cdda: cdda.clone(),
            disc_info: DiscInfo {
                capacity: 4096,
                sessions: vec![SessionInfo {
                    number: 1,
                    disc_type: 0,
                    start: 0,
                    leadout: LBA_START_SECTOR + 2,
                }],
                tracks: vec![TrackInfo {
                    tno: 1,
                    session: 1,
                    control: DATA_TRACK,
                    sector: LBA_START_SECTOR,
                }],
            },
        }));
        (cd, read, check, cdda)
    }

    /// A CD-ROM drive with a disc whose reads finish when the test says so
    fn cdrom_with_gated_disc() -> (ScsiTargetCdrom, ReadSender) {
        let (cd, read, _) = gated_cdrom();
        (cd, read)
    }

    fn test_unit_ready(cd: &mut ScsiTargetCdrom) -> ScsiCmdResult {
        cd.cmd(&[0x00, 0, 0, 0, 0, 0], None).unwrap()
    }

    /// Finishes the media check the backend started. Returns false if the
    /// target dropped it.
    fn finish_check(
        check: &CheckState,
        result: Result<(), CdromError>,
        disc_info: Option<DiscInfo>,
    ) -> bool {
        let sender = check
            .lock()
            .unwrap()
            .sender
            .take()
            .expect("no media check started");
        sender.send(Ok(MediaCheck { result, disc_info })).is_ok()
    }

    /// READ(10) of the second block
    fn read10(cd: &mut ScsiTargetCdrom) -> ScsiCmdResult {
        cd.cmd(&[0x28, 0, 0, 0, 0, 1, 0, 0, 1, 0], None).unwrap()
    }

    /// Finishes the read the backend started
    fn finish_read(read: &ReadSender, result: Result<Vec<u8>, CdromError>) {
        let sender = read.lock().unwrap().take().expect("no read started");
        // The target may have dropped the reply already
        let _ = sender.send(result);
    }

    #[test]
    fn read_finishes_in_the_background() {
        let (mut cd, read) = cdrom_with_gated_disc();
        assert!(matches!(read10(&mut cd), ScsiCmdResult::Pending));
        assert!(cd.poll_pending().is_none());

        // A short read is padded to a whole block
        finish_read(&read, Ok(vec![0x55; 2000]));
        match cd.poll_pending() {
            Some(Ok(ScsiCmdResult::DataIn(d))) => {
                assert_eq!(d.len(), 2048);
                assert_eq!(d[0], 0x55);
                assert_eq!(d[2047], 0);
            }
            other => panic!(
                "expected data, got {:?}",
                other.map(|r| r.map(|r| other_status(&r)))
            ),
        }
        assert!(cd.poll_pending().is_none());
    }

    #[test]
    fn aborted_read_is_dropped() {
        let (mut cd, read) = cdrom_with_gated_disc();
        assert!(matches!(read10(&mut cd), ScsiCmdResult::Pending));
        cd.abort_pending();
        finish_read(&read, Ok(vec![0x55; 2048]));
        assert!(cd.poll_pending().is_none());

        // The next READ goes on as usual
        assert!(matches!(read10(&mut cd), ScsiCmdResult::Pending));
        finish_read(&read, Ok(vec![0xAA; 2048]));
        match cd.poll_pending() {
            Some(Ok(ScsiCmdResult::DataIn(d))) => assert_eq!(d[0], 0xAA),
            _ => panic!("expected data"),
        }
    }

    #[test]
    fn wait_takes_the_read() {
        let (mut cd, read) = cdrom_with_gated_disc();
        assert!(matches!(read10(&mut cd), ScsiCmdResult::Pending));
        finish_read(&read, Ok(vec![0x55; 2048]));
        match cd.wait_pending() {
            Ok(ScsiCmdResult::DataIn(d)) => assert_eq!(d.len(), 2048),
            _ => panic!("expected data"),
        }
        assert!(cd.wait_pending().is_err());
    }

    #[test]
    fn dead_drive_during_a_read_ejects() {
        let (mut cd, read) = cdrom_with_gated_disc();
        assert!(matches!(read10(&mut cd), ScsiCmdResult::Pending));
        finish_read(&read, Err(CdromError::BackendDead));
        assert!(matches!(
            cd.poll_pending(),
            Some(Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION)))
        ));
        assert!(cd.backend.is_none());
        assert_eq!(
            cd.common.req_sense(),
            (CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT)
        );
    }

    #[test]
    fn image_read_finishes_at_once() {
        let dir = temp_dir("image_read");
        write_file(&dir, "disc.iso", 4096);
        let mut cd = ScsiTargetCdrom::new(None);
        cd.load_media(&dir.join("disc.iso")).unwrap();

        match read10(&mut cd) {
            ScsiCmdResult::DataIn(d) => assert_eq!(d.len(), 2048),
            other => panic!("expected data, got {}", other_status(&other)),
        }
        assert!(cd.poll_pending().is_none());

        drop(cd);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn eject_during_a_read_ends_it() {
        let (mut cd, read) = cdrom_with_gated_disc();
        assert!(matches!(read10(&mut cd), ScsiCmdResult::Pending));
        cd.eject_media();
        // The read was dropped with the disc
        assert!(
            read.lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .send(Ok(vec![0x55; 2048]))
                .is_err()
        );
        assert!(matches!(
            cd.poll_pending(),
            Some(Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION)))
        ));
        assert_eq!(
            cd.common.req_sense(),
            (CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT)
        );
        assert!(cd.poll_pending().is_none());
    }

    #[test]
    fn disc_swap_during_a_read_ends_it() {
        let (mut cd, read) = cdrom_with_gated_disc();
        assert!(matches!(read10(&mut cd), ScsiCmdResult::Pending));
        let (other, _) = cdrom_with_gated_disc();
        cd.set_backend(other.backend);
        assert!(
            read.lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .send(Ok(vec![0x55; 2048]))
                .is_err()
        );
        assert!(matches!(
            cd.wait_pending(),
            Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
        ));
        assert_eq!(
            cd.common.req_sense(),
            (CC_KEY_UNIT_ATTENTION, ASC_NOT_READY_TO_READY_CHANGE)
        );
    }

    #[test]
    fn unit_ready_checks_media_in_the_background() {
        let (mut cd, _, check) = gated_cdrom();
        assert!(matches!(test_unit_ready(&mut cd), ScsiCmdResult::Pending));
        assert!(cd.poll_pending().is_none());

        assert!(finish_check(&check, Ok(()), None));
        assert!(matches!(
            cd.poll_pending(),
            Some(Ok(ScsiCmdResult::Status(STATUS_GOOD)))
        ));
        assert!(cd.poll_pending().is_none());
    }

    #[test]
    fn unit_ready_reports_the_media_check() {
        let (mut cd, _, check) = gated_cdrom();
        assert!(matches!(test_unit_ready(&mut cd), ScsiCmdResult::Pending));
        let not_ready = CdromError::CheckCondition(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
        assert!(finish_check(&check, Err(not_ready), None));
        assert!(matches!(
            cd.poll_pending(),
            Some(Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION)))
        ));
        assert_eq!(
            cd.common.req_sense(),
            (CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT)
        );
    }

    #[test]
    fn media_check_gives_the_backend_the_disc_info() {
        let (mut cd, _, check) = gated_cdrom();
        assert!(matches!(test_unit_ready(&mut cd), ScsiCmdResult::Pending));
        let disc_info = DiscInfo {
            capacity: 2048,
            sessions: vec![],
            tracks: vec![],
        };
        assert!(finish_check(&check, Ok(()), Some(disc_info)));
        assert!(cd.poll_pending().is_some());
        assert_eq!(check.lock().unwrap().applied, Some(Some(2048)));
    }

    #[test]
    fn aborted_media_check_is_dropped() {
        let (mut cd, _, check) = gated_cdrom();
        assert!(matches!(test_unit_ready(&mut cd), ScsiCmdResult::Pending));
        cd.abort_pending();
        assert!(!finish_check(&check, Ok(()), None));
        assert!(cd.poll_pending().is_none());
        assert_eq!(check.lock().unwrap().applied, None);
    }

    #[test]
    fn wait_takes_the_media_check() {
        let (mut cd, _, check) = gated_cdrom();
        assert!(matches!(test_unit_ready(&mut cd), ScsiCmdResult::Pending));
        assert!(finish_check(&check, Ok(()), None));
        assert!(matches!(
            cd.wait_pending(),
            Ok(ScsiCmdResult::Status(STATUS_GOOD))
        ));
    }

    #[test]
    fn unit_ready_without_a_disc_answers_at_once() {
        let mut cd = ScsiTargetCdrom::new(None);
        assert!(matches!(
            test_unit_ready(&mut cd),
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert_eq!(
            cd.common.req_sense(),
            (CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT)
        );
    }

    /// READ TOC of the formatted TOC, from track 1, in LBA format
    fn read_toc_cmd(cd: &mut ScsiTargetCdrom) -> ScsiCmdResult {
        cd.cmd(&[0x43, 0, 0, 0, 0, 0, 1, 0, 100, 0], None).unwrap()
    }

    #[test]
    fn read_toc_checks_media_in_the_background() {
        let (mut cd, _, check) = gated_cdrom();
        assert!(matches!(read_toc_cmd(&mut cd), ScsiCmdResult::Pending));
        assert!(cd.poll_pending().is_none());

        assert!(finish_check(&check, Ok(()), None));
        match cd.poll_pending() {
            Some(Ok(ScsiCmdResult::DataIn(d))) => {
                // Header, track 1 and the lead-out
                assert_eq!(d.len(), 4 + 8 + 8);
                // First and last track
                assert_eq!(d[2..4], [1, 1]);
                // Track 1 starts at LBA 0, the lead-out at LBA 2
                assert_eq!(d[6], 1);
                assert_eq!(d[8..12], [0, 0, 0, 0]);
                assert_eq!(d[14], TRACK_LEADOUT);
                assert_eq!(d[16..20], [0, 0, 0, 2]);
            }
            other => panic!(
                "expected the TOC, got {:?}",
                other.map(|r| r.map(|r| other_status(&r)))
            ),
        }
        assert!(cd.poll_pending().is_none());
    }

    #[test]
    fn read_toc_reports_a_failed_media_check() {
        let (mut cd, _, check) = gated_cdrom();
        assert!(matches!(read_toc_cmd(&mut cd), ScsiCmdResult::Pending));
        let not_ready = CdromError::CheckCondition(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
        assert!(finish_check(&check, Err(not_ready), None));
        assert!(matches!(
            cd.wait_pending(),
            Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
        ));
        assert_eq!(
            cd.common.req_sense(),
            (CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT)
        );
    }

    #[test]
    fn read_toc_of_an_image_answers_at_once() {
        let dir = temp_dir("image_toc");
        write_file(&dir, "disc.iso", 4096);
        let mut cd = ScsiTargetCdrom::new(None);
        cd.load_media(&dir.join("disc.iso")).unwrap();

        match read_toc_cmd(&mut cd) {
            ScsiCmdResult::DataIn(d) => assert_eq!(d.len(), 4 + 8 + 8),
            other => panic!("expected the TOC, got {}", other_status(&other)),
        }

        drop(cd);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// PLAY AUDIO MSF from 00:02:00 (the first sector) to 00:02:01
    fn play_audio_cmd(cd: &mut ScsiTargetCdrom) -> ScsiCmdResult {
        cd.cmd(&[0x47, 0, 0, 0, 2, 0, 0, 2, 1, 0], None).unwrap()
    }

    /// Finishes the CDDA read the backend started with a sector of the given
    /// track type
    fn finish_cdda(cdda: &CddaSender, control: u8) {
        let sender = cdda.lock().unwrap().take().expect("no CDDA read started");
        let sector = RawSector {
            data: [0; RAW_SECTOR_LEN],
            qsub: QSub::new_mode1(control, 1, 1, LBA_START_SECTOR, LBA_START_SECTOR),
        };
        sender.send(Ok(sector)).unwrap();
    }

    #[test]
    fn play_audio_reads_the_first_sector_in_the_background() {
        let (mut cd, _, _, cdda) = gated_cdrom_with_cdda();
        assert!(matches!(play_audio_cmd(&mut cd), ScsiCmdResult::Pending));
        assert!(cd.poll_pending().is_none());
        assert!(cd.audio_state == AudioState::Stopped);

        finish_cdda(&cdda, AUDIO_TRACK);
        assert!(matches!(
            cd.poll_pending(),
            Some(Ok(ScsiCmdResult::Status(STATUS_GOOD)))
        ));
        assert!(cd.audio_state == AudioState::Playing);
        assert_eq!(cd.audio_pos, LBA_START_SECTOR);
        assert_eq!(cd.audio_stop, LBA_START_SECTOR + 1);
    }

    #[test]
    fn play_audio_from_a_data_track_fails() {
        let (mut cd, _, _, cdda) = gated_cdrom_with_cdda();
        assert!(matches!(play_audio_cmd(&mut cd), ScsiCmdResult::Pending));
        finish_cdda(&cdda, DATA_TRACK);
        assert!(matches!(
            cd.wait_pending(),
            Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
        ));
        assert_eq!(
            cd.common.req_sense(),
            (CC_KEY_ILLEGAL_REQUEST, ASC_ILLEGAL_MODE_FOR_THIS_TRACK)
        );
        assert!(cd.audio_state == AudioState::Stopped);
    }

    /// An audio sink that counts the buffers it gets, and is never full
    #[derive(Default)]
    struct CountingSink(Arc<Mutex<usize>>);

    impl AudioSink for CountingSink {
        fn send(&self, _buffer: crate::renderer::AudioBuffer) -> Result<()> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }

        fn is_full(&self) -> bool {
            false
        }

        fn is_empty(&self) -> bool {
            true
        }
    }

    /// A gated CD-ROM drive playing audio from the first sector, with an
    /// audio sink counting the buffers it gets
    fn gated_cdrom_playing() -> (ScsiTargetCdrom, CddaSender, Arc<Mutex<usize>>) {
        let (mut cd, _, _, cdda) = gated_cdrom_with_cdda();
        let sent = Arc::new(Mutex::new(0));
        cd.audio_sink = Some(Box::new(CountingSink(sent.clone())));
        cd.audio_state = AudioState::Playing;
        cd.audio_pos = LBA_START_SECTOR;
        cd.audio_stop = LBA_START_SECTOR + 2;
        (cd, cdda, sent)
    }

    #[test]
    fn audio_sectors_are_read_in_the_background() {
        let (mut cd, cdda, sent) = gated_cdrom_playing();

        cd.pump_audio().unwrap();
        assert_eq!(*sent.lock().unwrap(), 0);
        assert_eq!(cd.audio_pos, LBA_START_SECTOR);

        // Still reading
        cd.pump_audio().unwrap();
        assert_eq!(*sent.lock().unwrap(), 0);

        finish_cdda(&cdda, AUDIO_TRACK);
        cd.pump_audio().unwrap();
        assert_eq!(*sent.lock().unwrap(), 1);
        assert_eq!(cd.audio_pos, LBA_START_SECTOR + 1);

        // The next sector is read in turn
        cd.pump_audio().unwrap();
        assert!(cdda.lock().unwrap().is_some());
        assert_eq!(cd.audio_read.as_ref().unwrap().0, LBA_START_SECTOR + 1);
    }

    #[test]
    fn seek_drops_the_audio_sector_being_read() {
        let (mut cd, cdda, sent) = gated_cdrom_playing();
        cd.pump_audio().unwrap();
        let old = cdda.lock().unwrap().take().unwrap();

        cd.audio_pos = LBA_START_SECTOR + 1;
        cd.pump_audio().unwrap();
        // The old read was dropped, and one for the new position started
        assert!(old.send(Err(CdromError::BackendDead)).is_err());
        assert_eq!(cd.audio_read.as_ref().unwrap().0, LBA_START_SECTOR + 1);
        assert_eq!(*sent.lock().unwrap(), 0);
    }

    #[test]
    fn eject_drops_the_audio_sector_being_read() {
        let (mut cd, cdda, _) = gated_cdrom_playing();
        cd.pump_audio().unwrap();
        cd.eject_media();
        assert!(cd.audio_read.is_none());
        let sender = cdda.lock().unwrap().take().unwrap();
        assert!(sender.send(Err(CdromError::BackendDead)).is_err());
    }

    /// A backend whose loading in the background failed
    struct FailedLoadBackend(Option<anyhow::Error>);

    impl CdromBackend for FailedLoadBackend {
        fn check_media(&mut self) -> Started<MediaCheck> {
            Started::Done(Err(CdromError::BackendDead))
        }

        fn take_load_error(&mut self) -> Option<anyhow::Error> {
            self.0.take()
        }

        fn byte_len(&self) -> usize {
            0
        }

        fn read_bytes(&self, _offset: usize, _length: usize) -> Started<Vec<u8>> {
            Started::Done(Err(CdromError::BackendDead))
        }

        fn image_path(&self) -> Option<&Path> {
            None
        }

        fn sessions(&self) -> Option<&[SessionInfo]> {
            None
        }

        fn tracks(&self) -> Option<&[TrackInfo]> {
            None
        }

        fn read_cdda_sector(&self, _sector: u32) -> Started<RawSector> {
            Started::Done(Err(CdromError::BackendDead))
        }
    }

    /// A backend without a disc yet: loading one, or with none
    struct LoadingBackend {
        loading: bool,
    }

    impl CdromBackend for LoadingBackend {
        fn check_media(&mut self) -> Started<MediaCheck> {
            Started::Done(Err(CdromError::BackendDead))
        }

        fn is_loading(&self) -> bool {
            self.loading
        }

        fn byte_len(&self) -> usize {
            0
        }

        fn read_bytes(&self, _offset: usize, _length: usize) -> Started<Vec<u8>> {
            Started::Done(Err(CdromError::BackendDead))
        }

        fn image_path(&self) -> Option<&Path> {
            None
        }

        fn sessions(&self) -> Option<&[SessionInfo]> {
            None
        }

        fn tracks(&self) -> Option<&[TrackInfo]> {
            None
        }

        fn read_cdda_sector(&self, _sector: u32) -> Started<RawSector> {
            Started::Done(Err(CdromError::BackendDead))
        }
    }

    const READ_CAPACITY: [u8; 10] = [0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    /// A command, then the sense key and ASC/ASCQ that REQUEST SENSE reports
    fn cmd_with_sense(cd: &mut ScsiTargetCdrom, cdb: &[u8]) -> (ScsiCmdResult, u8, u16) {
        let result = cd.cmd(cdb, None).unwrap();
        let ScsiCmdResult::DataIn(sense) = cd.cmd(&[0x03, 0, 0, 0, 18, 0], None).unwrap() else {
            panic!("REQUEST SENSE returns data");
        };
        (
            result,
            sense[2] & 0x0F,
            u16::from_be_bytes([sense[12], sense[13]]),
        )
    }

    #[test]
    fn read_capacity_while_loading_is_becoming_ready() {
        let mut cd = ScsiTargetCdrom::new(None);
        cd.backend = Some(Box::new(LoadingBackend { loading: true }));
        let (result, key, asc) = cmd_with_sense(&mut cd, &READ_CAPACITY);
        assert!(matches!(
            result,
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert_eq!(key, CC_KEY_NOT_READY);
        assert_eq!(asc, ASC_LOGICAL_UNIT_BECOMING_READY);
    }

    #[test]
    fn read_capacity_without_a_disc_is_medium_not_present() {
        let mut cd = ScsiTargetCdrom::new(None);
        cd.backend = Some(Box::new(LoadingBackend { loading: false }));
        let (result, key, asc) = cmd_with_sense(&mut cd, &READ_CAPACITY);
        assert!(matches!(
            result,
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert_eq!(key, CC_KEY_NOT_READY);
        assert_eq!(asc, ASC_MEDIUM_NOT_PRESENT);
    }

    #[test]
    fn read_capacity_reports_the_last_block() {
        let (mut cd, _, _) = gated_cdrom();
        let ScsiCmdResult::DataIn(data) = cd.cmd(&READ_CAPACITY, None).unwrap() else {
            panic!("READ CAPACITY returns data");
        };
        // 4096 bytes in blocks of 2048: the last block is 1
        assert_eq!(data, [0, 0, 0, 1, 0, 0, 0x08, 0]);
    }

    const READ10_BLOCK0: [u8; 10] = [0x28, 0, 0, 0, 0, 0, 0, 0, 1, 0];

    #[test]
    fn read10_while_loading_is_becoming_ready() {
        let mut cd = ScsiTargetCdrom::new(None);
        cd.backend = Some(Box::new(LoadingBackend { loading: true }));
        let (result, key, asc) = cmd_with_sense(&mut cd, &READ10_BLOCK0);
        assert!(matches!(
            result,
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert_eq!(key, CC_KEY_NOT_READY);
        assert_eq!(asc, ASC_LOGICAL_UNIT_BECOMING_READY);
    }

    #[test]
    fn read6_without_a_disc_is_medium_not_present() {
        let mut cd = ScsiTargetCdrom::new(None);
        cd.backend = Some(Box::new(LoadingBackend { loading: false }));
        let (result, key, asc) = cmd_with_sense(&mut cd, &[0x08, 0, 0, 0, 1, 0]);
        assert!(matches!(
            result,
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert_eq!(key, CC_KEY_NOT_READY);
        assert_eq!(asc, ASC_MEDIUM_NOT_PRESENT);
    }

    #[test]
    fn read10_past_block_0_without_a_disc_is_medium_not_present() {
        let mut cd = ScsiTargetCdrom::new(None);
        cd.backend = Some(Box::new(LoadingBackend { loading: false }));
        // No blocks, so block 5 is past the end of nothing
        let (result, key, asc) = cmd_with_sense(&mut cd, &[0x28, 0, 0, 0, 0, 5, 0, 0, 1, 0]);
        assert!(matches!(
            result,
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert_eq!(key, CC_KEY_NOT_READY);
        assert_eq!(asc, ASC_MEDIUM_NOT_PRESENT);
    }

    #[test]
    fn play_audio_while_loading_is_becoming_ready() {
        let mut cd = ScsiTargetCdrom::new(None);
        cd.backend = Some(Box::new(LoadingBackend { loading: true }));
        // PLAY AUDIO MSF from 00:02:00 to 00:03:00
        let (result, key, asc) = cmd_with_sense(&mut cd, &[0x47, 0, 0, 0, 2, 0, 0, 3, 0, 0]);
        assert!(matches!(
            result,
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert_eq!(key, CC_KEY_NOT_READY);
        assert_eq!(asc, ASC_LOGICAL_UNIT_BECOMING_READY);
    }

    #[test]
    fn audio_scan_while_loading_is_becoming_ready() {
        let mut cd = ScsiTargetCdrom::new(None);
        cd.backend = Some(Box::new(LoadingBackend { loading: true }));
        let (result, key, asc) = cmd_with_sense(&mut cd, &[0xCD, 0x10, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(matches!(
            result,
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert_eq!(key, CC_KEY_NOT_READY);
        assert_eq!(asc, ASC_LOGICAL_UNIT_BECOMING_READY);
    }

    #[test]
    fn read10_past_the_end_is_still_out_of_range() {
        // Two blocks: block 5 is past the end
        let (mut cd, _, _) = gated_cdrom();
        let (result, key, asc) = cmd_with_sense(&mut cd, &[0x28, 0, 0, 0, 0, 5, 0, 0, 1, 0]);
        assert!(matches!(
            result,
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert_eq!(key, CC_KEY_ILLEGAL_REQUEST);
        assert_eq!(asc, ASC_LOGICAL_BLOCK_ADDRESS_OUT_OF_RANGE);
    }

    fn cdrom_with_failed_load() -> ScsiTargetCdrom {
        let mut cd = ScsiTargetCdrom::new(None);
        cd.backend = Some(Box::new(FailedLoadBackend(Some(anyhow!("no drive")))));
        cd
    }

    #[test]
    fn failed_load_is_reported_then_ejected() {
        let mut cd = cdrom_with_failed_load();
        match cd.take_event() {
            Some(ScsiTargetEvent::LoadFailed(msg)) => assert_eq!(msg, "no drive"),
            _ => panic!("expected LoadFailed"),
        }
        assert!(cd.backend.is_none());
        assert!(matches!(
            cd.take_event(),
            Some(ScsiTargetEvent::MediaEjected)
        ));
        assert!(cd.take_event().is_none());
    }

    #[test]
    fn failed_load_found_by_a_command_is_still_reported() {
        let mut cd = cdrom_with_failed_load();
        // The guest finds the drive dead first, which ejects it
        assert!(matches!(
            test_unit_ready(&mut cd),
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        assert!(cd.backend.is_none());
        match cd.take_event() {
            Some(ScsiTargetEvent::LoadFailed(msg)) => assert_eq!(msg, "no drive"),
            _ => panic!("expected LoadFailed"),
        }
        assert!(matches!(
            cd.take_event(),
            Some(ScsiTargetEvent::MediaEjected)
        ));
    }
}
