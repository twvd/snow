//! Simulation of the Lisa's floppy controller: the 6504 with its ROM, and its drives
//!
//! Spec: docs/lisa/lisa-spec-fdc.md. It is seen through its shared RAM and two lines only,
//! so that a 6504 running the ROM can replace it. The Lisa 1's ROM ($40) drives two Twiggy
//! drives and keeps some of its state elsewhere in the RAM than the later ROMs, which drive
//! one Sony.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::pfg::EEPROM_SIZE;
use crate::emulator::devices::{
    BusKind, DeviceModel, Floppy, FloppySettings, FloppyStatus, FunctionKind, ModelInfo,
    replace_status,
};
use crate::mac::MacModel;
use crate::tickable::Ticks;

const RAM_SIZE: usize = 1024;

/// RAM locations
const GOBYTE: usize = 0x00;
const COMMAND: usize = 0x01;
const ADRH: usize = 0x02;
const DRIVE: usize = 0x02;
const SIDE: usize = 0x03;
const SECTOR: usize = 0x04;
const TRACK: usize = 0x05;
const SPEED: usize = 0x06;
const CONFIRM: usize = 0x07;
const ERRSTAT: usize = 0x08;
const DISKID: usize = 0x09;
const NOSIDES: usize = 0x0A;
const CONSTANTS: usize = 0x10;
const CLAMPED: usize = 0x20;
const MTRON: usize = 0x21;
const CURTRACK: usize = 0x22;
const DRVCONN: usize = 0x24;
const FMTTYPE: usize = 0x25;
const FMTTYPE_SIDE_BIT: u8 = 0x20;
const IMALIVE: usize = 0x28;
const IMSK: usize = 0x2C;
const OKTOGO: usize = 0x2E;
const IST: usize = 0x2F;
const PARAMETER_MEMORY: usize = 0xC0;
pub(super) const PARAMETER_MEMORY_SIZE: usize = 0x40;
const TAG: usize = 0x1F4;
const DATA: usize = 0x200;

/// Where the ROM id is in the constants
const CONSTANT_ROM_ID: usize = 8;
/// ROM id of the 2/5. The ROM of the 2/10 only differs by this.
pub const ROM_ID_2_5: u8 = 0xA8;
pub const ROM_ID_2_10: u8 = 0x88;
/// The Lisa 1's ROM, known as $40, puts $52 there: bit 7 clear says Lisa 1 to the boot ROM
pub const ROM_ID_LISA_1: u8 = 0x52;

/// Default constants, from the ROM of the 2/5. The one at $18 is the ROM id.
const CONSTANTS_2_5: [u8; 13] = [
    0xD5, 0xC0, 0xA7, 0x89, 0x64, 0x1E, 0x04, 0x09, 0xA8, 0x64, 0x02, 0x82, 0x4F,
];

/// What the Lisa 1's ROM copies to $10 at a cold start, from $1111
const CONSTANTS_LISA_1: [u8; 14] = [
    0x00, 0x04, 0x00, 0xA0, 0x50, 0x1E, 0x02, 0x08, 0x52, 0x64, 0x01, 0x09, 0x48, 0x5A,
];
/// Where the Lisa 1's ROM keeps what the later ROMs keep elsewhere
const CLAMPED_LISA_1: [usize; 2] = [0x25, 0x27];
const IMALIVE_LISA_1: usize = 0x2E;
/// IST & IMSK, which raises FDIR while nonzero, instead of OKTOGO
const FDIR_LISA_1: usize = 0x43;
const IMSK_LISA_1: usize = 0x44;
/// The Lisa 1's ROM clears the RAM up to here at a cold start
const CLEARED_LISA_1: usize = 0xA1;

/// What the Lisa 1's ROM checks before a command, at $1100: bits for the drive, side,
/// sector, track, mask, format confirmation and write protect, checked in that order
const CHECK_DRIVE: u8 = 0x01;
const CHECK_SIDE: u8 = 0x02;
const CHECK_SECTOR: u8 = 0x04;
const CHECK_TRACK: u8 = 0x08;
const CHECK_MASK: u8 = 0x10;
const CHECK_CONFIRM: u8 = 0x20;
const CHECK_WRITE_PROTECT: u8 = 0x40;
/// For each RWTS command
const CHECKS_LISA_1: [u8; 10] = [0x0F, 0x4F, 0x01, 0x6B, 0x0B, 0x6B, 0x0B, 0x0F, 0x4F, 0x00];
const CHECKS_SEEK_LISA_1: u8 = 0x0B;
const CHECKS_SET_MASK_LISA_1: u8 = 0x10;

const DRIVE_UPPER: u8 = 0x00;
const DRIVE_LOWER: u8 = 0x80;

const GO_NULL: u8 = 0x80;
const GO_RWTS: u8 = 0x81;
const GO_SEEK: u8 = 0x83;
const GO_CALL: u8 = 0x84;
const GO_CLEAR_IST: u8 = 0x85;
const GO_SET_MASK: u8 = 0x86;
const GO_CLEAR_MASK: u8 = 0x87;
const GO_WAIT_IN_ROM: u8 = 0x88;
const GO_AWAY: u8 = 0x89;

const RWTS_READ: u8 = 0;
const RWTS_WRITE: u8 = 1;
const RWTS_UNCLAMP: u8 = 2;
const RWTS_FORMAT: u8 = 3;
const RWTS_VERIFY: u8 = 4;
const RWTS_FORMAT_TRACK: u8 = 5;
const RWTS_VERIFY_TRACK: u8 = 6;
const RWTS_READ_BRUTE: u8 = 7;
const RWTS_WRITE_BRUTE: u8 = 8;
const RWTS_CLAMP: u8 = 9;

const ERR_OK: u8 = 0x00;
const ERR_BAD_GOBYTE: u8 = 0x01;
const ERR_BAD_DRIVE: u8 = 0x02;
const ERR_BAD_SIDE: u8 = 0x03;
const ERR_BAD_SECTOR: u8 = 0x04;
const ERR_BAD_TRACK: u8 = 0x05;
const ERR_BAD_MASK: u8 = 0x06;
const ERR_NO_DISK: u8 = 0x07;
const ERR_NOT_ENABLED: u8 = 0x08;
const ERR_INTERRUPT_PENDING: u8 = 0x09;
const ERR_BAD_FORMAT: u8 = 0x0A;
const ERR_WRITE_PROTECT: u8 = 0x14;
const ERR_READ: u8 = 0x17;

/// Interrupt status bits of the lower drive, and of the upper one 4 below
const IST_INSERTED: u8 = 0x10;
const IST_BUTTON: u8 = 0x20;
const IST_DONE: u8 = 0x40;
const IST_ANY: u8 = 0x80;
const MASK_LOWER: u8 = 0x80;
const MASK_UPPER: u8 = 0x08;

/// The controller's main loop goes around every 10 us
const COMMAND_TICKS: Ticks = 51;

/// 6504 code MacWorks Plus II runs from RAM, to time the 6504 against the CPU:
/// a loop, then $FF in the gobyte, then waiting for the host to clear it.
const TIMING_LOOP_CODE: [u8; 15] = [
    0xA0, 0x00, 0x88, 0xC8, 0x88, 0xD0, 0xFB, 0x88, 0x84, 0x00, 0xA5, 0x00, 0xD0, 0xFC, 0x60,
];
const TIMING_LOOP_AT: usize = 0x200;
/// Its 2315 cycles at DOTCK/10, with the CPU at DOTCK/4
const TIMING_LOOP_TICKS: Ticks = 5788;
/// The 6504's clock, DOTCK/10
pub const FDC_CLOCK: Ticks = 2_037_504;
/// From power on to listening
const START_TICKS: Ticks = 6_367_200;
const OPERATION_TICKS: Ticks = 101_875;
const STEP_TICKS: Ticks = 25_469;

