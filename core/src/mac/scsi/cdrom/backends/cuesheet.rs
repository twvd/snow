use anyhow::{Result, anyhow, bail};
use encoding_rs::WINDOWS_1252;
use encoding_rs_rw::DecodingReader;
use std::{
    cell::RefCell,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    str::CharIndices,
};
use symphonia::core::{
    audio::{Channels, SampleBuffer, SignalSpec},
    codecs::Decoder,
    formats::{FormatReader, SeekMode, SeekTo},
    io::MediaSourceStream,
    units::TimeBase,
};

use crate::mac::scsi::{
    ASC_ILLEGAL_MODE_FOR_THIS_TRACK, ASC_UNRECOVERED_READ_ERROR, CC_KEY_ILLEGAL_REQUEST,
    CC_KEY_MEDIUM_ERROR,
    cdrom::{
        AUDIO_FRAMES_PER_SEC, AUDIO_FRAMES_PER_SECTOR, AUDIO_TRACK, CdromBackend, CdromError,
        DATA_TRACK, LBA_START_SECTOR, Msf, QSub, RAW_SECTOR_LEN, RawSector, SessionInfo, TrackInfo,
        get_track_at_sector,
    },
};

// .cue file reference:
//
// [LIBODRAW]: <https://github.com/libyal/libodraw/blob/main/documentation/CUE%20sheet%20format.asciidoc>
// [LIBODRAW-RAW]: <https://github.com/libyal/libodraw/blob/main/documentation/Optical%20disc%20RAW%20format.asciidoc>

fn peek_char(c: &CharIndices) -> Option<char> {
    Some(c.clone().next()?.1)
}

fn skip_whitespace(reader: &mut CharIndices) {
    while peek_char(reader)
        .map(|c| c.is_whitespace())
        .unwrap_or(false)
    {
        reader.next();
    }
}

fn read_cue_word<'a>(reader: &'a mut CharIndices) -> Option<&'a str> {
    skip_whitespace(reader);

    peek_char(reader)?;

    let (start, start_offset) = (reader.as_str(), reader.offset());

    while peek_char(reader)
        .map(|c| !c.is_whitespace())
        .unwrap_or(false)
    {
        reader.next();
    }

    Some(&start[..reader.offset() - start_offset])
}

fn read_cue_path<'a>(reader: &'a mut CharIndices) -> Option<&'a str> {
    skip_whitespace(reader);

    peek_char(reader)?;

    if peek_char(reader).unwrap() == '"' {
        reader.next();

        let (start, start_offset) = (reader.as_str(), reader.offset());

        while peek_char(reader).map(|c| c != '"').unwrap_or(false) {
            reader.next();
        }

        let result = &start[..reader.offset() - start_offset];

        // Skip final '"' if present
        if peek_char(reader).is_some() {
            reader.next();
        }

        Some(result)
    } else {
        read_cue_word(reader)
    }
}

