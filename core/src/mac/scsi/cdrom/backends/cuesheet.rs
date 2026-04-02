use anyhow::{anyhow, bail, Result};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    iter::Peekable,
    path::{Path, PathBuf},
    str::Chars,
};

use crate::mac::scsi::cdrom::{
    CdromBackend, Msf, SessionInfo, TrackInfo, AUDIO_TRACK, DATA_TRACK, RAW_SECTOR_LEN,
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
    /// Absolute sector number where the data file resides on the CD
    sector: u32,
    sector_count: u32,
    file: File,
}

pub struct CuesheetCdromBackend {
    cue_path: PathBuf,
    data_files: Vec<CuesheetDataFile>,
    sessions: Vec<SessionInfo>,
}

enum CuesheetTrackForm {
    Audio,
    Mode1_2352,
}

impl CuesheetCdromBackend {
    pub fn new(path: &Path) -> Result<Self> {
        let cue_dir = path.parent().unwrap();
        let cue_file = BufReader::new(File::open(path)?);

        let mut data_files = vec![];
        // Data files always start at time 00:02:00 (sector 150).
        // This means there are 150 sectors located in the lead-in area which
        // cannot be specified by the .bin/.cue format.
        // These sectors are usually empty, but sometimes they contain various
        // data such as CD-TEXT information.
        let mut next_data_file_sector = Msf::new(0, 2, 0).to_sector();

        let mut track_num = 0u8;
        let mut track_form = CuesheetTrackForm::Audio;

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
                            sector: next_data_file_sector,
                            sector_count,
                            file: data_file,
                        });
                        next_data_file_sector += sector_count;
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
                        if index_num == 1 {
                            // The track will officially begin at index 1.
                            // Index sector is relative to the current data file.
                            let rel_sector = read_cue_msf(&mut chars)?.to_sector();
                            let curr_data_file = data_files
                                .last()
                                .ok_or_else(|| anyhow!("No data file loaded for INDEX command"))?;
                            tracks.push(TrackInfo {
                                tno: track_num,
                                control: match track_form {
                                    CuesheetTrackForm::Audio => AUDIO_TRACK,
                                    CuesheetTrackForm::Mode1_2352 => DATA_TRACK,
                                },
                                sector: curr_data_file.sector + rel_sector,
                            })
                        } else {
                            log::warn!("track {} INDEX {} ignored", track_num, index_num);
                        }
                    }
                    // TODO: Support multisession bin/cue's. IsoBuster emits REM SESSION commands to indicate a new session.
                    _ => log::warn!("Unknown cuesheet command {} ignored", command),
                }
            }
        }

        log::info!("Read tracks from cuesheet: {:#?}", tracks);

        let final_leadout = data_files
            .last()
            .map(|df| df.sector + df.sector_count)
            .unwrap_or(0);

        Ok(Self {
            cue_path: path.into(),
            data_files,
            sessions: vec![SessionInfo {
                leadout: Msf::from_sector(final_leadout)?,
                tracks,
            }],
        })
    }

    fn find_data_file_for_sector(&self, sector: u32) -> Option<&CuesheetDataFile> {
        self.data_files.iter().rev().find(|df| df.sector <= sector)
    }
}

impl CdromBackend for CuesheetCdromBackend {
    fn byte_len(&self) -> usize {
        // FIXME: What's the correct value here? Let's just say 333,000 * 2048-byte sectors.
        333_000 * 2048
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8> {
        log::info!("Reading {} bytes from offset 0x{:X}", length, offset);
        let mut result = Vec::<u8>::with_capacity(length);

        // TODO: uh-oh, do we need to support CD-ROM's where the data is in session 2?
        // Example: "Weird Al" Yankovic - Running With Scissors
        // (the Weird Al disc also uses Form 2 sectors in its data track!)
        const START_SECTOR: u32 = Msf::new(0, 2, 0).to_sector();
        let rel_sector: u32 = (offset / 2048).try_into().unwrap();
        // FIXME: Does the drive automatically find the data track?
        let mut sector = START_SECTOR + rel_sector;
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

    fn sessions(&self) -> Option<&[SessionInfo]> {
        Some(&self.sessions)
    }

    fn read_raw_sector(&self, sector: u32) -> Result<[u8; RAW_SECTOR_LEN]> {
        let mut result = [0; RAW_SECTOR_LEN];
        let data_file = self
            .find_data_file_for_sector(sector)
            .ok_or_else(|| anyhow!("No data file contains sector {}", sector))?;
        let rel_sector = sector - data_file.sector;
        // It turns out you don't need a &mut File to seek and read!
        // Just call seek and read on a `&File`. This will clobber the file cursor,
        // so use with caution.
        let mut data_file: &File = &data_file.file;
        data_file.seek(SeekFrom::Start(rel_sector as u64 * RAW_SECTOR_LEN as u64))?;
        data_file.read_exact(&mut result)?;
        Ok(result)
    }
}