const TRACKS: u8 = 80;
const SECTOR_SIZE: usize = 512;
const TAG_SIZE: usize = 12;

const DC42_HEADER_SIZE: usize = 0x54;
const DC42_DATA_SIZE: usize = 0x40;
const DC42_TAG_SIZE: usize = 0x44;
const DC42_DATA_CHECKSUM: usize = 0x48;
const DC42_TAG_CHECKSUM: usize = 0x4C;
/// $0100 in a DiskCopy 4.2 header
const DC42_MAGIC: usize = 0x52;
/// DiskCopy leaves the tags of the first sector out of its checksum
const DC42_TAG_CHECKSUM_FROM: usize = 12;

/// Sectors on a track
fn sectors_on_track(track: u8) -> u8 {
    12 - track / 16
}

/// Sectors before a track on a side
fn sectors_before_track(track: u8) -> usize {
    (0..track).map(|t| usize::from(sectors_on_track(t))).sum()
}

const TWIGGY_TRACKS: u8 = 46;
/// The first track of each zone and its sectors, from the Lisa 1's ROM at $114E and $1129
const TWIGGY_ZONES: [(u8, u8); 8] = [
    (0, 22),
    (4, 21),
    (11, 20),
    (17, 19),
    (23, 18),
    (29, 17),
    (35, 16),
    (42, 15),
];

/// Sectors on a track of a Twiggy disk. Past the last track, those of the last zone.
fn twiggy_sectors_on_track(track: u8) -> u8 {
    TWIGGY_ZONES
        .iter()
        .rev()
        .find(|(first, _)| track >= *first)
        .map_or(0, |(_, sectors)| *sectors)
}

fn twiggy_sectors_before_track(track: u8) -> usize {
    (0..track)
        .map(|t| usize::from(twiggy_sectors_on_track(t)))
        .sum()
}

/// A floppy image: 512-byte sectors, and a 12-byte tag for each
#[derive(Serialize, Deserialize)]
pub struct LisaDisk {
    data: Vec<u8>,
    tags: Vec<u8>,
    write_protect: bool,
    sides: u8,
    /// A Lisa 1 disk
    twiggy: bool,
    /// DiskCopy 4.2 header, none for a raw image
    dc42_header: Option<Vec<u8>>,
    /// Bytes after the tags, kept for writing back: BLU pads its images with $1A
    trailer: Vec<u8>,
    /// Written since the image was loaded
    dirty: bool,
    /// Name to show
    pub title: String,
    /// The file it was loaded from
    pub media: Option<PathBuf>,
    /// Written back to that file when it leaves the drive
    pub writeback: bool,
}

/// DiskCopy 4.2 checksum: add each big endian word, rotate right
fn dc42_checksum(bytes: &[u8]) -> u32 {
    bytes.chunks_exact(2).fold(0, |sum: u32, word| {
        sum.wrapping_add(u32::from(u16::from_be_bytes([word[0], word[1]])))
            .rotate_right(1)
    })
}