fn read_cue_msf(reader: &mut CharIndices) -> Result<Msf> {
    let msf_str = read_cue_word(reader).ok_or_else(|| anyhow!("Invalid MSF timecode"))?;

    let mut split = msf_str.split(':');
    let m: u8 = split
        .next()
        .ok_or_else(|| anyhow!("Invalid MSF timecode"))?
        .parse()?;
    let s: u8 = split
        .next()
        .ok_or_else(|| anyhow!("Invalid MSF timecode"))?
        .parse()?;
    let f: u8 = split
        .next()
        .ok_or_else(|| anyhow!("Invalid MSF timecode"))?
        .parse()?;
    Ok(Msf::new(m, s, f))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinarySourceFormat {
    Raw2352,
    Mode1_2048,
    // TODO: support more formats
}

impl BinarySourceFormat {
    fn bytes_per_sector(self) -> u64 {
        match self {
            Self::Raw2352 => 2352,
            Self::Mode1_2048 => 2048,
        }
    }
}

#[derive(Debug)]
enum SectorSource {
    /// Sectors filled with zeroes. Used for pregaps and postgaps.
    Zeros,
    /// Sectors sourced from a data file
    BinaryFile {
        /// Index of file in the `binary_files` array
        file_idx: usize,
        /// Byte offset within the file
        file_offset: u64,
        format: BinarySourceFormat,
    },
    /// Sectors sourced from an audio file
    AudioFile {
        /// Index of file in the `audio_files` array
        file_idx: usize,
        /// Timestamp in sectors (1/75 second)
        timestamp_in_sectors: u32,
    },
}

#[derive(Debug)]
struct SectorMapEntry {
    sector: u32,
    sector_count: u32,
    source: SectorSource,
}

struct BinaryFileCursor {
    /// Index of file in `binary_files` array
    idx: usize,
    /// Byte offset within file
    offset: u64,
    /// Max bytes in file
    size: u64,
}

struct AudioFileCursor {
    /// Index of file in `audio_files` array
    idx: usize,
    /// Audio timestamp in sectors (1/75 second)
    sector: u32,
    /// Max sectors in file
    sector_count: u64,
}

enum FileCursorInfo {
    Binary(BinaryFileCursor),
    Audio(AudioFileCursor),
}

struct FileCursor {
    // Sector number within file
    sector: u32,
    info: FileCursorInfo,
}

struct SectorMapBuilder {
    /// Absolute sector number
    abs_cursor: u32,
    map: Vec<SectorMapEntry>,

    file_cursor: Option<FileCursor>,
    /// Most recent source format for binary file
    curr_format: Option<BinarySourceFormat>,
    /// Source format to switch to at the next Index point
    next_format: Option<BinarySourceFormat>,
}

impl SectorMapBuilder {
    fn new() -> Self {
        // Data files always start at time 00:02:00 (sector 150).
        // This means there are 150 sectors located in the lead-in area which
        // cannot be specified by the .bin/.cue format.
        // These sectors are usually empty, but sometimes they contain various
        // data such as CD-TEXT information.
        Self {
            abs_cursor: LBA_START_SECTOR,
            file_cursor: None,
            curr_format: None,
            next_format: None,
            map: vec![],
        }
    }

    fn declare_binary_file(&mut self, idx: usize, size: u64) -> Result<()> {
        // If a file was previously declared, commit the rest of its data before
        // starting the new file.
        self.commit_rest_of_file()?;

        self.file_cursor = Some(FileCursor {
            sector: 0,
            info: FileCursorInfo::Binary(BinaryFileCursor {
                offset: 0,
                size,
                idx,
            }),
        });
        self.curr_format = None;
        self.next_format = None;

        Ok(())
    }

    fn declare_audio_file(&mut self, idx: usize, sector_count: u64) -> Result<()> {
        // If a file was previously declared, commit the rest of its data before
        // starting the new file.
        self.commit_rest_of_file()?;

        self.file_cursor = Some(FileCursor {
            sector: 0,
            info: FileCursorInfo::Audio(AudioFileCursor {
                idx,
                sector: 0,
                sector_count,
            }),
        });
        self.curr_format = None;
        self.next_format = None;

        Ok(())
    }

    fn declare_track(&mut self, format: BinarySourceFormat) {
        self.next_format = Some(format);
    }

    fn declare_index(&mut self, sector: u32) -> Result<()> {
        // Add any previous file data up to this point
        self.commit_file_up_to(sector)?;
        self.curr_format = self.next_format;
        Ok(())
    }

    /// Commit the last data file up to a given sector
    fn commit_file_up_to(&mut self, up_to: u32) -> Result<()> {
        let Some(file_cursor) = &mut self.file_cursor else {
            bail!("No file declared");
        };

        if up_to < file_cursor.sector {
            bail!("File sector number cannot decrease");
        }

        if up_to == file_cursor.sector {
            // Nothing to do
            return Ok(());
        }

        let Some(curr_format) = self.curr_format else {
            bail!("No format declared for file data");
        };

        let additional_sectors = up_to - file_cursor.sector;

        match &mut file_cursor.info {
            FileCursorInfo::Binary(file_cursor_info) => {
                if let Some(entry) = self.map.last_mut()
                    && let SectorSource::BinaryFile {
                        file_idx, format, ..
                    } = entry.source
                    && file_idx == file_cursor_info.idx
                    && Some(format) == self.curr_format
                {
                    // Add more sectors to the existing map entry
                    entry.sector_count += additional_sectors;
                } else if additional_sectors > 0 {
                    // Start a new map entry
                    self.map.push(SectorMapEntry {
                        sector: self.abs_cursor,
                        sector_count: additional_sectors,
                        source: SectorSource::BinaryFile {
                            file_idx: file_cursor_info.idx,
                            file_offset: file_cursor_info.offset,
                            format: curr_format,
                        },
                    });
                }

                file_cursor_info.offset +=
                    additional_sectors as u64 * curr_format.bytes_per_sector();
            }
            FileCursorInfo::Audio(file_cursor_info) => {
                if let Some(entry) = self.map.last_mut()
                    && let SectorSource::AudioFile { file_idx, .. } = entry.source
                    && file_idx == file_cursor_info.idx
                {
                    // Add more sectors to the existing map entry
                    entry.sector_count += additional_sectors;
                } else if additional_sectors > 0 {
                    // Start a new map entry
                    self.map.push(SectorMapEntry {
                        sector: self.abs_cursor,
                        sector_count: additional_sectors,
                        source: SectorSource::AudioFile {
                            file_idx: file_cursor_info.idx,
                            timestamp_in_sectors: file_cursor_info.sector,
                        },
                    });
                }

                file_cursor_info.sector += additional_sectors;
            }
        }

        file_cursor.sector += additional_sectors;
        self.abs_cursor += additional_sectors;

        Ok(())
    }

    /// Add sectors of silence (all zeroes)
    fn add_gap(&mut self, sectors: u32) {
        if let Some(entry) = self.map.last_mut()
            && matches!(entry.source, SectorSource::Zeros)
        {
            // Add more sectors to the existing map entry
            entry.sector_count += sectors;
        } else if sectors > 0 {
            // Start a new map entry
            self.map.push(SectorMapEntry {
                sector: self.abs_cursor,
                sector_count: sectors,
                source: SectorSource::Zeros,
            });
        }

        self.abs_cursor += sectors;
    }

    fn commit_rest_of_file(&mut self) -> Result<()> {
        let Some(file_cursor) = &self.file_cursor else {
            // Nothing to do
            return Ok(());
        };

        let Some(curr_format) = self.curr_format else {
            bail!("No format declared for file data (committing rest of file)");
        };

        let remaining_sectors: u32 = match &file_cursor.info {
            FileCursorInfo::Binary(file_cursor_info) => {
                let remaining_bytes = file_cursor_info.size - file_cursor_info.offset;
                (remaining_bytes / curr_format.bytes_per_sector()).try_into()?
            }
            FileCursorInfo::Audio(file_cursor_info) => {
                (file_cursor_info.sector_count - file_cursor_info.sector as u64).try_into()?
            }
        };

        self.commit_file_up_to(file_cursor.sector + remaining_sectors)?;

        self.file_cursor = None;
        Ok(())
    }

    fn build(mut self) -> Result<Vec<SectorMapEntry>> {
        self.commit_rest_of_file()?;
        Ok(self.map)
    }
}

struct DecodedPacket {
    timestamp: u64,
    sample_buf: SampleBuffer<i16>,
}

impl DecodedPacket {
    fn frame_count(&self) -> usize {
        self.sample_buf.len() / 2 // 2 samples per frame
    }
}

struct AudioFile {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,

    /// Timestamp cursor of stream-in (measured in frames)
    stream_ts: u64,
    /// Timestamp cursor of the next packet that will be fetched by the format_reader (measured in frames)
    next_packet_ts: u64,
    /// Current decoded packet
    decoded_packet: Option<DecodedPacket>,
}

impl AudioFile {
    /// Load an audio file. Returns (self, number of frames).
    fn new(path: &Path) -> Result<(Self, u64)> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let probed = symphonia::default::get_probe().format(
            &Default::default(),
            mss,
            &Default::default(),
            &Default::default(),
        )?;

        let symph_track = probed
            .format
            .default_track()
            .ok_or_else(|| anyhow!("No audio tracks found in audio file"))?;
        log::debug!("Probed symphonia track: {:?}", symph_track);

        if symph_track.codec_params.sample_rate != Some(AUDIO_FRAMES_PER_SEC) {
            // TODO: support arbitrary sample rates
            // (some files might be compressed to lower sample rates)
            bail!(
                "Invalid sample rate {:?} for CD audio",
                symph_track.codec_params.sample_rate
            );
        }

        if symph_track.codec_params.channels != Some(Channels::FRONT_LEFT | Channels::FRONT_RIGHT) {
            bail!(
                "Invalid channels {:?} for CD audio",
                symph_track.codec_params.channels
            );
        }

        if symph_track.codec_params.time_base != Some(TimeBase::new(1, AUDIO_FRAMES_PER_SEC)) {
            bail!(
                "Invalid time base {:?} for CD audio",
                symph_track.codec_params.time_base
            );
        }

        let n_frames = symph_track
            .codec_params
            .n_frames
            .ok_or_else(|| anyhow!("Audio file has no defined length"))?;

        let decoder = symphonia::default::get_codecs()
            .make(&symph_track.codec_params, &Default::default())?;

        Ok((
            Self {
                format_reader: probed.format,
                decoder,
                stream_ts: 0,
                next_packet_ts: 0,
                decoded_packet: None,
            },
            n_frames,
        ))
    }

    fn seek(&mut self, timestamp: u64) -> Result<()> {
        let needs_seek = match &self.decoded_packet {
            Some(decoded_packet) => !(decoded_packet.timestamp
                ..=decoded_packet.timestamp + decoded_packet.frame_count() as u64)
                .contains(&timestamp),
            None => true,
        };

        // If timestamp is not within the last decoded packet or immediately after it,
        // we need to perform a seek.
        if needs_seek {
            self.decoded_packet = None;
            self.decoder.reset();

            let seeked_to = self.format_reader.seek(
                SeekMode::Accurate,
                SeekTo::TimeStamp {
                    ts: timestamp,
                    track_id: self.format_reader.default_track().unwrap().id,
                },
            )?;

            self.next_packet_ts = seeked_to.actual_ts;
        }

        self.stream_ts = timestamp;
        Ok(())
    }

    fn stream_in_frames(&mut self, frame_count: usize) -> Result<Vec<i16>> {
        assert!(
            self.decoded_packet.is_none()
                || self.stream_ts >= self.decoded_packet.as_ref().unwrap().timestamp,
            "Stream cannot go backwards; use seek"
        );

        let sample_count = frame_count * 2;
        let mut result = Vec::with_capacity(sample_count);

        while result.len() < sample_count {
            if self.decoded_packet.is_none() || self.stream_ts >= self.next_packet_ts {
                let old_buf = self.decoded_packet.take().map(|p| p.sample_buf);

                let packet = self.format_reader.next_packet()?;
                let audio_buf = self.decoder.decode(&packet)?;
                let audio_buf_frame_count = audio_buf.frames();

                // Reuse old SampleBuffer if possible
                let mut converted_buf = if let Some(old_buf) = old_buf
                    && old_buf.capacity() >= audio_buf.capacity()
                {
                    old_buf
                } else {
                    SampleBuffer::<i16>::new(
                        audio_buf.capacity() as u64,
                        SignalSpec::new(
                            AUDIO_FRAMES_PER_SEC,
                            Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
                        ),
                    )
                };

                converted_buf.copy_interleaved_ref(audio_buf);

                self.decoded_packet = Some(DecodedPacket {
                    timestamp: self.next_packet_ts,
                    sample_buf: converted_buf,
                });
                self.next_packet_ts += audio_buf_frame_count as u64;
            }

            let decoded_packet = self.decoded_packet.as_ref().unwrap();
            let frame_in_packet = usize::try_from(self.stream_ts - decoded_packet.timestamp)? * 2;
            let samples = &decoded_packet.sample_buf.samples()[frame_in_packet..];
            let remaining = sample_count - result.len();
            let samples = &samples[..remaining.min(samples.len())];
            result.extend(samples);

            self.stream_ts += samples.len() as u64 / 2;
        }

        Ok(result)
    }

    /// Read frames from the audio file.
    ///
    /// A frame is comprised of a left and right stereo sample (2 samples, or 4 bytes).
    /// `timestamp` is the number of the frame within the file.
    fn read_frames(&mut self, timestamp: u64, frame_count: usize) -> Result<Vec<i16>> {
        self.seek(timestamp)?;
        self.stream_in_frames(frame_count)
    }
}

