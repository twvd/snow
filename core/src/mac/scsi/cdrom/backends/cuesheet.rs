use anyhow::{Result, anyhow, bail};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    iter::Peekable,
    path::{Path, PathBuf},
    str::Chars,
};

use crate::mac::scsi::cdrom::{
    AUDIO_TRACK, CdromBackend, DATA_TRACK, LBA_START_SECTOR, Msf, RAW_SECTOR_LEN, SessionInfo,
    TrackInfo,
};

// .cue file reference:
//
// [LIBODRAW]: <https://github.com/libyal/libodraw/blob/main/documentation/CUE%20sheet%20format.asciidoc>

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

struct CuesheetDataFile {
    /// Number of sectors in data file (not counting pregaps and postgaps)
    #[allow(unused)]
    sector_count: u32,
    file: File,
}

#[derive(Debug)]
enum SectorSource {
    /// Sectors filled with zeroes. Used for pregaps and postgaps.
    Zeros,
    /// Sectors sourced from a data file
    DataFile {
        /// Index of file in the data_files array
        data_file_idx: usize,
        /// Sector number relative to the beginning of the file
        data_file_sector: u32,
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
    /// Sector number within current data file
    data_file_cursor: u32,
    /// Max sectors in current data file
    data_file_max: u32,
    /// Index of current data file in the cuesheet's `data_files` array
    data_file_idx: usize,
    map: Vec<SectorMapEntry>,
}

impl SectorMapBuilder {
    fn new() -> Self {
        // Data files always start at time 00:02:00 (sector 150).
        // This means there are 150 sectors located in the lead-in area which
        // cannot be specified by the .bin/.cue format.
        // These sectors are usually empty, but sometimes they contain various
        // data such as CD-TEXT information.
        SectorMapBuilder {
            abs_cursor: LBA_START_SECTOR,
            data_file_cursor: 0,
            data_file_max: 0,
            data_file_idx: 0,
            map: vec![],
        }
    }

    fn start_new_data_file(&mut self, idx: usize, max: u32) -> Result<()> {
        // If a data file exists, add the rest of its sectors before starting the new file
        self.add_rest_of_data_file()?;
        self.data_file_cursor = 0;
        self.data_file_idx = idx;
        self.data_file_max = max;

        Ok(())
    }

    /// Add data sectors up to a given sector number within the data file
    fn add_data_up_to(&mut self, up_to: u32) -> Result<()> {
        if up_to < self.data_file_cursor {
            bail!("Data file sector number cannot decrease");
        }

        if up_to > self.data_file_max {
            bail!("Data file sector number exceeded max");
        }

        let additional = up_to - self.data_file_cursor;

        if let Some(entry) = self.map.last_mut()
            && let SectorSource::DataFile { data_file_idx, .. } = entry.source
            && data_file_idx == self.data_file_idx
        {
            // Add more sectors to the existing map entry
            entry.sector_count += additional;
        } else if additional > 0 {
            // Start a new map entry
            self.map.push(SectorMapEntry {
                sector: self.abs_cursor,
                sector_count: additional,
                source: SectorSource::DataFile {
                    data_file_idx: self.data_file_idx,
                    data_file_sector: self.data_file_cursor,
                },
            });
        }

        self.data_file_cursor += additional;
        self.abs_cursor += additional;

        Ok(())
    }

