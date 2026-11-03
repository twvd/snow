//! Simulation of a ProFile hard disk on the parallel port of the Lisa
//!
//! Spec: docs/lisa/lisa-spec-profile.md

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::emulator::devices::{
    BusKind, CreateFn, DeviceConfig, DeviceModel, FunctionKind, HardDisk, HardDiskSettings,
    HardDiskStatus, ModelInfo, replace_status,
};
use crate::mac::MacModel;
use crate::tickable::Ticks;

const BLOCK_SIZE: usize = 532;
const STATUS_SIZE: usize = 4;
const COMMAND_SIZE: usize = 6;

const BLOCKS_5MB: usize = 9728;
const DEVICE_BLOCK: usize = 0xFF_FFFF;

const CMD_READ: u8 = 0;
const CMD_WRITE: u8 = 1;

const STATE_READY: u8 = 0x01;
const STATE_READ: u8 = 0x02;
const STATE_WRITE: u8 = 0x03;
const STATE_WRITTEN: u8 = 0x06;

const REPLY_OK: u8 = 0x55;

/// Status byte 0 for a block past the end of the disk
const STATUS_BAD_BLOCK: u8 = 0x40;

/// CMD to busy, and a reply to not busy
const HANDSHAKE_TICKS: Ticks = 41;
/// Reading or writing a block
const BLOCK_TICKS: Ticks = 10_188;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
enum Phase {
    /// Waiting for a command, or sending data, which any handshake ends
    Ready,
    /// Taking the bytes of a command
    Command,
    /// Taking the block to write
    WriteData,
}

#[derive(Serialize, Deserialize)]
pub struct ProfileSim {
    /// The image. A save state has it apart, as the media of the slot.
    #[serde(skip)]
    data: Vec<u8>,
    /// The image the user gave: a restored machine runs on a copy
    path: Option<PathBuf>,
    #[serde(skip)]
    file: Option<File>,
    /// Writes go to the file. Without, they stay in memory.
    writable: bool,

    now: Ticks,
    phase: Phase,
    busy: bool,
    /// When busy changes
    busy_at: Option<Ticks>,
    /// CMD as last seen
    cmd: bool,
    /// The byte on the port while talking
    state_byte: u8,
    /// In a handshake, between CMD asserted and the reply
    in_handshake: bool,

    /// The block the write data goes to
    write_target: usize,
    /// Bytes coming in
    incoming: Vec<u8>,
    /// Bytes going out, the next at `outgoing_pos`
    outgoing: Vec<u8>,
    outgoing_pos: usize,
}

impl ProfileSim {
    pub fn new() -> Self {
        Self {
            data: vec![],
            path: None,
            file: None,
            writable: false,
            now: 0,
            phase: Phase::Ready,
            busy: false,
            busy_at: None,
            cmd: false,
            state_byte: STATE_READY,
            in_handshake: false,
            write_target: 0,
            incoming: Vec::with_capacity(BLOCK_SIZE),
            outgoing: Vec::with_capacity(BLOCK_SIZE + STATUS_SIZE),
            outgoing_pos: 0,
        }
    }

    /// A drive with its image, which writes go to if it's writable
    pub fn open(path: &Path, writable: bool) -> Result<Self> {
        let mut drive = Self::new();
        drive.load(path, writable)?;
        drive.path = Some(path.to_path_buf());
        Ok(drive)
    }