pub struct CuesheetCdromBackend {
    cue_path: PathBuf,
    binary_files: Vec<File>,
    audio_files: Vec<RefCell<AudioFile>>,
    sessions: Vec<SessionInfo>,
    tracks: Vec<TrackInfo>,
    /// Map of sectors. Entries are sorted in order of increasing `sector` field.
    /// This table maps absolute sector numbers to data files. This is NOT the
    /// table of contents; it doesn't carry information about tracks.
    sector_map: Vec<SectorMapEntry>,
}

impl CuesheetCdromBackend {
    pub fn new(path: &Path) -> Result<Self> {
        let cue_dir = path.parent().unwrap();

        // Cuesheet files do not have a standardized text encoding. Tools that
        // generate cue files tend to use the encoding of their native platform.
        // In practice, most cue files are generated in Windows-1252 format.
        // This becomes important if a cue file contains special characters such
        // as “ or ” (which CAN appear in filenames).
        // We use a decoder that defaults to Windows-1252 but switches to UTF-8,
        // UTF-16 LE or UTF-16 BE if it detects a byte-order mark.
        let cue_file = BufReader::new(File::open(path)?);
        let cue_file = DecodingReader::new(cue_file, WINDOWS_1252.new_decoder());
        let cue_file = BufReader::new(cue_file); // For the `lines` method

        let mut binary_files: Vec<File> = vec![];
        let mut audio_files: Vec<RefCell<AudioFile>> = vec![];
        let mut sector_map = SectorMapBuilder::new();

        let mut track_num = 0u8;
        let mut track_control = DATA_TRACK;
        let mut gap_sectors = 0;
        let mut session_has_tracks = false;

        let mut sessions = vec![SessionInfo {
            number: 1,
            disc_type: 0x00,
            leadin: 0,
            leadout: LBA_START_SECTOR,
        }];
        let mut tracks = vec![];

        // FIXME: I believe cue files have one command per line and never split commands across multiple lines. Is this true?
        for line in cue_file.lines() {
            let line = line?;
            let mut chars = line.char_indices();

            let Some(command) = read_cue_word(&mut chars) else {
                continue;
            };

            match command {
                "FILE" => {
                    let file_path = read_cue_path(&mut chars)
                        .ok_or_else(|| anyhow!("Failed to parse FILE command"))?;
                    let file_path = cue_dir.join(Path::new(&file_path));

                    let file_type = read_cue_word(&mut chars)
                        .ok_or_else(|| anyhow!("Failed to parse FILE command"))?;
                    match file_type {
                        "BINARY" => {
                            log::info!("Loading binary file from {}", file_path.to_string_lossy());

                            let file = File::open(file_path)?;
                            let file_len = file.metadata()?.len();
                            binary_files.push(file);

                            sector_map.declare_binary_file(binary_files.len() - 1, file_len)?;
                        }
                        "WAVE" => {
                            log::info!("Loading audio file from {}", file_path.to_string_lossy());

                            let (audio_file, n_frames) = AudioFile::new(&file_path)?;
                            audio_files.push(RefCell::new(audio_file));

                            let sector_count = n_frames.div_ceil(AUDIO_FRAMES_PER_SECTOR as u64);
                            sector_map.declare_audio_file(audio_files.len() - 1, sector_count)?;
                        }
                        _ => bail!("Unsupported file type `{}` in cuesheet", file_type),
                    }
                }
                "TRACK" => {
                    track_num = read_cue_word(&mut chars)
                        .ok_or_else(|| anyhow!("Invalid TRACK command"))?
                        .parse()?;
                    let track_form_str = read_cue_word(&mut chars)
                        .ok_or_else(|| anyhow!("Invalid TRACK command"))?;
                    let source_format;
                    (source_format, track_control) = match track_form_str {
                        "AUDIO" => (BinarySourceFormat::Raw2352, AUDIO_TRACK),
                        "MODE1/2352" => (BinarySourceFormat::Raw2352, DATA_TRACK),
                        "MODE1/2048" => (BinarySourceFormat::Mode1_2048, DATA_TRACK),
                        "MODE2/2352" => {
                            if !session_has_tracks {
                                sessions.last_mut().unwrap().disc_type = 0x20; // CD data XA disc with first track in Mode 2
                            }
                            (BinarySourceFormat::Raw2352, DATA_TRACK)
                        }
                        _ => bail!("Unsupported track form {}", track_form_str),
                    };
                    sector_map.declare_track(source_format);
                }
                "INDEX" => {
                    let index_num: u8 = read_cue_word(&mut chars)
                        .ok_or_else(|| anyhow!("Invalid INDEX command"))?
                        .parse()?;

                    // Index sector is relative to the current data file
                    let file_sector = read_cue_msf(&mut chars)?.to_sector();

                    sector_map.declare_index(file_sector)?;

                    // Add pregaps/postgaps here
                    sector_map.add_gap(gap_sectors);
                    gap_sectors = 0;

                    if index_num == 1 {
                        // The track will officially begin at index 1.
                        tracks.push(TrackInfo {
                            tno: track_num,
                            session: sessions.last().unwrap().number,
                            control: track_control,
                            sector: sector_map.abs_cursor,
                        });
                    }

                    if !session_has_tracks {
                        // Set the lead-in to 150 sectors before the first track
                        // FIXME: lead-in might be set incorrectly if pregaps/postgaps are present...
                        sessions.last_mut().unwrap().leadin =
                            sector_map.abs_cursor.saturating_sub(LBA_START_SECTOR);
                    }

                    session_has_tracks = true;
                }
                "PREGAP" | "POSTGAP" => {
                    let duration = read_cue_msf(&mut chars)?.to_sector();
                    // Zeros sectors will be added to the map by the INDEX command
                    gap_sectors += duration;
                }
                "REM" => {
                    let Some(rem_cmd) = read_cue_word(&mut chars) else {
                        continue;
                    };

                    match rem_cmd {
                        "LEAD-OUT" => {
                            if let Ok(leadout_msf) = read_cue_msf(&mut chars) {
                                sector_map.commit_file_up_to(leadout_msf.to_sector())?;

                                sector_map.add_gap(gap_sectors);
                                gap_sectors = 0;

                                sessions.last_mut().unwrap().leadout = sector_map.abs_cursor;
                            } else {
                                log::warn!("Failed to parse MSF in REM LEAD-OUT");
                            }
                        }
                        "SESSION" => {
                            if let Some(new_session) =
                                read_cue_word(&mut chars).and_then(|w| w.parse::<u8>().ok())
                            {
                                let last_session = sessions.last().unwrap();
                                if new_session == last_session.number + 1 {
                                    sessions.push(SessionInfo {
                                        number: new_session,
                                        disc_type: 0x00,
                                        leadin: last_session.leadout,
                                        leadout: last_session.leadout,
                                    });

                                    session_has_tracks = false;
                                } else if !(new_session == 1 && sessions.len() == 1) {
                                    log::warn!(
                                        "Unexpected session number {} in REM SESSION command",
                                        new_session
                                    );
                                }
                            } else {
                                log::warn!("Unexpected token in REM SESSION command");
                            }
                        }
                        _ => (), // Just a regular REM comment; ignore
                    }
                }
                "CATALOG" | "PERFORMER" | "TITLE" => (), // Ignore
                _ => log::warn!("Unknown cuesheet command {} ignored", command),
            }
        }

        log::debug!("Tracks: {:#?}", tracks);

        // In case the final track has a postgap...
        sector_map.commit_rest_of_file()?;
        sector_map.add_gap(gap_sectors);

        let sector_map = sector_map.build()?;
        log::debug!("Sector map: {:#?}", sector_map);

        let final_leadout = sector_map
            .last()
            .map(|e| e.sector + e.sector_count)
            .unwrap_or(LBA_START_SECTOR);

        if sessions.last().unwrap().leadout < final_leadout {
            sessions.last_mut().unwrap().leadout = final_leadout;
        }

        log::debug!("Sessions: {:#?}", sessions);

        Ok(Self {
            cue_path: path.into(),
            binary_files,
            audio_files,
            sessions,
            tracks,
            sector_map,
        })
    }