    /// Add sectors of silence (all zeroes)
    fn add_gap(&mut self, sectors: u32) {
        if let Some(entry) = self.map.last_mut()
            && let SectorSource::Zeros = entry.source
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

    fn add_rest_of_data_file(&mut self) -> Result<()> {
        self.add_data_up_to(self.data_file_max)
    }

    fn build(mut self) -> Result<Vec<SectorMapEntry>> {
        self.add_rest_of_data_file()?;
        Ok(self.map)
    }
}

pub struct CuesheetCdromBackend {
    cue_path: PathBuf,
    data_files: Vec<CuesheetDataFile>,
    sessions: Vec<SessionInfo>,
    /// Map of sectors. Entries are sorted in order of increasing `sector` field.
    /// This table maps absolute sector numbers to data files. This is NOT the
    /// table of contents; it doesn't carry information about tracks.
    sector_map: Vec<SectorMapEntry>,
}

enum CuesheetTrackForm {
    Audio,
    Mode1_2352,
}

impl CuesheetCdromBackend {
    pub fn new(path: &Path) -> Result<Self> {
        let cue_dir = path.parent().unwrap();
        let cue_file = BufReader::new(File::open(path)?);

        let mut data_files: Vec<CuesheetDataFile> = vec![];
        let mut sector_map = SectorMapBuilder::new();

        let mut track_num = 0u8;
        let mut track_form = CuesheetTrackForm::Audio;
        let mut gap_sectors = 0;

        let mut tracks = vec![];

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

                        let file_type = read_cue_word(&mut chars)
                            .ok_or_else(|| anyhow!("Failed to parse FILE command"))?;
                        // TODO: support WAVE?
                        if file_type != "BINARY" {
                            bail!("Unsupported data file type `{}` in cuesheet", file_type);
                        }

                        log::info!("Loading datafile from {}", data_file_path.to_string_lossy());

                        let data_file = File::open(data_file_path)?;
                        let data_file_len = data_file.metadata()?.len();
                        let sector_count =
                            data_file_len.div_ceil(RAW_SECTOR_LEN as u64).try_into()?;
                        data_files.push(CuesheetDataFile {
                            sector_count,
                            file: data_file,
                        });

                        sector_map.start_new_data_file(data_files.len() - 1, sector_count)?;
                    }
                    "TRACK" => {
                        track_num = read_cue_word(&mut chars)
                            .ok_or_else(|| anyhow!("Invalid TRACK command"))?
                            .parse()?;
                        track_form = match read_cue_word(&mut chars)
                            .ok_or_else(|| anyhow!("Invalid TRACK command"))?
                            .as_str()
                        {
                            "AUDIO" => CuesheetTrackForm::Audio,
                            "MODE1/2352" => CuesheetTrackForm::Mode1_2352,
                            _ => bail!("Unsupported track form"),
                        };
                    }
                    "INDEX" => {
                        let index_num: u8 = read_cue_word(&mut chars)
                            .ok_or_else(|| anyhow!("Invalid INDEX command"))?
                            .parse()?;

                        // Index sector is relative to the current data file.
                        let data_file_sector = read_cue_msf(&mut chars)?.to_sector();
                        sector_map.add_data_up_to(data_file_sector)?;

                        // Add pregaps/postgaps here
                        sector_map.add_gap(gap_sectors);
                        gap_sectors = 0;

                        if index_num == 1 {
                            // The track will officially begin at index 1.
                            tracks.push(TrackInfo {
                                tno: track_num,
                                control: match track_form {
                                    CuesheetTrackForm::Audio => AUDIO_TRACK,
                                    CuesheetTrackForm::Mode1_2352 => DATA_TRACK,
                                },
                                sector: sector_map.abs_cursor,
                            });
                        }
                    }
                    "PREGAP" | "POSTGAP" => {
                        let duration = read_cue_msf(&mut chars)?.to_sector();
                        gap_sectors += duration;
                    }
                    // TODO: Support multisession bin/cue's. IsoBuster emits REM SESSION commands to indicate a new session.
                    _ => log::warn!("Unknown cuesheet command {} ignored", command),
                }
            }
        }

        log::debug!("Read tracks from cuesheet: {:#?}", tracks);

        // In case the final track has a postgap...
        sector_map.add_rest_of_data_file()?;
        sector_map.add_gap(gap_sectors);

        let sector_map = sector_map.build()?;
        log::debug!("Sector map: {:#?}", sector_map);

        let final_leadout = sector_map
            .last()
            .map(|e| e.sector + e.sector_count)
            .unwrap_or(LBA_START_SECTOR);

        let self_ = Self {
            cue_path: path.into(),
            data_files,
            sessions: vec![SessionInfo {
                leadout: final_leadout,
                tracks,
            }],
            sector_map,
        };

        let test_sector = 20934;
        let test = self_.find_map_entry_for_sector(test_sector);
        log::debug!("map entry for sector {}: {:#?}", test_sector, test);
        log::debug!("sector {} is at rel sector {}", test_sector, test_sector - test.unwrap().sector);
        let test_source = match test.unwrap().source {
            SectorSource::DataFile{ data_file_idx: _, data_file_sector } => data_file_sector,
            _ => unreachable!()
        };
        let file_sector = test_source + test_sector - test.unwrap().sector;
        log::debug!("sector {} is at file sector {} (file offset 0x{:X})", test_sector, file_sector, file_sector * 2352);

        Ok(self_)
    }

    fn find_map_entry_for_sector(&self, sector: u32) -> Option<&SectorMapEntry> {
        // TODO: use partition_point?
        let idx = self.sector_map.iter().rposition(|entry| sector >= entry.sector)?;
        self.sector_map.get(idx)
    }
}

impl CdromBackend for CuesheetCdromBackend {
    fn byte_len(&self) -> usize {
        // FIXME: What's the correct value here? Let's just say 333,000 * 2048-byte sectors.
        333_000 * 2048
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>> {
        let mut result = Vec::<u8>::with_capacity(length);

        // TODO: uh-oh, do we need to support CD-ROM's where the data is in session 2?
        // Example: "Weird Al" Yankovic - Running With Scissors
        // (the Weird Al disc also uses Form 2 sectors in its data track!)
        let rel_sector: u32 = (offset / 2048).try_into().unwrap();
        // FIXME: Does the drive automatically find the data track?
        let mut sector = LBA_START_SECTOR + rel_sector;
        while result.len() < length {
            let raw_sector = self.read_raw_sector(sector)?;
            sector += 1;
            // TODO: Check sync, mode and error detection data?
            let sector_data = &raw_sector[16..][..2048];
            result.extend_from_slice(sector_data);
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

    fn read_raw_sector(&self, sector: u32) -> Result<[u8; RAW_SECTOR_LEN]> {
        let map_entry = self
            .find_map_entry_for_sector(sector)
            .ok_or_else(|| anyhow!("Sector {} not found in sector map", sector))?;
        if !(map_entry.sector..map_entry.sector + map_entry.sector_count).contains(&sector) {
            bail!("Sector {} not found in sector map", sector);
        }

        let rel_sector = sector - map_entry.sector;

        let result = match map_entry.source {
            SectorSource::Zeros => [0; RAW_SECTOR_LEN],
            SectorSource::DataFile {
                data_file_idx,
                data_file_sector,
            } => {
                let data_file = &self.data_files[data_file_idx];
                let sector_in_file = data_file_sector + rel_sector;
                // It turns out you don't need a &mut File to seek and read!
                // Just call seek and read on a `&File`. This will clobber the file cursor,
                // so use with caution.
                let mut data_file: &File = &data_file.file;
                data_file.seek(SeekFrom::Start(
                    sector_in_file as u64 * RAW_SECTOR_LEN as u64,
                ))?;
                let mut result = [0; RAW_SECTOR_LEN];
                data_file.read_exact(&mut result)?;
                result
            }
        };

        Ok(result)
    }
}
