//! Simulation of the Lisa's floppy controller: the 6504 with its ROM, and a drive
//!
//! Spec: docs/lisa/lisa-spec-fdc.md. It is seen through its shared RAM and two lines only,
//! so that a 6504 running the ROM can replace it.

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

/// Default constants, from the ROM of the 2/5. The one at $18 is the ROM id.
const CONSTANTS_2_5: [u8; 13] = [
    0xD5, 0xC0, 0xA7, 0x89, 0x64, 0x1E, 0x04, 0x09, 0xA8, 0x64, 0x02, 0x82, 0x4F,
];

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
const ERR_NO_DISK: u8 = 0x07;
const ERR_NOT_ENABLED: u8 = 0x08;
const ERR_INTERRUPT_PENDING: u8 = 0x09;
const ERR_BAD_FORMAT: u8 = 0x0A;
const ERR_WRITE_PROTECT: u8 = 0x14;
const ERR_READ: u8 = 0x17;

/// Interrupt status bits of the lower drive, and of the upper one 4 below
const IST_INSERTED: u8 = 0x10;
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

/// A floppy image: 512-byte sectors, and a 12-byte tag for each
#[derive(Serialize, Deserialize)]
pub struct LisaDisk {
    data: Vec<u8>,
    tags: Vec<u8>,
    write_protect: bool,
    sides: u8,
    /// DiskCopy 4.2 header, none for a raw image
    dc42_header: Option<Vec<u8>>,
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
        let (data, tags) = match (be32(DC42_DATA_SIZE), be32(DC42_TAG_SIZE)) {
            (Some(data_size), Some(tag_size))
                if data_size > 0
                    && data_size % SECTOR_SIZE == 0
                    && DC42_HEADER_SIZE + data_size + tag_size == image.len() =>
            {
                dc42_header = Some(image[..DC42_HEADER_SIZE].to_vec());
                let tags_at = DC42_HEADER_SIZE + data_size;
                (&image[DC42_HEADER_SIZE..tags_at], &image[tags_at..])
            }
            _ if !image.is_empty() && image.len().is_multiple_of(SECTOR_SIZE) => {
                (image, &image[..0])
            }
            _ => bail!("Not a DiskCopy 4.2 or raw image"),
        };
        let sectors = data.len() / SECTOR_SIZE;
        let sides = if sectors == sectors_before_track(TRACKS) {
            1
        } else if sectors == 2 * sectors_before_track(TRACKS) {
            2
        } else {
            bail!(
                "A Lisa disk has 800 or 1600 sectors, this image has {}",
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
            dc42_header,
            dirty: false,
            title: title.to_owned(),
            media: None,
            writeback: false,
        })
    }

    pub fn sides(&self) -> u8 {
        self.sides
    }

    /// The first sector of a track and side. Side 1 of a track follows side 0.
    fn first_sector(&self, track: u8, side: u8) -> usize {
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
    disk: Option<LisaDisk>,
    /// A disk came out
    /// Out of the drive, for the emulator to write back
    ejected: Option<LisaDisk>,
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
            disk: None,
            ejected: None,
            parameter_memory_path: None,
            parameter_memory_changed: false,
        };
        fdc.reset();
        fdc
    }

    /// Cold start of the controller. Only parameter memory stays.
    pub fn reset(&mut self) {
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
        self.ram[CLAMPED] = if self.disk.is_some() { 0xFF } else { 0 };
        self.ready_at = self.now + START_TICKS;
        self.command_at = None;
        self.operation = None;
        self.parked = false;
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
        let mask = self.ram[IMSK];
        (mask & MASK_LOWER != 0 && ist & 0xF0 != 0) || (mask & MASK_UPPER != 0 && ist & 0x0F != 0)
    }

    /// DSKDIAG: listening
    pub fn diag(&self) -> bool {
        self.now >= self.ready_at
    }

    /// Whether the drive can take a disk
    pub fn fits(&self, disk: &LisaDisk) -> bool {
        self.double_sided || disk.sides < 2
    }

    /// The clock of a PFG, or None for the 6504's own
    pub fn set_clock(&mut self, clock: Option<Ticks>) {
        self.clock = clock;
    }

    pub fn double_sided(&self) -> bool {
        self.double_sided
    }

    pub fn insert(&mut self, disk: LisaDisk) {
        self.disk = Some(disk);
        self.ram[CLAMPED] = 0xFF;
        self.set_ist(IST_INSERTED);
    }

    /// Takes the disk out, as the user does
    pub fn eject(&mut self) {
        self.ram[CLAMPED] = 0;
        self.ejected = self.disk.take();
    }

    pub fn disk_title(&self) -> Option<&str> {
        self.disk.as_ref().map(|d| d.title.as_str())
    }

    pub fn take_ejected(&mut self) -> Option<LisaDisk> {
        self.ejected.take()
    }

    fn set_ist(&mut self, bits: u8) {
        self.ram[IST] |= bits | IST_ANY;
        self.update_fdir();
    }

    fn update_fdir(&mut self) {
        self.ram[OKTOGO] = if self.fdir() { 0xFF } else { 0 };
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
            self.ram[IMALIVE] = self.ram[IMALIVE].wrapping_add(1);
            self.command();
        }
        if let Some(op) = self.operation
            && self.now >= op.done_at
        {
            self.operation = None;
            self.ram[ERRSTAT] = op.error;
            log::debug!("Floppy operation done, status {:02X}", op.error);
            let bits = if op.drive == DRIVE_LOWER {
                IST_DONE
            } else {
                (IST_DONE | IST_ANY) >> 4
            };
            if op.drive == DRIVE_LOWER {
                self.set_ist(bits);
            } else {
                self.ram[IST] |= bits;
                self.update_fdir();
            }
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
        match go {
            GO_NULL => (),
            GO_RWTS => self.rwts(),
            GO_SEEK => self.ram[CURTRACK] = self.ram[TRACK],
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
                self.ram[IMSK] |= mask & (MASK_LOWER | MASK_UPPER);
                self.update_fdir();
            }
            GO_CLEAR_MASK => {
                self.ram[IMSK] &= !mask;
                self.update_fdir();
            }
            GO_WAIT_IN_ROM | GO_AWAY => self.parked = true,
            _ => self.ram[ERRSTAT] = ERR_BAD_GOBYTE,
        }
        self.ram[GOBYTE] = 0;
    }

    fn validate(&self, op: u8) -> u8 {
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
        let Some(disk) = self.disk.as_ref().filter(|_| drive == DRIVE_LOWER) else {
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

    fn rwts(&mut self) {
        let op = self.ram[COMMAND];
        let drive = self.ram[DRIVE];
        let mut seek = 0;
        let error = if self.fdir() {
            ERR_INTERRUPT_PENDING
        } else {
            let error = self.validate(op);
            if error == ERR_OK {
                let track = self.ram[TRACK];
                seek = u64::from(self.ram[CURTRACK].abs_diff(track)) * STEP_TICKS;
                self.perform(op);
            }
            error
        };
        self.operation = Some(Operation {
            done_at: self.now + OPERATION_TICKS + seek,
            drive,
            error,
        });
    }

    fn perform(&mut self, op: u8) {
        let track = self.ram[TRACK];
        let sector = usize::from(self.ram[SECTOR]);
        let side = self.ram[SIDE].min(1);
        let Some(disk) = self.disk.as_mut() else {
            return;
        };
        let index = disk.first_sector(track.min(TRACKS - 1), side) + sector;
        if matches!(
            op,
            RWTS_READ | RWTS_READ_BRUTE | RWTS_WRITE | RWTS_WRITE_BRUTE
        ) {
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
                self.ram[CURTRACK] = track;
                self.ram[DATA..DATA + SECTOR_SIZE]
                    .copy_from_slice(&disk.data[index * SECTOR_SIZE..(index + 1) * SECTOR_SIZE]);
                self.ram[TAG..TAG + TAG_SIZE]
                    .copy_from_slice(&disk.tags[index * TAG_SIZE..(index + 1) * TAG_SIZE]);
            }
            RWTS_WRITE | RWTS_WRITE_BRUTE => {
                self.ram[CURTRACK] = track;
                disk.data[index * SECTOR_SIZE..(index + 1) * SECTOR_SIZE]
                    .copy_from_slice(&self.ram[DATA..DATA + SECTOR_SIZE]);
                disk.tags[index * TAG_SIZE..(index + 1) * TAG_SIZE]
                    .copy_from_slice(&self.ram[TAG..TAG + TAG_SIZE]);
                disk.dirty = true;
            }
            RWTS_UNCLAMP => {
                self.eject();
            }
            RWTS_FORMAT => {
                disk.data.fill(0);
                disk.tags.fill(0);
                disk.dirty = true;
            }
            RWTS_FORMAT_TRACK => {
                let count = usize::from(sectors_on_track(track.min(TRACKS - 1)));
                let first = disk.first_sector(track.min(TRACKS - 1), side);
                disk.data[first * SECTOR_SIZE..(first + count) * SECTOR_SIZE].fill(0);
                disk.tags[first * TAG_SIZE..(first + count) * TAG_SIZE].fill(0);
                disk.dirty = true;
            }
            RWTS_VERIFY | RWTS_VERIFY_TRACK | RWTS_CLAMP => (),
            _ => log::warn!("Unknown floppy controller RWTS command {:02X}", op),
        }
        self.ram[MTRON] = 0xFF;
    }
}

