use anyhow::{Result, anyhow, bail};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    iter::Peekable,
    path::{Path, PathBuf},
    str::Chars,
};

use crate::mac::scsi::{
    ASC_ILLEGAL_MODE_FOR_THIS_TRACK, ASC_UNRECOVERED_READ_ERROR, CC_KEY_ILLEGAL_REQUEST,
    CC_KEY_MEDIUM_ERROR,
    cdrom::{
        AUDIO_TRACK, CdromBackend, CdromError, DATA_TRACK, LBA_START_SECTOR, Msf, RAW_SECTOR_LEN,
        RawSector, SessionInfo, TrackInfo, get_track_at_sector,
    },
};

// .cue file reference:
//
// [LIBODRAW]: <https://github.com/libyal/libodraw/blob/main/documentation/CUE%20sheet%20format.asciidoc>
// [LIBODRAW-RAW]: <https://github.com/libyal/libodraw/blob/main/documentation/Optical%20disc%20RAW%20format.asciidoc>

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

fn read_cue_msf(reader: &mut Peekable<Chars>) -> Result<Msf> {
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
enum SectorSourceFormat {
    Raw2352,
    // TODO: support 2048-byte formats
}

impl SectorSourceFormat {
    fn bytes_per_sector(self) -> u64 {
        match self {
            Self::Raw2352 => 2352,
        }
    }
}

#[derive(Debug)]
enum SectorSource {
    /// Sectors filled with zeroes. Used for pregaps and postgaps.
    Zeros,
    /// Sectors sourced from a data file
    DataFile {
        /// Index of file in the `files` array
        file_idx: usize,
        /// Byte offset within the file
        file_offset: u64,
        format: SectorSourceFormat,
    },
}

#[derive(Debug)]
struct SectorMapEntry {
    sector: u32,
    sector_count: u32,
    source: SectorSource,
}

struct SectorMapBuilder {
    /// Absolute sector number
    abs_cursor: u32,
    /// Byte offset within current data file
    file_offset: u64,
    /// Max bytes in current data file
    file_size: u64,
    /// Sector number within current data file
    file_sector: u32,
    /// Index of current data file in the cuesheet's `files` array
    file_idx: usize,
    /// Most recent source format
    last_format: SectorSourceFormat,
    map: Vec<SectorMapEntry>,
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
            file_offset: 0,
            file_size: 0,
            file_sector: 0,
            file_idx: 0,
            last_format: SectorSourceFormat::Raw2352,
            map: vec![],
        }
    }

    fn start_new_file(&mut self, idx: usize, size: u64) -> Result<()> {
        // If a data file exists, add the rest of its sectors before starting the new file
        // TODO: support formats other than Raw2352
        self.add_rest_of_file()?;
        self.file_offset = 0;
        self.file_size = size;
        self.file_sector = 0;
        self.file_idx = idx;

        Ok(())
    }

    /// Add data sectors up to a given sector number within the data file
    fn add_file_up_to(&mut self, up_to: u32, format: SectorSourceFormat) -> Result<()> {
        if up_to < self.file_sector {
            bail!("File sector number cannot decrease");
        }

        let additional_sectors = up_to - self.file_sector;

        if let Some(entry) = self.map.last_mut()
            && let SectorSource::DataFile {
                file_idx,
                format: last_format,
                ..
            } = entry.source
            && file_idx == self.file_idx
            && last_format == format
        {
            // Add more sectors to the existing map entry
            entry.sector_count += additional_sectors;
        } else if additional_sectors > 0 {
            // Start a new map entry
            self.map.push(SectorMapEntry {
                sector: self.abs_cursor,
                sector_count: additional_sectors,
                source: SectorSource::DataFile {
                    file_idx: self.file_idx,
                    file_offset: self.file_offset,
                    format,
                },
            });
        }

        self.file_sector += additional_sectors;
        self.file_offset += additional_sectors as u64 * format.bytes_per_sector();
        self.abs_cursor += additional_sectors;
        self.last_format = format;

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

    fn add_rest_of_file(&mut self) -> Result<()> {
        let remaining_bytes = self.file_size - self.file_offset;
        let remaining_sectors: u32 =
            (remaining_bytes / self.last_format.bytes_per_sector()).try_into()?;
        self.add_file_up_to(self.file_sector + remaining_sectors, self.last_format)
    }

    fn build(mut self) -> Result<Vec<SectorMapEntry>> {
        self.add_rest_of_file()?;
        Ok(self.map)
    }
}

pub struct CuesheetCdromBackend {
    cue_path: PathBuf,
    files: Vec<File>,
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
        let cue_file = BufReader::new(File::open(path)?);

        let mut files: Vec<File> = vec![];
        let mut sector_map = SectorMapBuilder::new();