impl LisaDisk {
    /// Parses a DiskCopy 4.2 image, or else raw sectors
    pub fn from_image(image: &[u8], title: &str, write_protect: bool) -> Result<Self> {
        let be32 = |at: usize| {
            image
                .get(at..at + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
        };
        let mut dc42_header = None;
        let mut trailer = &image[..0];
        let padded = image.get(DC42_MAGIC..DC42_MAGIC + 2) == Some(&[0x01, 0x00]);
        let (data, tags) = match (be32(DC42_DATA_SIZE), be32(DC42_TAG_SIZE)) {
            (Some(data_size), Some(tag_size))
                if data_size > 0
                    && data_size % SECTOR_SIZE == 0
                    && (DC42_HEADER_SIZE + data_size + tag_size == image.len()
                        || (padded && DC42_HEADER_SIZE + data_size + tag_size < image.len())) =>
            {
                dc42_header = Some(image[..DC42_HEADER_SIZE].to_vec());
                let tags_at = DC42_HEADER_SIZE + data_size;
                let end = tags_at + tag_size;
                trailer = &image[end..];
                (&image[DC42_HEADER_SIZE..tags_at], &image[tags_at..end])
            }
            _ if !image.is_empty() && image.len().is_multiple_of(SECTOR_SIZE) => {
                (image, &image[..0])
            }
            _ => bail!("Not a DiskCopy 4.2 or raw image"),
        };
        let sectors = data.len() / SECTOR_SIZE;
        let (sides, twiggy) = if sectors == sectors_before_track(TRACKS) {
            (1, false)
        } else if sectors == 2 * sectors_before_track(TRACKS) {
            (2, false)
        } else if sectors == 2 * twiggy_sectors_before_track(TWIGGY_TRACKS) {
            (2, true)
        } else {
            bail!(
                "A Lisa disk has 800, 1600 or 1702 sectors, this image has {}",
                sectors
            );
        };
        let mut tags = tags.to_vec();
        tags.resize(sectors * TAG_SIZE, 0);
        Ok(Self {
            data: data.to_vec(),
            tags,
            write_protect,
            sides,
            twiggy,
            dc42_header,
            trailer: trailer.to_vec(),
            dirty: false,
            title: title.to_owned(),
            media: None,
            writeback: false,
        })
    }

    pub fn sides(&self) -> u8 {
        self.sides
    }

    pub fn twiggy(&self) -> bool {
        self.twiggy
    }

    fn tracks(&self) -> u8 {
        if self.twiggy { TWIGGY_TRACKS } else { TRACKS }
    }

    fn sectors_on(&self, track: u8) -> u8 {
        if self.twiggy {
            twiggy_sectors_on_track(track)
        } else {
            sectors_on_track(track)
        }
    }

    /// The first sector of a track and side. On a Sony disk, side 1 of a track follows
    /// side 0. On a Twiggy disk, side 1's tracks follow all of side 0's.
    fn first_sector(&self, track: u8, side: u8) -> usize {
        if self.twiggy {
            return usize::from(side) * twiggy_sectors_before_track(TWIGGY_TRACKS)
                + twiggy_sectors_before_track(track);
        }
        sectors_before_track(track) * usize::from(self.sides)
            + usize::from(side) * usize::from(sectors_on_track(track))
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// The image file for the disk as it is now, in the format it was loaded from
    pub fn to_image(&self) -> Vec<u8> {
        let Some(header) = &self.dc42_header else {
            return self.data.clone();
        };
        let tag_size = u32::from_be_bytes([
            header[DC42_TAG_SIZE],
            header[DC42_TAG_SIZE + 1],
            header[DC42_TAG_SIZE + 2],
            header[DC42_TAG_SIZE + 3],
        ]) as usize;
        let tags = &self.tags[..tag_size];
        let tag_checksum = tags.get(DC42_TAG_CHECKSUM_FROM..).map_or(0, dc42_checksum);

        let mut image = header.clone();
        image[DC42_DATA_CHECKSUM..DC42_DATA_CHECKSUM + 4]
            .copy_from_slice(&dc42_checksum(&self.data).to_be_bytes());
        image[DC42_TAG_CHECKSUM..DC42_TAG_CHECKSUM + 4]
            .copy_from_slice(&tag_checksum.to_be_bytes());
        image.extend_from_slice(&self.data);
        image.extend_from_slice(tags);
        image.extend_from_slice(&self.trailer);
        image
    }
}

/// An operation in progress
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Operation {
    done_at: Ticks,
    /// Drive it interrupts for
    drive: u8,
    error: u8,
}

/// A drive on the controller
#[derive(Default, Serialize, Deserialize)]
struct Drive {
    disk: Option<LisaDisk>,
    /// Where the head is, for the time a seek takes
    head: u8,
    /// Lisa 1: "inserted" was raised for the disk in the drive
    insert_signalled: bool,
    /// Lisa 1: the eject button was pressed, and the controller hasn't looked yet
    button: bool,
}

#[derive(Serialize, Deserialize)]
pub struct FdcSim {
    rom_id: u8,
    /// An 800K drive instead of a 400K one
    double_sided: bool,
    ram: Vec<u8>,
    now: Ticks,
    /// When the controller is listening after a reset
    ready_at: Ticks,
    /// When a command is taken
    command_at: Option<Ticks>,
    operation: Option<Operation>,
    /// Commands $88 and $89: parked in the ROM
    parked: bool,
    /// When the timing loop is over
    timing_loop_done_at: Option<Ticks>,
    /// The clock a PFG gives the 6504, instead of its own
    clock: Option<Ticks>,
    /// The lower drive, and on a Lisa 1 the upper drive first
    drives: Vec<Drive>,
    /// Disks out of their drive, for the emulator to write back
    ejected: Vec<LisaDisk>,
    /// File the parameter memory is kept in. A save state doesn't bring it back, like
    /// the PRAM file of the Macs.
    #[serde(skip)]
    parameter_memory_path: Option<PathBuf>,
    #[serde(skip)]
    parameter_memory_changed: bool,
}

impl FdcSim {
    pub fn new(rom_id: u8, double_sided: bool) -> Self {
        let mut fdc = Self {
            rom_id,
            double_sided,
            // Parameter memory as with a run down battery. All zeroes would pass its
            // checksum and select the hard disk to boot from.
            ram: vec![0xFF; RAM_SIZE],
            now: 0,
            ready_at: 0,
            command_at: None,
            operation: None,
            parked: false,
            timing_loop_done_at: None,
            clock: None,
            drives: (0..if rom_id == ROM_ID_LISA_1 { 2 } else { 1 })
                .map(|_| Drive::default())
                .collect(),
            ejected: vec![],
            parameter_memory_path: None,
            parameter_memory_changed: false,
        };
        fdc.reset();
        fdc
    }

    /// Cold start of the controller. Only parameter memory stays.
    pub fn reset(&mut self) {
        if self.lisa_1() {
            self.ram[..=CLEARED_LISA_1].fill(0);
            self.ram[CONSTANTS..CONSTANTS + CONSTANTS_LISA_1.len()]
                .copy_from_slice(&CONSTANTS_LISA_1);
            // It clamps the disks in place again once it runs
            for drive in &mut self.drives {
                drive.insert_signalled = false;
            }
            self.restart();
            return;
        }
        let range = PARAMETER_MEMORY..PARAMETER_MEMORY + PARAMETER_MEMORY_SIZE;
        let parameter_memory = self.ram[range.clone()].to_vec();
        self.ram.fill(0);
        self.ram[range].copy_from_slice(&parameter_memory);
        self.ram[CONSTANTS..CONSTANTS + CONSTANTS_2_5.len()].copy_from_slice(&CONSTANTS_2_5);
        self.ram[CONSTANTS + CONSTANT_ROM_ID] = self.rom_id;
        self.ram[NOSIDES] = if self.double_sided { 2 } else { 1 };
        self.ram[DRVCONN] = 0xFF;
        self.ram[FMTTYPE] = if self.double_sided {
            FMTTYPE_SIDE_BIT | 0x02
        } else {
            0x02
        };
        self.ram[DISKID] = 0x02;
        self.ram[CLAMPED] = if self.drives[0].disk.is_some() {
            0xFF
        } else {
            0
        };
        self.restart();
    }

    fn restart(&mut self) {
        self.ready_at = self.now + START_TICKS;
        self.command_at = None;
        self.operation = None;
        self.parked = false;
    }

    /// The Lisa 1's controller, with two Twiggy drives
    pub fn lisa_1(&self) -> bool {
        self.rom_id == ROM_ID_LISA_1
    }

    fn imsk(&self) -> usize {
        if self.lisa_1() { IMSK_LISA_1 } else { IMSK }
    }

    fn imalive(&self) -> usize {
        if self.lisa_1() {
            IMALIVE_LISA_1
        } else {
            IMALIVE
        }
    }

    /// Which of `drives` a DRIVE byte names
    fn drive_index(&self, drive: u8) -> Option<usize> {
        match (self.lisa_1(), drive) {
            (true, DRIVE_UPPER) => Some(0),
            (true, DRIVE_LOWER) => Some(1),
            (false, DRIVE_LOWER) => Some(0),
            _ => None,
        }
    }

    /// The DRIVE byte of one of `drives`
    fn drive_byte(&self, index: usize) -> u8 {
        if self.lisa_1() && index == 0 {
            DRIVE_UPPER
        } else {
            DRIVE_LOWER
        }
    }

    fn clamped_at(&self, index: usize) -> usize {
        if self.lisa_1() {
            CLAMPED_LISA_1[index]
        } else {
            CLAMPED
        }
    }

    fn enabled(&self, drive: u8) -> bool {
        let mask = if drive == DRIVE_LOWER {
            MASK_LOWER
        } else {
            MASK_UPPER
        };
        self.ram[self.imsk()] & mask != 0
    }

    /// Keeps parameter memory in a file from now on. A file that doesn't exist or is
    /// empty is a run down battery. The EEPROM of a PFG may follow it. A file of another
    /// size is refused and left as it is: it is likely the PRAM file of a Mac.
    pub fn persist_parameter_memory(&mut self, path: &Path) -> Result<()> {
        let saved = match std::fs::read(path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        match saved.len() {
            0 => self.parameter_memory_changed = true,
            n if n == PARAMETER_MEMORY_SIZE || n == PARAMETER_MEMORY_SIZE + EEPROM_SIZE => {
                self.ram[PARAMETER_MEMORY..PARAMETER_MEMORY + PARAMETER_MEMORY_SIZE]
                    .copy_from_slice(&saved[..PARAMETER_MEMORY_SIZE]);
            }
            n => bail!("{} bytes, not {}", n, PARAMETER_MEMORY_SIZE),
        }
        self.parameter_memory_path = Some(path.to_path_buf());
        Ok(())
    }

    /// Writes parameter memory to its file if the guest changed it
    pub fn save_parameter_memory(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.parameter_memory_changed) {
            return Ok(());
        }
        if let Some(path) = &self.parameter_memory_path {
            // Without truncating: the EEPROM of a PFG may follow
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            file.write_all(&self.ram[PARAMETER_MEMORY..PARAMETER_MEMORY + PARAMETER_MEMORY_SIZE])?;
        }
        Ok(())
    }

    /// RAM at the 68000 side
    pub fn ram_read(&self, n: usize) -> u8 {
        self.ram[n & (RAM_SIZE - 1)]
    }

    pub fn ram_write(&mut self, n: usize, val: u8) {
        let n = n & (RAM_SIZE - 1);
        if (PARAMETER_MEMORY..PARAMETER_MEMORY + PARAMETER_MEMORY_SIZE).contains(&n)
            && self.ram[n] != val
        {
            self.parameter_memory_changed = true;
        }
        self.ram[n] = val;
        if n == GOBYTE {
            if self.parked {
                // Waiting in the ROM for the bytes that restart it
                if val == 0x96 && self.ram[COMMAND] == 0x69 {
                    self.reset();
                }
            } else if val & 0x80 != 0 {
                self.command_at = Some(self.now + COMMAND_TICKS);
            }
        }
    }

    /// FDIR
    pub fn fdir(&self) -> bool {
        let ist = self.ram[IST];
        let mask = self.ram[self.imsk()];
        (mask & MASK_LOWER != 0 && ist & 0xF0 != 0) || (mask & MASK_UPPER != 0 && ist & 0x0F != 0)
    }

    /// DSKDIAG: listening
    pub fn diag(&self) -> bool {
        self.now >= self.ready_at
    }

    /// Whether the drives can take a disk
    pub fn fits(&self, disk: &LisaDisk) -> bool {
        if self.lisa_1() {
            return disk.twiggy;
        }
        !disk.twiggy && (self.double_sided || disk.sides < 2)
    }

    /// The clock of a PFG, or None for the 6504's own
    pub fn set_clock(&mut self, clock: Option<Ticks>) {
        self.clock = clock;
    }

    pub fn double_sided(&self) -> bool {
        self.double_sided
    }

    /// Puts a disk in a drive. A Sony drive clamps it at once. The Lisa 1's controller
    /// looks at its drives between commands.
    pub fn insert(&mut self, index: usize, disk: LisaDisk) {
        self.drives[index].disk = Some(disk);
        if !self.lisa_1() {
            self.ram[CLAMPED] = 0xFF;
            self.signal(DRIVE_LOWER, IST_INSERTED);
        }
    }

    /// Takes the disk out, as the user does: on a Lisa 1 by pressing the drive's button
    pub fn eject(&mut self, index: usize) {
        if self.lisa_1() {
            self.drives[index].button = true;
        } else {
            self.take_out(index);
        }
    }

    fn take_out(&mut self, index: usize) {
        let clamped = self.clamped_at(index);
        self.ram[clamped] = 0;
        let drive = &mut self.drives[index];
        drive.insert_signalled = false;
        if let Some(disk) = drive.disk.take() {
            self.ejected.push(disk);
        }
    }

    pub fn take_ejected(&mut self) -> Option<LisaDisk> {
        self.ejected.pop()
    }

    /// Raises interrupt status bits, given as the lower drive's, for a drive
    fn signal(&mut self, drive: u8, bits: u8) {
        let bits = bits | IST_ANY;
        self.ram[IST] |= if drive == DRIVE_LOWER {
            bits
        } else {
            bits >> 4
        };
        self.update_fdir();
    }

    fn update_fdir(&mut self) {
        if self.lisa_1() {
            self.ram[FDIR_LISA_1] = self.ram[IST] & self.ram[IMSK_LISA_1];
        } else {
            self.ram[OKTOGO] = if self.fdir() { 0xFF } else { 0 };
        }
    }

    /// What the Lisa 1's ROM does with each drive between commands, from $137E: clamps a
    /// disk put in a drive that isn't enabled, or raises "inserted" if it is; on the eject
    /// button, raises "button" if the drive is enabled, or else ejects.
    fn look_at_drives(&mut self) {
        for index in 0..self.drives.len() {
            let drive = self.drive_byte(index);
            let enabled = self.enabled(drive);
            let clamped = self.clamped_at(index);
            if self.drives[index].disk.is_none() {
                self.ram[clamped] = 0;
                self.drives[index].insert_signalled = false;
                self.drives[index].button = false;
                continue;
            }
            if self.ram[clamped] == 0 {
                if !enabled {
                    self.ram[clamped] = 0xFF;
                } else if !self.drives[index].insert_signalled {
                    self.drives[index].insert_signalled = true;
                    self.signal(drive, IST_INSERTED);
                }
            }
            if std::mem::take(&mut self.drives[index].button) {
                if enabled {
                    self.signal(drive, IST_BUTTON);
                } else {
                    self.take_out(index);
                }
            }
        }
    }

    pub fn tick(&mut self, ticks: Ticks) {
        self.now += ticks;
        if self.now < self.ready_at {
            return;
        }
        if self.timing_loop_done_at.is_some_and(|t| self.now >= t) {
            self.timing_loop_done_at = None;
            self.ram[GOBYTE] = 0xFF;
        }
        if self.command_at.is_some_and(|t| self.now >= t) {
            self.command_at = None;
            let imalive = self.imalive();
            self.ram[imalive] = self.ram[imalive].wrapping_add(1);
            self.command();
        }
        if let Some(op) = self.operation
            && self.now >= op.done_at
        {
            self.operation = None;
            self.ram[ERRSTAT] = op.error;
            log::debug!("Floppy operation done, status {:02X}", op.error);
            self.signal(op.drive, IST_DONE);
        }
        if self.lisa_1() && !self.parked && self.command_at.is_none() && self.operation.is_none() {
            self.look_at_drives();
        }
    }

    fn command(&mut self) {
        let go = self.ram[GOBYTE];
        let mask = self.ram[COMMAND];
        log::debug!(
            "Floppy command {:02X} {:02X} drive {:02X} side {} sector {} track {} at {}",
            go,
            mask,
            self.ram[DRIVE],
            self.ram[SIDE],
            self.ram[SECTOR],
            self.ram[TRACK],
            self.now
        );
        // The Lisa 1's ROM checks some of these too
        let checks = match go {
            GO_SEEK => CHECKS_SEEK_LISA_1,
            GO_SET_MASK | GO_CLEAR_MASK => CHECKS_SET_MASK_LISA_1,
            _ => 0,
        };
        if self.lisa_1() && checks != 0 {
            let error = self.validate_lisa_1(checks);
            if error != ERR_OK {
                self.ram[ERRSTAT] = error;
                self.ram[GOBYTE] = 0;
                return;
            }
        }
        match go {
            GO_NULL => (),
            GO_RWTS => self.rwts(),
            GO_SEEK => {
                let index = self.drive_index(self.ram[DRIVE]).unwrap_or(0);
                self.drives[index].head = self.ram[TRACK];
                if !self.lisa_1() {
                    self.ram[CURTRACK] = self.ram[TRACK];
                }
            }
            GO_CALL => {
                let target = u16::from_be_bytes([self.ram[ADRH], self.ram[COMMAND]]);
                if target == 0x1FFB {
                    self.reset();
                    return;
                }
                if usize::from(target) == TIMING_LOOP_AT
                    && self.ram[TIMING_LOOP_AT..TIMING_LOOP_AT + TIMING_LOOP_CODE.len()]
                        == TIMING_LOOP_CODE
                {
                    let ticks = match self.clock {
                        Some(clock) => TIMING_LOOP_TICKS * FDC_CLOCK / clock,
                        None => TIMING_LOOP_TICKS,
                    };
                    self.timing_loop_done_at = Some(self.now + ticks);
                    return;
                }
                log::warn!(
                    "Call of 6504 code at {:04X} can't run without a 6504: ignored",
                    target
                );
                self.ram[SPEED] = 0;
            }
            GO_CLEAR_IST => {
                self.ram[IST] &= !mask;
                // The OR bits follow the others
                if self.ram[IST] & 0x70 == 0 {
                    self.ram[IST] &= !0x80;
                }
                if self.ram[IST] & 0x07 == 0 {
                    self.ram[IST] &= !0x08;
                }
                self.update_fdir();
            }
            GO_SET_MASK => {
                let imsk = self.imsk();
                self.ram[imsk] |= mask & (MASK_LOWER | MASK_UPPER);
                self.update_fdir();
            }
            GO_CLEAR_MASK => {
                let imsk = self.imsk();
                self.ram[imsk] &= !mask;
                self.update_fdir();
            }
            GO_WAIT_IN_ROM | GO_AWAY => self.parked = true,
            _ => self.ram[ERRSTAT] = ERR_BAD_GOBYTE,
        }
        self.ram[GOBYTE] = 0;
    }

    fn validate(&self, op: u8) -> u8 {
        if self.lisa_1() {
            return match CHECKS_LISA_1.get(usize::from(op)) {
                Some(&checks) => self.validate_lisa_1(checks),
                None => ERR_BAD_GOBYTE,
            };
        }
        let drive = self.ram[DRIVE];
        if drive != DRIVE_LOWER && drive != DRIVE_UPPER {
            return ERR_BAD_DRIVE;
        }
        let mask = if drive == DRIVE_LOWER {
            MASK_LOWER
        } else {
            MASK_UPPER
        };
        if self.ram[IMSK] & mask == 0 {
            return ERR_NOT_ENABLED;
        }
        // One drive, the lower one
        let Some(disk) = self.drives[0]
            .disk
            .as_ref()
            .filter(|_| drive == DRIVE_LOWER)
        else {
            return ERR_NO_DISK;
        };
        if matches!(
            op,
            RWTS_READ | RWTS_WRITE | RWTS_READ_BRUTE | RWTS_WRITE_BRUTE
        ) {
            match self.ram[SIDE] {
                0 => (),
                1 if !self.double_sided => return ERR_BAD_SIDE,
                1 if disk.sides < 2 => return ERR_READ,
                1 => (),
                _ => return ERR_BAD_SIDE,
            }
            if self.ram[TRACK] >= TRACKS {
                return ERR_BAD_TRACK;
            }
            if self.ram[SECTOR] >= sectors_on_track(self.ram[TRACK]) {
                return ERR_BAD_SECTOR;
            }
        }
        if matches!(
            op,
            RWTS_WRITE | RWTS_WRITE_BRUTE | RWTS_FORMAT | RWTS_FORMAT_TRACK
        ) && disk.write_protect
        {
            return ERR_WRITE_PROTECT;
        }
        if matches!(op, RWTS_FORMAT | RWTS_FORMAT_TRACK) && self.ram[CONFIRM] != 0xFF {
            return ERR_BAD_FORMAT;
        }
        ERR_OK
    }

    /// The checks of the Lisa 1's ROM, from $14E9, in its order. A disk not clamped is no
    /// disk, and the sector is checked against the track's zone before the track itself.
    fn validate_lisa_1(&self, checks: u8) -> u8 {
        let drive = self.ram[DRIVE];
        let index = self.drive_index(drive);
        if checks & CHECK_DRIVE != 0 {
            let Some(index) = index else {
                return ERR_BAD_DRIVE;
            };
            if self.ram[self.clamped_at(index)] == 0 {
                return ERR_NO_DISK;
            }
            if !self.enabled(drive) {
                return ERR_NOT_ENABLED;
            }
        }
        let track = self.ram[TRACK];
        if checks & CHECK_SIDE != 0 && self.ram[SIDE] > 1 {
            return ERR_BAD_SIDE;
        }
        if checks & CHECK_SECTOR != 0 && self.ram[SECTOR] >= twiggy_sectors_on_track(track) {
            return ERR_BAD_SECTOR;
        }
        if checks & CHECK_TRACK != 0 && track >= TWIGGY_TRACKS {
            return ERR_BAD_TRACK;
        }
        if checks & CHECK_MASK != 0 && self.ram[COMMAND] & !(MASK_LOWER | MASK_UPPER) != 0 {
            return ERR_BAD_MASK;
        }
        if checks & CHECK_CONFIRM != 0 && self.ram[CONFIRM] != 0xFF {
            return ERR_BAD_FORMAT;
        }
        let protected = index
            .and_then(|i| self.drives[i].disk.as_ref())
            .is_some_and(|d| d.write_protect);
        if checks & CHECK_WRITE_PROTECT != 0 && protected {
            return ERR_WRITE_PROTECT;
        }
        ERR_OK
    }

    fn rwts(&mut self) {
        let op = self.ram[COMMAND];
        let drive = self.ram[DRIVE];
        let mut seek = 0;
        let error = if self.fdir() {
            ERR_INTERRUPT_PENDING
        } else {
            let mut error = self.validate(op);
            if error == ERR_OK {
                let index = self.drive_index(drive).unwrap_or(self.drives.len() - 1);
                let track = self.ram[TRACK];
                seek = u64::from(self.drives[index].head.abs_diff(track)) * STEP_TICKS;
                error = self.perform(op);
            }
            error
        };
        self.operation = Some(Operation {
            done_at: self.now + OPERATION_TICKS + seek,
            drive,
            error,
        });
    }

    /// Returns the error status: only clamp can fail once the command is valid
    fn perform(&mut self, op: u8) -> u8 {
        // The Lisa 1's ROM takes any DRIVE byte other than the upper drive's as the lower
        // drive for clamp, which it doesn't check
        let drive = self
            .drive_index(self.ram[DRIVE])
            .unwrap_or(self.drives.len() - 1);
        let lisa_1 = self.lisa_1();
        match op {
            RWTS_UNCLAMP => {
                self.take_out(drive);
                if !lisa_1 {
                    self.ram[MTRON] = 0xFF;
                }
                return ERR_OK;
            }
            RWTS_CLAMP if lisa_1 => {
                let clamped = self.clamped_at(drive);
                if self.ram[clamped] == 0 {
                    if self.drives[drive].disk.is_none() {
                        return ERR_NO_DISK;
                    }
                    self.ram[clamped] = 0xFF;
                }
                return ERR_OK;
            }
            _ => (),
        }
        let track = self.ram[TRACK];
        let sector = usize::from(self.ram[SECTOR]);
        let side = self.ram[SIDE].min(1);
        let Some(tracks) = self.drives[drive].disk.as_ref().map(LisaDisk::tracks) else {
            return ERR_OK;
        };
        self.drives[drive].head = track.min(tracks - 1);
        let Some(disk) = self.drives[drive].disk.as_mut() else {
            return ERR_OK;
        };
        let index = disk.first_sector(track.min(tracks - 1), side) + sector;
        if !lisa_1
            && matches!(
                op,
                RWTS_READ | RWTS_READ_BRUTE | RWTS_WRITE | RWTS_WRITE_BRUTE
            )
        {
            self.ram[CURTRACK] = track;
            // The ROM copies the format byte of every sector header it reads to DISKID,
            // for writes too ($12C4 in ROM $88). An image doesn't hold it: this is the
            // one the ROM writes when formatting, FMTTYPE for the sides of the disk.
            self.ram[DISKID] = if disk.sides == 2 {
                FMTTYPE_SIDE_BIT | 0x02
            } else {
                0x02
            };
        }
        match op {
            RWTS_READ | RWTS_READ_BRUTE => {
                self.ram[DATA..DATA + SECTOR_SIZE]
                    .copy_from_slice(&disk.data[index * SECTOR_SIZE..(index + 1) * SECTOR_SIZE]);
                self.ram[TAG..TAG + TAG_SIZE]
                    .copy_from_slice(&disk.tags[index * TAG_SIZE..(index + 1) * TAG_SIZE]);
            }
            RWTS_WRITE | RWTS_WRITE_BRUTE => {
                disk.data[index * SECTOR_SIZE..(index + 1) * SECTOR_SIZE]
                    .copy_from_slice(&self.ram[DATA..DATA + SECTOR_SIZE]);
                disk.tags[index * TAG_SIZE..(index + 1) * TAG_SIZE]
                    .copy_from_slice(&self.ram[TAG..TAG + TAG_SIZE]);
                disk.dirty = true;
            }
            RWTS_FORMAT => {
                disk.data.fill(0);
                disk.tags.fill(0);
                disk.dirty = true;
            }
            RWTS_FORMAT_TRACK => {
                let track = track.min(tracks - 1);
                let count = usize::from(disk.sectors_on(track));
                let first = disk.first_sector(track, side);
                disk.data[first * SECTOR_SIZE..(first + count) * SECTOR_SIZE].fill(0);
                disk.tags[first * TAG_SIZE..(first + count) * TAG_SIZE].fill(0);
                disk.dirty = true;
            }
            RWTS_VERIFY | RWTS_VERIFY_TRACK | RWTS_CLAMP => (),
            _ => log::warn!("Unknown floppy controller RWTS command {:02X}", op),
        }
        if !lisa_1 {
            self.ram[MTRON] = 0xFF;
        }
        ERR_OK
    }
}

/// One of the controller's drives, as the Floppy function sees it
pub struct LisaDrive<'a> {
    fdc: &'a mut FdcSim,
    index: usize,
}

impl FdcSim {
    /// A drive, by its slot: the upper drive first on a Lisa 1
    pub fn drive(&mut self, index: usize) -> Option<LisaDrive<'_>> {
        (index < self.drives.len()).then_some(LisaDrive { fdc: self, index })
    }