/// The Floppy function of the drive
impl Floppy for FdcSim {
    fn update_status(&mut self, status: &mut FloppyStatus) -> bool {
        let disk = self.disk.as_ref();
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
        if !self.fits(&disk) {
            bail!("'{}' is an 800K disk and this drive is 400K", title);
        }
        disk.media = Some(media.to_path_buf());
        self.insert(disk);
        Ok(())
    }

    fn eject(&mut self) {
        Self::eject(self);
    }

    fn set_writeback(&mut self, enabled: bool) -> Result<()> {
        let Some(disk) = self.disk.as_mut() else {
            bail!("There is no disk in the drive");
        };
        disk.writeback = enabled;
        Ok(())
    }
}

const IMAGE_TYPES: &[&str] = &["dc42", "image", "img", "dsk"];

pub const INFO_400K: ModelInfo = ModelInfo {
    model: DeviceModel::Lisa400KDrive,
    name: "400K drive",
    bus: BusKind::LisaFdc,
    fits: MacModel::is_lisa,
    function: FunctionKind::Floppy,
    fixed: true,
    one_per_bus: false,
    rom: None,
    path_keys: &["media"],
    adds_buses: &[],
    monitors: &[],
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
    monitors: &[],
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
        fdc.insert(disk());
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
        fdc.insert(disk());
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
        fdc.insert(disk());
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        fdc.ram_write(TRACK, 1);
        fdc.ram_write(SECTOR, 0);
        fdc.ram_write(DATA, 0x77);
        fdc.ram_write(TAG, 0x88);
        assert!(!fdc.disk.as_ref().unwrap().dirty());
        fdc.perform(RWTS_WRITE);
        fdc.perform(RWTS_UNCLAMP);
        assert!(fdc.disk.is_none());

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
        fdc.insert(LisaDisk::from_image(&vec![0; 800 * SECTOR_SIZE], "wp", true).unwrap());
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
        fdc.insert(double_sided_disk());
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
        fdc.insert(disk());
        fdc.ram[IMSK] |= MASK_LOWER;
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        fdc.ram_write(SIDE, 1);
        assert_eq!(fdc.validate(RWTS_READ), ERR_BAD_SIDE);

        let mut fdc = FdcSim::new(ROM_ID_2_10, true);
        assert!(fdc.fits(&double_sided_disk()));
        fdc.insert(disk());
        fdc.ram[IMSK] |= MASK_LOWER;
        fdc.ram_write(DRIVE, DRIVE_LOWER);
        fdc.ram_write(SIDE, 1);
        assert_eq!(fdc.validate(RWTS_READ), ERR_READ);
        fdc.ram_write(SIDE, 0);
        fdc.ram[DISKID] = 0;
        fdc.perform(RWTS_WRITE);
        assert_eq!(fdc.ram_read(DISKID), 0x02);
    }
}