    fn find_map_entry_for_sector(&self, sector: u32) -> Option<&SectorMapEntry> {
        let idx = match self.sector_map.binary_search_by(|e| e.sector.cmp(&sector)) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        self.sector_map
            .get(idx)
            .filter(|e| (e.sector..e.sector + e.sector_count).contains(&sector))
    }

    fn read_raw_sector(&self, sector: u32) -> Result<RawSector, CdromError> {
        let map_entry = self
            .find_map_entry_for_sector(sector)
            .ok_or_else(|| anyhow!("Sector {} not found in sector map", sector))?;

        let rel_sector = sector - map_entry.sector;

        let track = get_track_at_sector(&self.tracks, sector)
            .ok_or_else(|| anyhow!("No track found at sector {}", sector))?;
        if sector >= self.sessions[track.session as usize - 1].leadout {
            return Err(anyhow!("Sector {} was past the lead-out", sector).into());
        }

        let data = match map_entry.source {
            SectorSource::Zeros => [0; RAW_SECTOR_LEN],
            SectorSource::BinaryFile {
                file_idx,
                file_offset,
                format,
            } => {
                // It turns out you don't need a &mut File to seek and read!
                // Just call seek and read on a `&File`. This will clobber the file cursor,
                // so use with caution.
                let mut file: &File = &self.binary_files[file_idx];
                file.seek(SeekFrom::Start(
                    file_offset + rel_sector as u64 * format.bytes_per_sector(),
                ))
                .map_err(|e| anyhow!(e))?;

                match format {
                    BinarySourceFormat::Raw2352 => {
                        let mut result = [0u8; RAW_SECTOR_LEN];
                        file.read_exact(&mut result).map_err(|e| anyhow!(e))?;
                        result
                    }
                    BinarySourceFormat::Mode1_2048 => {
                        // Reconstruct Mode 1 sector from 2048-byte user data.
                        // Only the most important fields are filled.
                        let mut result = [0u8; RAW_SECTOR_LEN];
                        // Sync
                        result[0..12]
                            .copy_from_slice(b"\x00\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x00");
                        // Mode
                        result[15] = 1;
                        // User data
                        file.read_exact(&mut result[16..][..2048])
                            .map_err(|e| anyhow!(e))?;
                        // TODO: fill out more fields?
                        result
                    }
                }
            }
            SectorSource::AudioFile {
                file_idx,
                timestamp_in_sectors,
            } => {
                let mut file = self.audio_files[file_idx].borrow_mut();

                let ts = (timestamp_in_sectors as u64 + rel_sector as u64)
                    * AUDIO_FRAMES_PER_SECTOR as u64;
                let samples = file.read_frames(ts, AUDIO_FRAMES_PER_SECTOR)?;

                let mut result = [0u8; RAW_SECTOR_LEN];
                for (s, out) in samples.into_iter().zip(result.as_chunks_mut::<2>().0) {
                    out.copy_from_slice(&s.to_le_bytes());
                }

                result
            }
        };

        Ok(RawSector {
            data,
            // TODO: use correct INDEX (should be 0 during pre-gap)
            qsub: QSub::new_mode1(track.control, track.tno, 1, sector, track.sector),
        })
    }
}