    /// An image made for a Mac disk has a part block at the end, which isn't used
    fn load(&mut self, path: &Path, writable: bool) -> Result<()> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)
            .with_context(|| format!("Cannot open {}", path.display()))?;
        let mut data = vec![];
        file.read_to_end(&mut data)?;
        if data.len() / BLOCK_SIZE < BLOCKS_5MB {
            bail!(
                "A Lisa hard disk has at least {} blocks of {} bytes",
                BLOCKS_5MB,
                BLOCK_SIZE
            );
        }
        self.data = data;
        self.file = writable.then_some(file);
        self.writable = writable;
        Ok(())
    }

    /// Takes the image back after a save state was loaded
    #[cfg(feature = "savestates")]
    pub fn after_restore(&mut self, media: &Path) -> Result<()> {
        self.load(media, self.writable)
    }

    /// The image, for a save state
    #[cfg(feature = "savestates")]
    pub fn savestate_media(&self) -> &[u8] {
        &self.data
    }

    pub fn reset(&mut self) {
        self.phase = Phase::Ready;
        self.busy = false;
        self.busy_at = None;
        self.in_handshake = false;
        self.incoming.clear();
        self.outgoing.clear();
        self.outgoing_pos = 0;
    }

    /// BSY pin: high is not busy
    pub fn not_busy(&self) -> bool {
        !self.busy
    }

    /// What the drive puts on the data lines
    pub fn port(&self) -> u8 {
        if self.in_handshake {
            self.state_byte
        } else {
            self.outgoing
                .get(self.outgoing_pos)
                .copied()
                .unwrap_or(0xFF)
        }
    }

    pub fn tick(&mut self, ticks: Ticks) {
        self.now += ticks;
        if let Some(at) = self.busy_at
            && self.now >= at
        {
            self.busy_at = None;
            self.busy = self.in_handshake;
        }
    }

    /// The CMD line, with what the host has on the data lines
    pub fn set_cmd(&mut self, asserted: bool, port: u8) {
        if asserted == self.cmd {
            return;
        }
        self.cmd = asserted;
        if asserted {
            self.start_handshake();
        } else if self.in_handshake {
            self.end_handshake(port);
        }
    }

    fn start_handshake(&mut self) {
        self.in_handshake = true;
        self.state_byte = match self.phase {
            Phase::Ready => STATE_READY,
            Phase::Command => match self.incoming.first() {
                Some(&CMD_READ) if self.incoming.len() >= COMMAND_SIZE => STATE_READ,
                Some(&CMD_WRITE) if self.incoming.len() >= COMMAND_SIZE => STATE_WRITE,
                _ => STATE_READY,
            },
            Phase::WriteData => STATE_WRITTEN,
        };
        self.busy_at = Some(self.now + HANDSHAKE_TICKS);
    }

    fn end_handshake(&mut self, reply: u8) {
        self.in_handshake = false;
        let mut release = HANDSHAKE_TICKS;
        let phase = self.phase;
        let state = self.state_byte;
        self.phase = Phase::Ready;
        self.outgoing.clear();
        self.outgoing_pos = 0;
        if reply == REPLY_OK {
            match (phase, state) {
                (Phase::Ready, _) => {
                    self.incoming.clear();
                    self.phase = Phase::Command;
                }
                (Phase::Command, STATE_READ) => {
                    self.read_block();
                    release = BLOCK_TICKS;
                }
                (Phase::Command, STATE_WRITE) => {
                    self.write_target = self.command_block();
                    self.incoming.clear();
                    self.phase = Phase::WriteData;
                }
                (Phase::WriteData, _) => {
                    self.write_block();
                    release = BLOCK_TICKS;
                }
                _ => (),
            }
        }
        self.busy_at = Some(self.now + release);
    }

    fn command_block(&self) -> usize {
        self.incoming[1..4]
            .iter()
            .fold(0, |block, &b| (block << 8) | usize::from(b))
    }

    fn block_count(&self) -> usize {
        self.data.len() / BLOCK_SIZE
    }

    fn device_block(&self) -> [u8; BLOCK_SIZE] {
        let blocks = self.block_count();
        let mut block = [0; BLOCK_SIZE];
        let (name, device): (&[u8; 13], u8) = if blocks > BLOCKS_5MB {
            (b"PROFILE 10M  ", 0x10)
        } else {
            (b"PROFILE      ", 0x00)
        };
        block[..13].copy_from_slice(name);
        block[15] = device;
        // Firmware revision
        block[16] = 0x03;
        block[17] = 0x98;
        block[18] = (blocks >> 16) as u8;
        block[19] = (blocks >> 8) as u8;
        block[20] = blocks as u8;
        // Bytes per block
        block[21] = (BLOCK_SIZE >> 8) as u8;
        block[22] = BLOCK_SIZE as u8;
        // Spare blocks
        block[23] = 0x20;
        block
    }

    fn read_block(&mut self) {
        let block = self.command_block();
        self.outgoing.clear();
        self.outgoing_pos = 0;
        if block == DEVICE_BLOCK {
            let device_block = self.device_block();
            self.outgoing.extend_from_slice(&[0; STATUS_SIZE]);
            self.outgoing.extend_from_slice(&device_block);
        } else if block < self.block_count() {
            self.outgoing.extend_from_slice(&[0; STATUS_SIZE]);
            self.outgoing
                .extend_from_slice(&self.data[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE]);
        } else {
            log::warn!("ProFile read past the end: block {:06X}", block);
            self.outgoing
                .extend_from_slice(&[STATUS_BAD_BLOCK, 0, 0, 0]);
            self.outgoing.resize(STATUS_SIZE + BLOCK_SIZE, 0);
        }
    }

    fn write_block(&mut self) {
        let block = self.write_target;
        let mut status = 0;
        if block < self.block_count() && self.incoming.len() == BLOCK_SIZE {
            let at = block * BLOCK_SIZE;
            self.data[at..at + BLOCK_SIZE].copy_from_slice(&self.incoming);
            if let Err(e) = self.write_to_file(at) {
                log::error!("Writing the ProFile image: {:#}", e);
            }
        } else {
            log::warn!(
                "ProFile write of {} bytes to block {:06X}",
                self.incoming.len(),
                block
            );
            status = STATUS_BAD_BLOCK;
        }
        self.incoming.clear();
        self.outgoing.clear();
        self.outgoing.extend_from_slice(&[status, 0, 0, 0]);
        self.outgoing_pos = 0;
    }

    fn write_to_file(&mut self, at: usize) -> Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.seek(SeekFrom::Start(at as u64))?;
            file.write_all(&self.data[at..at + BLOCK_SIZE])?;
        }
        Ok(())
    }

    /// The host read a byte with the strobe
    pub fn strobe_read(&mut self) {
        if self.outgoing_pos < self.outgoing.len() {
            self.outgoing_pos += 1;
        }
    }

    /// The host wrote a byte with the strobe
    pub fn strobe_write(&mut self, val: u8) {
        match self.phase {
            Phase::Command if self.incoming.len() < COMMAND_SIZE => self.incoming.push(val),
            Phase::WriteData if self.incoming.len() < BLOCK_SIZE => self.incoming.push(val),
            _ => (),
        }
    }
}