    pub fn drive_count(&self) -> usize {
        self.drives.len()
    }
}

impl LisaDrive<'_> {
    fn disk(&self) -> Option<&LisaDisk> {
        self.fdc.drives[self.index].disk.as_ref()
    }
}

/// The Floppy function of a drive
impl Floppy for LisaDrive<'_> {
    fn update_status(&mut self, status: &mut FloppyStatus) -> bool {
        let disk = self.disk();
        let new = FloppyStatus {
            settings: FloppySettings {
                media: disk.and_then(|d| d.media.clone()),
                read_only: disk.is_some_and(|d| d.write_protect),
            },
            title: disk.map(|d| d.title.clone()).unwrap_or_default(),
            ejected: disk.is_none(),
            dirty: disk.is_some_and(|d| d.dirty),
            writeback_supported: disk.is_some_and(|d| d.media.is_some()),
            writeback_enabled: disk.is_some_and(|d| d.writeback),
            image_label: "Lisa floppy images",
            image_types: IMAGE_TYPES.to_vec(),
            ..FloppyStatus::default()
        };
        replace_status(status, new)
    }

    fn load(&mut self, media: &Path, read_only: bool) -> Result<()> {
        let image =
            std::fs::read(media).with_context(|| format!("Cannot read {}", media.display()))?;
        let title = media
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut disk = LisaDisk::from_image(&image, &title, read_only)?;
        if !self.fdc.fits(&disk) {
            if self.fdc.lisa_1() || disk.twiggy {
                bail!("'{}' is not a disk for this machine's drives", title);
            }
            bail!("'{}' is an 800K disk and this drive is 400K", title);
        }
        disk.media = Some(media.to_path_buf());
        self.fdc.insert(self.index, disk);
        Ok(())
    }

    fn eject(&mut self) {
        self.fdc.eject(self.index);
    }

    fn set_writeback(&mut self, enabled: bool) -> Result<()> {
        let Some(disk) = self.fdc.drives[self.index].disk.as_mut() else {
            bail!("There is no disk in the drive");
        };
        disk.writeback = enabled;
        Ok(())
    }
}