        let mut track_num = 0u8;
        let mut track_control = DATA_TRACK;
        let mut source_format = SectorSourceFormat::Raw2352;
        let mut next_source_format = SectorSourceFormat::Raw2352;
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
            let mut chars = line.chars().peekable();

            if let Some(command) = read_cue_word(&mut chars) {
                match command.as_str() {
                    "FILE" => {
                        let file_path = read_cue_path(&mut chars)
                            .ok_or_else(|| anyhow!("Failed to parse FILE command"))?;
                        let file_path = cue_dir.join(Path::new(&file_path));

                        let file_type = read_cue_word(&mut chars)
                            .ok_or_else(|| anyhow!("Failed to parse FILE command"))?;
                        // TODO: support WAVE?
                        if file_type != "BINARY" {
                            bail!("Unsupported file type `{}` in cuesheet", file_type);
                        }

                        log::info!("Loading file from {}", file_path.to_string_lossy());

                        let file = File::open(file_path)?;
                        let file_len = file.metadata()?.len();
                        files.push(file);

                        sector_map.start_new_file(files.len() - 1, file_len)?;
                    }
                    "TRACK" => {
                        track_num = read_cue_word(&mut chars)
                            .ok_or_else(|| anyhow!("Invalid TRACK command"))?
                            .parse()?;
                        let track_form_str = read_cue_word(&mut chars)
                            .ok_or_else(|| anyhow!("Invalid TRACK command"))?;
                        (next_source_format, track_control) = match track_form_str.as_str() {
                            "AUDIO" => (SectorSourceFormat::Raw2352, AUDIO_TRACK),
                            "MODE1/2352" => (SectorSourceFormat::Raw2352, DATA_TRACK),
                            "MODE2/2352" => {
                                if !session_has_tracks {
                                    sessions.last_mut().unwrap().disc_type = 0x20; // CD data XA disc with first track in Mode 2
                                }
                                (SectorSourceFormat::Raw2352, DATA_TRACK)
                            }
                            _ => bail!("Unsupported track form {}", track_form_str),
                        };
                    }
                    "INDEX" => {
                        let index_num: u8 = read_cue_word(&mut chars)
                            .ok_or_else(|| anyhow!("Invalid INDEX command"))?
                            .parse()?;

                        // Index sector is relative to the current data file
                        let file_sector = read_cue_msf(&mut chars)?.to_sector();

                        // Add any previous file data up to this point
                        sector_map.add_file_up_to(file_sector, source_format)?;
                        source_format = next_source_format;

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
                        if let Some(rem_cmd) = read_cue_word(&mut chars) {
                            match rem_cmd.as_str() {
                                "LEAD-OUT" => {
                                    if let Ok(leadout_msf) = read_cue_msf(&mut chars) {
                                        sector_map.add_file_up_to(
                                            leadout_msf.to_sector(),
                                            source_format,
                                        )?;

                                        sector_map.add_gap(gap_sectors);
                                        gap_sectors = 0;

                                        sessions.last_mut().unwrap().leadout =
                                            sector_map.abs_cursor;
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
                    }
                    _ => log::warn!("Unknown cuesheet command {} ignored", command),
                }
            }
        }

        log::debug!("Tracks: {:#?}", tracks);

        // In case the final track has a postgap...
        sector_map.add_rest_of_file()?;
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
            files,
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
}

impl CdromBackend for CuesheetCdromBackend {
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

            if raw_sector.control != DATA_TRACK {
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

    fn read_raw_sector(&self, sector: u32) -> Result<RawSector> {
        let map_entry = self
            .find_map_entry_for_sector(sector)
            .ok_or_else(|| anyhow!("Sector {} not found in sector map", sector))?;

        let rel_sector = sector - map_entry.sector;

        let track = get_track_at_sector(&self.tracks, sector)
            .ok_or_else(|| anyhow!("No track found at sector {}", sector))?;

        let data = match map_entry.source {
            SectorSource::Zeros => [0; RAW_SECTOR_LEN],
            SectorSource::DataFile {
                file_idx,
                file_offset,
                format,
            } => {
                // It turns out you don't need a &mut File to seek and read!
                // Just call seek and read on a `&File`. This will clobber the file cursor,
                // so use with caution.
                let mut file: &File = &self.files[file_idx];
                file.seek(SeekFrom::Start(
                    file_offset + rel_sector as u64 * format.bytes_per_sector(),
                ))?;

                match format {
                    SectorSourceFormat::Raw2352 => {
                        let mut result = [0u8; RAW_SECTOR_LEN];
                        file.read_exact(&mut result)?;
                        result
                    }
                }
            }
        };

        Ok(RawSector {
            data,
            control: track.control,
        })
    }
}