impl CdromBackend for CuesheetCdromBackend {
    fn check_media(&mut self) -> Result<(), CdromError> {
        Ok(())
    }

    fn byte_len(&self) -> usize {
        // To find the capacity, treat each sector (except the lead-in) as a block
        // containing 2048 bytes.
        // However, only sectors in a Data track in Mode 1 or Mode 2 Form 1 are readable
        // via `read_bytes`.
        let final_leadout = self
            .sector_map
            .last()
            .map(|e| e.sector + e.sector_count)
            .unwrap_or(LBA_START_SECTOR);
        let final_lba = final_leadout.saturating_sub(LBA_START_SECTOR);
        usize::try_from(final_lba).unwrap() * 2048
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError> {
        let mut result = Vec::<u8>::with_capacity(length);

        let mut sector: u32 =
            LBA_START_SECTOR + u32::try_from(offset / 2048).map_err(|e| anyhow!(e))?;
        let mut data_offset = offset % 2048;
        while result.len() < length {
            let raw_sector = self.read_raw_sector(sector)?;

            if !raw_sector.qsub.is_data() {
                log::warn!("Tried to read bytes from non-data sector {}", sector);
                return Err(CdromError::CheckCondition(
                    CC_KEY_ILLEGAL_REQUEST,
                    ASC_ILLEGAL_MODE_FOR_THIS_TRACK,
                ));
            }

            let raw_sector = raw_sector.data;

            // Check sync field
            let sync = &raw_sector[0..12];
            if sync != *b"\x00\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x00" {
                log::warn!("Failed to read sector {}: invalid sync field", sector);
                return Err(CdromError::CheckCondition(
                    CC_KEY_MEDIUM_ERROR,
                    ASC_UNRECOVERED_READ_ERROR,
                ));
            }

            // Check mode field
            let mode = raw_sector[15];
            let user_data = match mode {
                // TODO: check error detection codes?
                1 => &raw_sector[16..][..2048],
                2 => {
                    let subheader = &raw_sector[16..20];
                    if *subheader != raw_sector[20..24] {
                        log::warn!(
                            "Failed to read sector {}: Mode 2 subheader mismatch",
                            sector
                        );
                        return Err(CdromError::CheckCondition(
                            CC_KEY_MEDIUM_ERROR,
                            ASC_UNRECOVERED_READ_ERROR,
                        ));
                    }

                    if subheader[2] & (1 << 5) != 0 {
                        log::warn!(
                            "Failed to read sector {}: Cannot read Mode 2 Form 2",
                            sector
                        );
                        return Err(CdromError::CheckCondition(
                            CC_KEY_MEDIUM_ERROR,
                            ASC_UNRECOVERED_READ_ERROR,
                        ));
                    }

                    // TODO: check error detection codes?
                    &raw_sector[24..][..2048]
                }
                _ => {
                    log::warn!(
                        "Failed to read sector {}: Unexpected mode field {}",
                        sector,
                        mode
                    );
                    return Err(CdromError::CheckCondition(
                        CC_KEY_MEDIUM_ERROR,
                        ASC_UNRECOVERED_READ_ERROR,
                    ));
                }
            };

            result.extend_from_slice(&user_data[data_offset..]);

            sector += 1;
            data_offset = 0;
        }

        result.truncate(length);
        Ok(result)
    }

    fn image_path(&self) -> Option<&Path> {
        Some(&self.cue_path)
    }

    fn sessions(&self) -> Option<&[SessionInfo]> {
        Some(&self.sessions)
    }

    fn tracks(&self) -> Option<&[TrackInfo]> {
        Some(&self.tracks)
    }

    fn read_cdda_sector(&self, sector: u32) -> Result<RawSector, CdromError> {
        self.read_raw_sector(sector)
    }
}