const IMAGE_TYPES: &[&str] = &["dc42", "image", "img", "dsk"];

/// The Lisa 2's
fn fits_400k(mac: MacModel) -> bool {
    matches!(mac, MacModel::Lisa25 | MacModel::Lisa210)
}

pub const INFO_400K: ModelInfo = ModelInfo {
    model: DeviceModel::Lisa400KDrive,
    name: "400K drive",
    bus: BusKind::LisaFdc,
    fits: fits_400k,
    function: FunctionKind::Floppy,
    fixed: true,
    one_per_bus: false,
    rom: None,
    path_keys: &["media"],
    adds_buses: &[],
    create: None,
};

fn fits_twiggy(mac: MacModel) -> bool {
    mac == MacModel::Lisa1
}

pub const INFO_TWIGGY: ModelInfo = ModelInfo {
    model: DeviceModel::LisaTwiggyDrive,
    name: "Twiggy drive",
    bus: BusKind::LisaFdc,
    fits: fits_twiggy,
    function: FunctionKind::Floppy,
    fixed: true,
    one_per_bus: false,
    rom: None,
    path_keys: &["media"],
    adds_buses: &[],
    create: None,
};

/// The Lisa 2/10 can have one, for MacWorks Plus
fn fits_800k(mac: MacModel) -> bool {
    mac == MacModel::Lisa210
}