/// The HardDisk function of the drive
impl HardDisk for ProfileSim {
    fn update_status(&mut self, status: &mut HardDiskStatus) -> bool {
        let new = HardDiskStatus {
            settings: HardDiskSettings {
                media: self.path.clone().unwrap_or_default(),
                read_only: !self.writable,
            },
            capacity: Some(self.block_count() * BLOCK_SIZE),
            can_branch: false,
        };
        replace_status(status, new)
    }

    fn branch(&mut self, _path: &Path) -> Result<()> {
        bail!("A Lisa hard disk's image can't be branched off yet")
    }
}

fn create_profile(config: &DeviceConfig) -> Result<ProfileSim> {
    let settings: HardDiskSettings = config.parse()?;
    ProfileSim::open(&settings.media, !settings.read_only)
}

fn fits_profile(mac: MacModel) -> bool {
    mac == MacModel::Lisa25
}

pub const INFO_PROFILE: ModelInfo = ModelInfo {
    model: DeviceModel::ProFile,
    name: "ProFile",
    bus: BusKind::LisaParallel,
    fits: fits_profile,
    function: FunctionKind::HardDisk,
    fixed: false,
    one_per_bus: false,
    rom: None,
    path_keys: &["media"],
    adds_buses: &[],
    create: Some(CreateFn::LisaParallel(create_profile)),
};

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(blocks: usize) -> ProfileSim {
        let mut drive = ProfileSim::new();
        drive.data = vec![0; blocks * BLOCK_SIZE];
        drive.data[BLOCK_SIZE] = 0xAB;
        drive
    }

    /// Expects a state byte and answers OK
    fn handshake(drive: &mut ProfileSim, state: u8) {
        drive.set_cmd(true, 0xFF);
        drive.tick(HANDSHAKE_TICKS);
        assert!(!drive.not_busy());
        assert_eq!(drive.port(), state);
        drive.set_cmd(false, REPLY_OK);
        drive.tick(BLOCK_TICKS);
        assert!(drive.not_busy());
    }

    fn command(drive: &mut ProfileSim, cmd: u8, block: u32) {
        handshake(drive, STATE_READY);
        for b in [
            cmd,
            (block >> 16) as u8,
            (block >> 8) as u8,
            block as u8,
            10,
            3,
        ] {
            drive.strobe_write(b);
        }
    }

    fn read_bytes(drive: &mut ProfileSim, count: usize) -> Vec<u8> {
        (0..count)
            .map(|_| {
                let b = drive.port();
                drive.strobe_read();
                b
            })
            .collect()
    }

    #[test]
    fn read() {
        let mut drive = drive(4);
        command(&mut drive, CMD_READ, 1);
        handshake(&mut drive, STATE_READ);
        assert_eq!(read_bytes(&mut drive, STATUS_SIZE), [0; STATUS_SIZE]);
        assert_eq!(read_bytes(&mut drive, 1), [0xAB]);
    }

    #[test]
    fn write() {
        let mut drive = drive(4);
        command(&mut drive, CMD_WRITE, 2);
        handshake(&mut drive, STATE_WRITE);
        for i in 0..BLOCK_SIZE {
            drive.strobe_write(i as u8);
        }
        handshake(&mut drive, STATE_WRITTEN);
        assert_eq!(read_bytes(&mut drive, STATUS_SIZE), [0; STATUS_SIZE]);
        assert_eq!(drive.data[2 * BLOCK_SIZE + 5], 5);
        // Ready for the next one
        handshake(&mut drive, STATE_READY);
    }

    #[test]
    fn device_block() {
        let mut drive = drive(BLOCKS_5MB);
        command(&mut drive, CMD_READ, DEVICE_BLOCK as u32);
        handshake(&mut drive, STATE_READ);
        let bytes = read_bytes(&mut drive, STATUS_SIZE + BLOCK_SIZE);
        let block = &bytes[STATUS_SIZE..];
        assert_eq!(&block[..7], b"PROFILE");
        assert_eq!(block[14], 0);
        assert_eq!(&block[18..21], [0x00, 0x26, 0x00]);
    }

    #[test]
    fn past_the_end() {
        let mut drive = drive(4);
        command(&mut drive, CMD_READ, 4);
        handshake(&mut drive, STATE_READ);
        assert_eq!(read_bytes(&mut drive, 1), [STATUS_BAD_BLOCK]);
    }

    #[test]
    fn bad_reply_goes_back_to_ready() {
        let mut drive = drive(4);
        drive.set_cmd(true, 0xFF);
        drive.tick(HANDSHAKE_TICKS);
        drive.set_cmd(false, 0x00);
        drive.tick(HANDSHAKE_TICKS);
        assert!(drive.not_busy());
        handshake(&mut drive, STATE_READY);
    }
}