pub const INFO_800K: ModelInfo = ModelInfo {
    model: DeviceModel::Lisa800KDrive,
    name: "800K drive",
    bus: BusKind::LisaFdc,
    fits: fits_800k,
    function: FunctionKind::Floppy,
    fixed: true,
    one_per_bus: false,
    rom: None,
    path_keys: &["media"],
    adds_buses: &[],
    create: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn disk() -> LisaDisk {
        let sectors = 800;
        let mut data = vec![0; sectors * SECTOR_SIZE];
        // Sector 12: first of track 1
        data[12 * SECTOR_SIZE] = 0x5A;
        let mut tags = vec![0; sectors * TAG_SIZE];
        tags[12 * TAG_SIZE] = 0xAA;
        let mut image = vec![0; DC42_HEADER_SIZE];
        image[DC42_DATA_SIZE..DC42_DATA_SIZE + 4]
            .copy_from_slice(&((sectors * SECTOR_SIZE) as u32).to_be_bytes());
        image[DC42_TAG_SIZE..DC42_TAG_SIZE + 4]
            .copy_from_slice(&((sectors * TAG_SIZE) as u32).to_be_bytes());
        image.extend(data);
        image.extend(tags);
        LisaDisk::from_image(&image, "test", false).unwrap()
    }

    #[test]
    fn parameter_memory_file() {
        let dir = std::env::temp_dir().join(format!("snow-lisa-pm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lisa.pram");
        let _ = std::fs::remove_file(&path);

        // No file: run down battery, written out as such
        let mut fdc = ready();
        fdc.persist_parameter_memory(&path).unwrap();
        fdc.save_parameter_memory().unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            vec![0xFF; PARAMETER_MEMORY_SIZE]
        );

        // Only parameter memory counts as a change, and survives the next start
        std::fs::remove_file(&path).unwrap();
        fdc.ram_write(COMMAND, 0x12);
        fdc.ram_write(PARAMETER_MEMORY, 0xFF);
        fdc.save_parameter_memory().unwrap();
        assert!(!path.exists());
        fdc.ram_write(PARAMETER_MEMORY + 1, 0x34);
        fdc.save_parameter_memory().unwrap();

        let mut fdc = ready();
        fdc.persist_parameter_memory(&path).unwrap();
        assert_eq!(fdc.ram_read(PARAMETER_MEMORY + 1), 0x34);
        assert_eq!(fdc.ram_read(PARAMETER_MEMORY), 0xFF);

        // A Mac's PRAM file stays as it is
        std::fs::write(&path, vec![0x55; 256]).unwrap();
        let mut fdc = ready();
        assert!(fdc.persist_parameter_memory(&path).is_err());
        fdc.ram_write(PARAMETER_MEMORY, 0);
        fdc.save_parameter_memory().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), vec![0x55; 256]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn ready() -> FdcSim {
        let mut fdc = FdcSim::new(ROM_ID_2_5, false);
        fdc.tick(START_TICKS);
        fdc
    }

    fn go(fdc: &mut FdcSim, gobyte: u8, command: u8) {
        fdc.ram_write(COMMAND, command);
        fdc.ram_write(GOBYTE, gobyte);
        fdc.tick(COMMAND_TICKS);
        assert_eq!(fdc.ram_read(GOBYTE), 0);
    }

    #[test]
    fn not_listening_at_first() {
        let mut fdc = FdcSim::new(ROM_ID_2_5, false);
        assert!(!fdc.diag());
        assert_eq!(fdc.ram_read(0x18), 0xA8);
        assert_eq!(FdcSim::new(ROM_ID_2_10, false).ram_read(0x18), 0x88);
        fdc.tick(START_TICKS);
        assert!(fdc.diag());
    }

    #[test]
    fn geometry() {
        assert_eq!(sectors_before_track(1), 12);
        assert_eq!(sectors_before_track(16), 192);
        assert_eq!(sectors_before_track(80), 800);
        assert_eq!(sectors_on_track(79), 8);
    }

    #[test]
    fn read_sector() {
        let mut fdc = ready();
        fdc.insert(0, disk());
        go(&mut fdc, GO_SET_MASK, MASK_LOWER);
        // The insertion pends
        assert!(fdc.fdir());
        go(&mut fdc, GO_CLEAR_IST, 0xFF);
        assert!(!fdc.fdir());

        fdc.ram_write(DRIVE, DRIVE_LOWER);
        fdc.ram_write(TRACK, 1);
        fdc.ram_write(SECTOR, 0);
        go(&mut fdc, GO_RWTS, RWTS_READ);
        fdc.tick(OPERATION_TICKS + STEP_TICKS);
        assert!(fdc.fdir());
        assert_eq!(fdc.ram_read(ERRSTAT), ERR_OK);
        assert_eq!(fdc.ram_read(DATA), 0x5A);
        assert_eq!(fdc.ram_read(TAG), 0xAA);
        go(&mut fdc, GO_CLEAR_IST, 0xCC);
        assert!(!fdc.fdir());
    }

    #[test]
    fn pending_interrupt_refuses() {
        let mut fdc = ready();
        fdc.insert(0, disk());
        go(&mut fdc, GO_SET_MASK, MASK_LOWER);
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        go(&mut fdc, GO_RWTS, RWTS_READ);
        fdc.tick(OPERATION_TICKS);
        assert_eq!(fdc.ram_read(ERRSTAT), ERR_INTERRUPT_PENDING);
    }

    #[test]
    fn no_disk_and_not_enabled() {
        let mut fdc = ready();
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        go(&mut fdc, GO_RWTS, RWTS_READ);
        fdc.tick(OPERATION_TICKS);
        assert_eq!(fdc.ram_read(ERRSTAT), ERR_NOT_ENABLED);

        go(&mut fdc, GO_SET_MASK, MASK_LOWER);
        go(&mut fdc, GO_CLEAR_IST, 0xFF);
        go(&mut fdc, GO_RWTS, RWTS_READ);
        fdc.tick(OPERATION_TICKS);
        assert_eq!(fdc.ram_read(ERRSTAT), ERR_NO_DISK);
    }

    #[test]
    fn parameter_memory_survives() {
        let mut fdc = FdcSim::new(ROM_ID_2_5, false);
        fdc.ram_write(0xC5, 0x12);
        fdc.ram_write(0x30, 0x34);
        fdc.reset();
        assert_eq!(fdc.ram_read(0xC5), 0x12);
        assert_eq!(fdc.ram_read(0x30), 0);
    }

    #[test]
    fn rejects_other_sizes() {
        assert!(LisaDisk::from_image(&[0; 1024], "x", false).is_err());
    }

    #[test]
    fn dc42_checksum_rotates() {
        assert_eq!(dc42_checksum(&[0, 1, 0, 2]), 0x4000_0001);
    }

    #[test]
    fn write_comes_back_in_the_image() {
        let mut fdc = ready();
        fdc.insert(0, disk());
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        fdc.ram_write(TRACK, 1);
        fdc.ram_write(SECTOR, 0);
        fdc.ram_write(DATA, 0x77);
        fdc.ram_write(TAG, 0x88);
        assert!(!fdc.drives[0].disk.as_ref().unwrap().dirty());
        fdc.perform(RWTS_WRITE);
        fdc.perform(RWTS_UNCLAMP);
        assert!(fdc.drives[0].disk.is_none());

        let ejected = fdc.take_ejected().unwrap();
        assert!(ejected.dirty());
        assert!(fdc.take_ejected().is_none());
        let again = LisaDisk::from_image(&ejected.to_image(), "again", false).unwrap();
        assert_eq!(again.data[12 * SECTOR_SIZE], 0x77);
        assert_eq!(again.tags[12 * TAG_SIZE], 0x88);
        assert!(!again.dirty());
    }

    #[test]
    fn raw_image_stays_raw() {
        let image = vec![0x11; 800 * SECTOR_SIZE];
        let disk = LisaDisk::from_image(&image, "raw", false).unwrap();
        assert_eq!(disk.to_image(), image);
    }

    #[test]
    fn write_protected_disk_refuses_writes() {
        let mut fdc = ready();
        fdc.insert(
            0,
            LisaDisk::from_image(&vec![0; 800 * SECTOR_SIZE], "wp", true).unwrap(),
        );
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        fdc.ram[IMSK] |= MASK_LOWER;
        assert_eq!(fdc.validate(RWTS_READ), ERR_OK);
        assert_eq!(fdc.validate(RWTS_WRITE), 20);
    }

    fn double_sided_disk() -> LisaDisk {
        let mut image = vec![0; 1600 * SECTOR_SIZE];
        // Track 1 side 0, after the 24 of track 0, then its side 1
        image[24 * SECTOR_SIZE] = 0x10;
        image[36 * SECTOR_SIZE] = 0x11;
        LisaDisk::from_image(&image, "800K", false).unwrap()
    }

    #[test]
    fn side_1_of_an_800k_disk() {
        let mut fdc = FdcSim::new(ROM_ID_2_10, true);
        assert_eq!(fdc.ram_read(NOSIDES), 2);
        assert_eq!(fdc.ram_read(FMTTYPE), 0x22);
        fdc.insert(0, double_sided_disk());
        fdc.ram[IMSK] |= MASK_LOWER;
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        fdc.ram_write(TRACK, 1);
        fdc.ram_write(SECTOR, 0);
        fdc.ram_write(SIDE, 0);
        fdc.perform(RWTS_READ);
        assert_eq!(fdc.ram_read(DATA), 0x10);
        assert_eq!(fdc.ram_read(DISKID), 0x22);
        fdc.ram_write(SIDE, 1);
        assert_eq!(fdc.validate(RWTS_READ), ERR_OK);
        fdc.perform(RWTS_READ);
        assert_eq!(fdc.ram_read(DATA), 0x11);
    }

    #[test]
    fn drive_and_disk_sides() {
        let mut fdc = FdcSim::new(ROM_ID_2_10, false);
        assert!(!fdc.fits(&double_sided_disk()));
        fdc.insert(0, disk());
        fdc.ram[IMSK] |= MASK_LOWER;
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        fdc.ram_write(SIDE, 1);
        assert_eq!(fdc.validate(RWTS_READ), ERR_BAD_SIDE);

        let mut fdc = FdcSim::new(ROM_ID_2_10, true);
        assert!(fdc.fits(&double_sided_disk()));
        fdc.insert(0, disk());
        fdc.ram[IMSK] |= MASK_LOWER;
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        fdc.ram_write(SIDE, 1);
        assert_eq!(fdc.validate(RWTS_READ), ERR_READ);
        fdc.ram_write(SIDE, 0);
        fdc.ram[DISKID] = 0;
        fdc.perform(RWTS_WRITE);
        assert_eq!(fdc.ram_read(DISKID), 0x02);
    }

    /// A Lisa 1 disk, padded as BLU pads them, with a mark in the first sector of
    /// track 4 on side 1
    fn twiggy_image() -> Vec<u8> {
        let sectors = 1702;
        let mut data = vec![0; sectors * SECTOR_SIZE];
        data[(851 + 4 * 22) * SECTOR_SIZE] = 0x41;
        let tags = vec![0; sectors * TAG_SIZE];
        let mut image = vec![0; DC42_HEADER_SIZE];
        image[DC42_DATA_SIZE..DC42_DATA_SIZE + 4]
            .copy_from_slice(&((sectors * SECTOR_SIZE) as u32).to_be_bytes());
        image[DC42_TAG_SIZE..DC42_TAG_SIZE + 4]
            .copy_from_slice(&((sectors * TAG_SIZE) as u32).to_be_bytes());
        image[DC42_MAGIC] = 0x01;
        image.extend(data);
        image.extend(tags);
        image.extend([0x1A; 996]);
        image
    }

    fn lisa_1() -> FdcSim {
        let mut fdc = FdcSim::new(ROM_ID_LISA_1, false);
        fdc.tick(START_TICKS);
        fdc
    }

    #[test]
    fn twiggy_geometry() {
        assert_eq!(twiggy_sectors_before_track(TWIGGY_TRACKS), 851);
        assert_eq!(twiggy_sectors_on_track(3), 22);
        assert_eq!(twiggy_sectors_on_track(4), 21);
        assert_eq!(twiggy_sectors_on_track(45), 15);
    }

    #[test]
    fn twiggy_image_keeps_its_padding() {
        let image = twiggy_image();
        let disk = LisaDisk::from_image(&image, "twiggy", false).unwrap();
        assert!(disk.twiggy());
        assert_eq!(
            disk.to_image()[DC42_HEADER_SIZE..],
            image[DC42_HEADER_SIZE..]
        );
        assert!(!FdcSim::new(ROM_ID_2_10, true).fits(&disk));
        assert!(lisa_1().fits(&disk));
        assert!(!lisa_1().fits(&disk_with_sides(1)));
    }

    fn disk_with_sides(sides: usize) -> LisaDisk {
        LisaDisk::from_image(&vec![0; 800 * sides * SECTOR_SIZE], "sony", false).unwrap()
    }

    #[test]
    fn lisa_1_constants() {
        let fdc = lisa_1();
        assert_eq!(fdc.ram_read(0x18), ROM_ID_LISA_1);
        assert_eq!(fdc.ram_read(0x15), 0x1E);
    }

    #[test]
    fn lisa_1_clamps_a_disk_in_a_drive_not_enabled() {
        let mut fdc = lisa_1();
        fdc.insert(
            0,
            LisaDisk::from_image(&twiggy_image(), "t", false).unwrap(),
        );
        fdc.tick(1);
        assert_eq!(fdc.ram_read(CLAMPED_LISA_1[0]), 0xFF);
        assert!(!fdc.fdir());

        go(&mut fdc, GO_SET_MASK, MASK_UPPER);
        assert_eq!(fdc.ram_read(IMSK_LISA_1), MASK_UPPER);
        fdc.ram_write(DRIVE, DRIVE_UPPER);
        fdc.ram_write(SIDE, 1);
        fdc.ram_write(TRACK, 4);
        fdc.ram_write(SECTOR, 0);
        go(&mut fdc, GO_RWTS, RWTS_READ);
        fdc.tick(OPERATION_TICKS + 4 * STEP_TICKS);
        assert_eq!(fdc.ram_read(ERRSTAT), ERR_OK);
        assert_eq!(fdc.ram_read(DATA), 0x41);
        // Done, for drive 1
        assert_eq!(fdc.ram_read(IST), 0x0C);
        assert!(fdc.fdir());
        assert_eq!(fdc.ram_read(FDIR_LISA_1), 0x08);
    }

    #[test]
    fn lisa_1_waits_for_clamp_in_an_enabled_drive() {
        let mut fdc = lisa_1();
        go(&mut fdc, GO_SET_MASK, MASK_LOWER);
        fdc.insert(
            1,
            LisaDisk::from_image(&twiggy_image(), "t", false).unwrap(),
        );
        fdc.tick(1);
        assert_eq!(fdc.ram_read(CLAMPED_LISA_1[1]), 0);
        assert_eq!(fdc.ram_read(IST), IST_INSERTED | IST_ANY);
        go(&mut fdc, GO_CLEAR_IST, 0xFF);
        fdc.tick(1);
        // Once only
        assert_eq!(fdc.ram_read(IST), 0);

        fdc.ram_write(DRIVE, DRIVE_LOWER);
        assert_eq!(fdc.validate(RWTS_READ), ERR_NO_DISK);
        go(&mut fdc, GO_RWTS, RWTS_CLAMP);
        fdc.tick(OPERATION_TICKS);
        assert_eq!(fdc.ram_read(ERRSTAT), ERR_OK);
        assert_eq!(fdc.ram_read(CLAMPED_LISA_1[1]), 0xFF);
    }

    #[test]
    fn lisa_1_eject_button() {
        let mut fdc = lisa_1();
        fdc.insert(
            0,
            LisaDisk::from_image(&twiggy_image(), "t", false).unwrap(),
        );
        fdc.tick(1);
        go(&mut fdc, GO_SET_MASK, MASK_UPPER);
        fdc.eject(0);
        fdc.tick(1);
        // Enabled: the host decides
        assert_eq!(fdc.ram_read(IST), (IST_BUTTON | IST_ANY) >> 4);
        assert!(fdc.drives[0].disk.is_some());
        go(&mut fdc, GO_CLEAR_IST, 0xFF);
        go(&mut fdc, GO_CLEAR_MASK, MASK_UPPER);
        // Not enabled: out it comes
        fdc.eject(0);
        fdc.tick(1);
        assert!(fdc.drives[0].disk.is_none());
        assert_eq!(fdc.ram_read(CLAMPED_LISA_1[0]), 0);
        assert!(fdc.take_ejected().is_some());
    }

    #[test]
    fn lisa_1_checks_in_its_order() {
        let mut fdc = lisa_1();
        fdc.ram_write(DRIVE, 0x40);
        assert_eq!(fdc.validate(RWTS_READ), ERR_BAD_DRIVE);
        fdc.ram_write(DRIVE, DRIVE_UPPER);
        // No disk before not enabled
        assert_eq!(fdc.validate(RWTS_READ), ERR_NO_DISK);
        fdc.insert(0, LisaDisk::from_image(&twiggy_image(), "t", true).unwrap());
        fdc.tick(1);
        assert_eq!(fdc.validate(RWTS_READ), ERR_NOT_ENABLED);
        go(&mut fdc, GO_SET_MASK, MASK_UPPER);
        fdc.ram_write(SIDE, 2);
        assert_eq!(fdc.validate(RWTS_READ), ERR_BAD_SIDE);
        fdc.ram_write(SIDE, 1);
        // The sector of the track's zone before the track
        fdc.ram_write(TRACK, 46);
        fdc.ram_write(SECTOR, 15);
        assert_eq!(fdc.validate(RWTS_READ), ERR_BAD_SECTOR);
        fdc.ram_write(SECTOR, 14);
        assert_eq!(fdc.validate(RWTS_READ), ERR_BAD_TRACK);
        fdc.ram_write(TRACK, 0);
        fdc.ram_write(SECTOR, 21);
        assert_eq!(fdc.validate(RWTS_READ), ERR_OK);
        assert_eq!(fdc.validate(RWTS_WRITE), ERR_WRITE_PROTECT);
        // A mask with other bits than the drives'
        go(&mut fdc, GO_SET_MASK, 0x01);
        assert_eq!(fdc.ram_read(ERRSTAT), ERR_BAD_MASK);
    }
}
