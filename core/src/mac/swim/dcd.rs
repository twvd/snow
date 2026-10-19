//! Directly Connected Disk (DCD) - the Apple HD20 hard disk on the floppy port
//!
//! A DCD sits on the external drive port (!ENBL2) and talks to the Mac through the IWM
//! as a byte stream at the floppy bit rate. CA0-CA2 form a 3-bit state that carries the
//! handshake, the SENSE line carries the device's /HSHK, and LSTRB (CA3) steps along a
//! daisy chain. The 128K ROM (512Ke, Plus) and later carry the host driver in .Sony.
//!
//! See docs/HD20.md for the protocol and its sources.

use std::collections::VecDeque;
use std::path::Path;

use anyhow::{Result, bail};
use log::*;
use serde::{Deserialize, Serialize};

use crate::debuggable::Debuggable;
use crate::mac::scsi::disk_image::{DiskImage, FileDiskImage};
use crate::tickable::Ticks;
use crate::{dbgprop_bool, dbgprop_byte, dbgprop_enum, dbgprop_udec};

/// Block size of the volume (without tags)
pub const DCD_BLOCKSIZE: usize = 512;

/// Tag bytes sent along with each block
const TAG_SIZE: usize = 20;

/// Sync byte that starts every transfer, in either direction
const SYNC: u8 = 0xAA;

/// C8M (7.8336 MHz) clocks per byte on the wire: 8 bit cells of 16 clocks (2.04us) each
pub const TICKS_PER_BYTE: Ticks = 128;

/// C8M, the clock [`TICKS_PER_BYTE`] counts
const C8M: Ticks = 7_833_600;

/// Bus clocks per byte on the wire for a bus running at `bus_frequency`.
///
/// The wire rate is fixed in time, so a faster bus (the Portable and the Mac II
/// family run at C16M) takes more of its clocks per byte.
pub fn ticks_per_byte(bus_frequency: Ticks) -> Ticks {
    TICKS_PER_BYTE * bus_frequency / C8M
}

/// Blocks addressable by the 24-bit block numbers of the protocol
const MAX_BLOCKS: usize = 0xFF_FFFF;

/// Characteristics byte of the Controller Status reply
mod chars {
    pub const MOUNTABLE: u8 = 0x80;
    pub const READABLE: u8 = 0x40;
    pub const WRITABLE: u8 = 0x20;
    pub const WRITE_PROTECTED: u8 = 0x08;
    pub const ICON_INCLUDED: u8 = 0x04;
    pub const DISK_IN_PLACE: u8 = 0x02;
}

/// Commands from the Mac
mod cmd {
    pub const READ: u8 = 0x00;
    pub const WRITE: u8 = 0x01;
    pub const WRITE_VERIFY: u8 = 0x02;
    pub const STATUS: u8 = 0x03;
    pub const FORMAT: u8 = 0x19;
    pub const VERIFY_FORMAT: u8 = 0x1A;
    /// Set on write continuation blocks
    pub const CONTINUATION: u8 = 0x40;
    /// Response to a command whose checksum didn't add up
    pub const NAK: u8 = 0x7F;
}

/// First status byte for a failed operation. The 128K ROM's .Sony and the HD20's own
/// firmware use bit 0 of this byte for "operation failed" (the ROM also fails on $0A);
/// the 1985 spec says bit 7. Set both.
const STATUS_FAILED: u8 = 0x81;

/// Encodes one group of 7 payload bytes as the device sends it: the 7 bytes shifted
/// right by one with the MSB set, followed by a byte holding the 7 LSBs (the first
/// byte's in bit 0).
pub fn encode_group(payload: &[u8; 7]) -> [u8; 8] {
    let mut out = [0; 8];
    let mut lsbs = 0x80;
    for (i, &b) in payload.iter().enumerate() {
        out[i] = 0x80 | (b >> 1);
        lsbs |= (b & 1) << i;
    }
    out[7] = lsbs;
    out
}

/// Decodes one group as the Mac sends it: the LSB byte first, then the 7 shifted bytes.
pub fn decode_group(wire: &[u8; 8]) -> [u8; 7] {
    let lsbs = wire[0];
    core::array::from_fn(|i| ((wire[i + 1] & 0x7F) << 1) | ((lsbs >> i) & 1))
}

/// Pads (or cuts) a payload to `groups` groups of 7 bytes and sets the last byte so
/// that all bytes add up to zero (mod 256).
pub fn seal_payload(mut payload: Vec<u8>, groups: usize) -> Vec<u8> {
    let len = (groups * 7).max(7);
    payload.resize(len - 1, 0);
    let sum = payload.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    payload.push(sum.wrapping_neg());
    payload
}

/// True if all bytes of a payload add up to zero (mod 256)
pub fn checksum_ok(payload: &[u8]) -> bool {
    payload.iter().fold(0u8, |a, &b| a.wrapping_add(b)) == 0
}

/// Finds the volume inside a disk image: the first Apple_HFS partition of an image with
/// an Apple partition map, or else the whole image.
/// Returns (first block, block count).
pub fn find_volume(image: &dyn DiskImage) -> (usize, usize) {
    let total = image.byte_len() / DCD_BLOCKSIZE;
    if total < 2 || image.read_bytes(0, 2) != b"ER" {
        return (0, total);
    }

    let mut entry = 1;
    let mut entries = 1;
    while entry <= entries && entry < total {
        let pm = image.read_bytes(entry * DCD_BLOCKSIZE, DCD_BLOCKSIZE);
        if &pm[0..2] != b"PM" {
            break;
        }
        let be32 = |o: usize| u32::from_be_bytes(pm[o..o + 4].try_into().unwrap()) as usize;
        entries = be32(4);
        let (start, count) = (be32(8), be32(12));
        let ptype = pm[48..80].split(|&c| c == 0).next().unwrap_or_default();
        if ptype == b"Apple_HFS" && start < total {
            return (start, count.min(total - start));
        }
        entry += 1;
    }

    warn!("HD20: partition map without an Apple_HFS partition, using the whole image");
    (0, total)
}

/// The 32x32 icon and mask shown for the drive (a plain hard disk box)
fn drive_icon() -> ([u8; 128], [u8; 128]) {
    let mut icon = [0u8; 128];
    let mut mask = [0u8; 128];
    let set = |bm: &mut [u8; 128], x: usize, y: usize| bm[y * 4 + x / 8] |= 0x80 >> (x % 8);

    for y in 9..=23 {
        for x in 1..=30 {
            set(&mut mask, x, y);
            let edge = y == 9 || y == 22 || y == 23 || x == 1 || x == 30;
            let slot = y == 15 && (4..=19).contains(&x);
            let led = y == 18 && (25..=27).contains(&x);
            if edge || slot || led {
                set(&mut icon, x, y);
            }
        }
    }
    (icon, mask)
}

/// Where the device is in the conversation with the Mac
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::IntoStaticStr)]
enum Phase {
    /// Nothing going on, /HSHK deasserted
    Idle,
    /// Mac asked to send (state 3), /HSHK asserted, waiting for state 1
    RxRequested,
    /// Receiving a command (states 1 and 0)
    Rx,
    /// Mac ended the command (state 3), /HSHK deasserted, waiting for state 2
    RxDone,
    /// A response is ready, /HSHK asserted, waiting for the Mac to read it (state 1)
    TxPending,
    /// Sending a response
    Tx,
    /// Response sent, /HSHK deasserted, waiting for state 2
    TxDone,
}

/// Where the receiver is in a command transfer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum RxStep {
    /// Waiting for the sync byte
    Sync,
    /// Waiting for the count of groups the Mac sends
    SendCount,
    /// Waiting for the count of groups the Mac expects back
    ReplyCount,
    /// Receiving groups
    Groups,
    /// Holdoff: the group is complete, waiting for state 1 and a new sync byte
    Holdoff,
    /// Waiting for the sync byte after a holdoff
    ResumeSync,
    /// All groups received
    Complete,
}

/// An HD20-compatible Directly Connected Disk
#[derive(Serialize, Deserialize)]
pub struct DcdDevice {
    #[serde(skip)]
    backend: Option<Box<dyn DiskImage>>,
    /// Size of the whole image file in bytes, for save states
    image_len: usize,

    /// First block of the volume in the image
    first_block: usize,
    /// Size of the volume in blocks
    blocks: usize,
    writable: bool,

    phase: Phase,
    /// Last seen CA2:CA1:CA0 state
    state: u8,
    /// /HSHK asserted (SENSE reads 0 in states 2 and 3)
    hshk: bool,
    /// The Mac signalled holdoff (state 0) during the current group
    holdoff: bool,

    rx_step: RxStep,
    rx_groups_expected: usize,
    rx_groups_reply: usize,
    rx_group: Vec<u8>,
    rx_payload: Vec<u8>,

    /// Responses waiting to be sent, as payloads including the checksum
    responses: VecDeque<Vec<u8>>,
    /// Response being sent, encoded
    tx_wire: Vec<u8>,
    tx_pos: usize,
    tx_sync: bool,
    tx_paused: bool,
    tx_ticks: Ticks,

    /// Block a write continuation goes to
    next_write_block: usize,
}

impl DcdDevice {
    /// Opens a disk image as a DCD. Images with an Apple partition map get their first
    /// Apple_HFS partition presented as the volume.
    pub fn open(path: &Path, writable: bool) -> Result<Self> {
        let image = FileDiskImage::open(path, writable)?;
        Self::new(Box::new(image), writable)
    }

    pub fn new(image: Box<dyn DiskImage>, writable: bool) -> Result<Self> {
        if !image.byte_len().is_multiple_of(DCD_BLOCKSIZE) {
            bail!(
                "HD20 image size is not a multiple of {} bytes",
                DCD_BLOCKSIZE
            );
        }
        let (first_block, blocks) = find_volume(image.as_ref());
        if blocks == 0 {
            bail!("HD20 image holds no blocks");
        }
        if blocks > MAX_BLOCKS {
            bail!("HD20 volume too large ({} blocks)", blocks);
        }
        info!(
            "HD20: volume at block {}, {} blocks ({} MB){}",
            first_block,
            blocks,
            blocks * DCD_BLOCKSIZE / (1024 * 1024),
            if writable { "" } else { ", read-only" }
        );

        Ok(Self {
            image_len: image.byte_len(),
            backend: Some(image),
            first_block,
            blocks,
            writable,
            phase: Phase::Idle,
            state: 7,
            hshk: false,
            holdoff: false,
            rx_step: RxStep::Sync,
            rx_groups_expected: 0,
            rx_groups_reply: 0,
            rx_group: Vec::with_capacity(8),
            rx_payload: Vec::new(),
            responses: VecDeque::new(),
            tx_wire: Vec::new(),
            tx_pos: 0,
            tx_sync: false,
            tx_paused: false,
            tx_ticks: 0,
            next_write_block: 0,
        })
    }

    /// Size of the presented volume in blocks
    pub fn blocks(&self) -> usize {
        self.blocks
    }

    /// False if the Mac can't write to the disk
    pub fn writable(&self) -> bool {
        self.writable
    }

    /// File the image was loaded from
    pub fn image_path(&self) -> Option<&Path> {
        self.backend.as_ref().and_then(|b| b.image_path())
    }

    /// Re-attaches the image after loading a save state
    #[cfg(feature = "savestates")]
    pub fn after_deserialize(&mut self, imgfn: &Path) -> Result<()> {
        let image = FileDiskImage::open(imgfn, true)?;
        if image.byte_len() != self.image_len {
            bail!(
                "HD20 image in save state is {} bytes, expected {}",
                image.byte_len(),
                self.image_len
            );
        }
        self.backend = Some(Box::new(image));
        Ok(())
    }

    /// Size of the image to store in a save state
    #[cfg(feature = "savestates")]
    pub fn savestate_img_len(&self) -> usize {
        self.image_len
    }

    /// Contents of the image to store in a save state
    #[cfg(feature = "savestates")]
    pub fn savestate_img_data(&self) -> Option<&[u8]> {
        self.backend.as_ref().and_then(|b| b.media_bytes())
    }

    /// Back to power-on state
    pub fn reset(&mut self) {
        self.phase = Phase::Idle;
        self.hshk = false;
        self.holdoff = false;
        self.rx_step = RxStep::Sync;
        self.rx_group.clear();
        self.rx_payload.clear();
        self.responses.clear();
        self.tx_wire.clear();
        self.tx_paused = false;
    }

    /// SENSE (RD) as seen by the Mac in the given CA2:CA1:CA0 state
    pub fn sense(&self, state: u8) -> bool {
        match state & 7 {
            // Data
            0 | 1 => true,
            // /HSHK
            2 | 3 => !self.hshk,
            // Reset, ID
            4 | 5 => false,
            // ID
            _ => true,
        }
    }

    /// Phase lines changed while the device is selected
    pub fn set_state(&mut self, state: u8) {
        let state = state & 7;
        if state == self.state {
            return;
        }
        self.state = state;

        if state == 4 {
            if self.phase != Phase::Idle {
                debug!("HD20: reset during {:?}", self.phase);
            }
            self.reset();
            return;
        }
        if state == 0 {
            self.holdoff = true;
        }

        match (self.phase, state) {
            (Phase::Idle, 3) => {
                self.hshk = true;
                self.phase = Phase::RxRequested;
            }
            (Phase::RxRequested, 1) => {
                self.phase = Phase::Rx;
                self.rx_step = RxStep::Sync;
                self.rx_group.clear();
                self.rx_payload.clear();
                self.holdoff = false;
            }
            (Phase::RxRequested, 3) => (),
            (Phase::RxRequested, _) => {
                self.hshk = false;
                self.phase = Phase::Idle;
            }
            (Phase::Rx, 1) => {
                if self.rx_step == RxStep::Holdoff {
                    self.rx_step = RxStep::ResumeSync;
                }
            }
            (Phase::Rx, 0) => (),
            (Phase::Rx, 3) => {
                self.hshk = false;
                self.phase = Phase::RxDone;
            }
            (Phase::Rx, _) => {
                warn!("HD20: command aborted in state {}", state);
                self.reset();
            }
            (Phase::RxDone, 2) => {
                self.process_command();
                self.next_response();
            }
            (Phase::TxPending, 1) => {
                self.phase = Phase::Tx;
                self.start_tx();
            }
            (Phase::Tx, 1) if self.tx_paused => {
                // Resume after holdoff
                self.tx_paused = false;
                self.tx_sync = true;
                self.tx_ticks = 0;
            }
            (Phase::Tx, 0 | 1) => (),
            (Phase::Tx, _) => {
                warn!("HD20: response aborted in state {}", state);
                self.reset();
            }
            (Phase::TxDone, 2) => self.next_response(),
            _ => (),
        }
    }

    /// Asserts /HSHK for the next queued response, if any
    fn next_response(&mut self) {
        if self.responses.is_empty() {
            self.hshk = false;
            self.phase = Phase::Idle;
        } else {
            self.hshk = true;
            self.phase = Phase::TxPending;
        }
    }

    fn start_tx(&mut self) {
        let payload = self.responses.pop_front().unwrap_or_default();
        self.tx_wire = payload
            .chunks_exact(7)
            .flat_map(|g| encode_group(g.try_into().unwrap()))
            .collect();
        self.tx_pos = 0;
        self.tx_sync = true;
        self.tx_paused = false;
        self.tx_ticks = 0;
        self.holdoff = false;
    }

    /// Advances time by `ticks` bus clocks, of which a byte takes `ticks_per_byte`;
    /// returns a byte that arrived in the IWM data register, if any
    pub fn tick(&mut self, ticks: Ticks, ticks_per_byte: Ticks) -> Option<u8> {
        if self.phase != Phase::Tx || self.tx_paused {
            return None;
        }
        self.tx_ticks += ticks;
        if self.tx_ticks < ticks_per_byte {
            return None;
        }
        self.tx_ticks -= ticks_per_byte;

        if self.tx_sync {
            self.tx_sync = false;
            return Some(SYNC);
        }

        let byte = self.tx_wire[self.tx_pos];
        self.tx_pos += 1;
        if self.tx_pos == self.tx_wire.len() {
            // Last byte latched: end of transmission
            self.hshk = false;
            self.phase = Phase::TxDone;
        } else if self.tx_pos.is_multiple_of(8) && self.holdoff {
            // Group complete and the Mac asked for holdoff during it
            trace!("HD20: holdoff while sending");
            self.holdoff = false;
            if self.state == 1 {
                self.tx_sync = true;
                self.tx_ticks = 0;
            } else {
                self.tx_paused = true;
            }
        }
        Some(byte)
    }

    /// A byte left the IWM write shifter
    pub fn write_byte(&mut self, byte: u8) {
        if self.phase != Phase::Rx || byte & 0x80 == 0 {
            // Not receiving, or a flush byte
            return;
        }

        match self.rx_step {
            RxStep::Sync | RxStep::ResumeSync if byte != SYNC => {
                debug!("HD20: expected sync, got {:02X}", byte);
            }
            RxStep::Sync => self.rx_step = RxStep::SendCount,
            RxStep::ResumeSync => self.rx_step = RxStep::Groups,
            RxStep::SendCount => {
                self.rx_groups_expected = (byte & 0x7F) as usize;
                self.rx_step = RxStep::ReplyCount;
            }
            RxStep::ReplyCount => {
                self.rx_groups_reply = (byte & 0x7F) as usize;
                self.holdoff = false;
                self.rx_step = if self.rx_groups_expected == 0 {
                    RxStep::Complete
                } else {
                    RxStep::Groups
                };
            }
            RxStep::Groups => {
                self.rx_group.push(byte);
                if self.rx_group.len() < 8 {
                    return;
                }
                let group: [u8; 8] = self.rx_group[..].try_into().unwrap();
                self.rx_payload.extend_from_slice(&decode_group(&group));
                self.rx_group.clear();

                if self.rx_payload.len() == self.rx_groups_expected * 7 {
                    self.rx_step = RxStep::Complete;
                } else if self.holdoff {
                    trace!("HD20: holdoff while receiving");
                    self.holdoff = false;
                    self.rx_step = if self.state == 1 {
                        RxStep::ResumeSync
                    } else {
                        RxStep::Holdoff
                    };
                }
            }
            RxStep::Holdoff | RxStep::Complete => (),
        }
    }

    /// Builds the response(s) to a received command
    fn process_command(&mut self) {
        let payload = std::mem::take(&mut self.rx_payload);
        let reply_groups = self.rx_groups_reply;

        if self.rx_step != RxStep::Complete || !checksum_ok(&payload) {
            warn!(
                "HD20: bad command ({:?}, {} bytes), sending NAK",
                self.rx_step,
                payload.len()
            );
            self.responses
                .push_back(seal_payload(vec![cmd::NAK, 0, 0, 0, 0, 0], 1));
            return;
        }

        let command = payload[0];
        let count = payload[1];
        let block = u32::from_be_bytes([0, payload[2], payload[3], payload[4]]) as usize;

        match command {
            cmd::READ => {
                debug!("HD20: read {} blocks at {}", count, block);
                for i in 0..count {
                    let blk = block + usize::from(i);
                    let mut resp = vec![0x80 | cmd::READ, count - i];
                    let Some(data) = self.read_block(blk) else {
                        // The ROM stops at a failed block, so the reply ends there
                        resp.extend_from_slice(&[STATUS_FAILED, 0, 0, 0]);
                        self.responses.push_back(seal_payload(resp, reply_groups));
                        break;
                    };
                    resp.extend_from_slice(&[0; 4]);
                    resp.extend_from_slice(&[0; TAG_SIZE]);
                    resp.extend_from_slice(&data);
                    self.responses.push_back(seal_payload(resp, reply_groups));
                }
            }
            c if c & !cmd::CONTINUATION == cmd::WRITE
                || c & !cmd::CONTINUATION == cmd::WRITE_VERIFY =>
            {
                let blk = if c & cmd::CONTINUATION != 0 {
                    self.next_write_block
                } else {
                    block
                };
                self.next_write_block = blk + 1;
                debug!("HD20: write block {} ({} left)", blk, count);

                let data_start = 6 + TAG_SIZE;
                let ok = payload.len() >= data_start + DCD_BLOCKSIZE
                    && self.write_block(blk, &payload[data_start..data_start + DCD_BLOCKSIZE]);
                let status = if ok { 0 } else { STATUS_FAILED };
                let resp = vec![0x80 | (c & !cmd::CONTINUATION), count, status, 0, 0, 0];
                self.responses.push_back(seal_payload(resp, reply_groups));
            }
            cmd::STATUS => {
                debug!("HD20: controller status");
                self.responses
                    .push_back(seal_payload(self.status_reply(), reply_groups));
            }
            cmd::FORMAT | cmd::VERIFY_FORMAT => {
                debug!("HD20: format/verify ({:02X})", command);
                self.responses
                    .push_back(seal_payload(vec![0x80 | command, 0], reply_groups));
            }
            _ => {
                warn!("HD20: unsupported command {:02X}", command);
                self.responses.push_back(seal_payload(
                    vec![0x80 | command, count, STATUS_FAILED],
                    reply_groups,
                ));
            }
        }
    }

    /// Controller Status reply (without checksum)
    fn status_reply(&self) -> Vec<u8> {
        let mut characteristics =
            chars::MOUNTABLE | chars::READABLE | chars::ICON_INCLUDED | chars::DISK_IN_PLACE;
        if self.writable {
            characteristics |= chars::WRITABLE;
        } else {
            characteristics |= chars::WRITE_PROTECTED;
        }

        let mut r = vec![0x80 | cmd::STATUS, 0, 0, 0, 0, 0];
        // Device type, manufacturer (as the HD20)
        r.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        r.push(characteristics);
        r.extend_from_slice(&(self.blocks as u32).to_be_bytes()[1..]);
        // Spare blocks, bad blocks, manufacturer reserved
        r.resize(70, 0);
        let (icon, mask) = drive_icon();
        r.extend_from_slice(&icon);
        r.extend_from_slice(&mask);
        // "Where:" string for Get Info
        let place = b"Snow HD20";
        r.push(place.len() as u8);
        r.extend_from_slice(place);
        r.resize(342, 0);
        r
    }

    fn read_block(&self, block: usize) -> Option<Vec<u8>> {
        let backend = self.backend.as_deref()?;
        if block >= self.blocks {
            warn!("HD20: read past end of volume (block {})", block);
            return None;
        }
        Some(backend.read_bytes((self.first_block + block) * DCD_BLOCKSIZE, DCD_BLOCKSIZE))
    }

    fn write_block(&mut self, block: usize, data: &[u8]) -> bool {
        if !self.writable {
            return false;
        }
        let Some(backend) = self.backend.as_deref_mut() else {
            return false;
        };
        if block >= self.blocks {
            warn!("HD20: write past end of volume (block {})", block);
            return false;
        }
        backend.write_bytes((self.first_block + block) * DCD_BLOCKSIZE, data);
        true
    }
}

impl Debuggable for DcdDevice {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;

        vec![
            dbgprop_enum!("Phase", self.phase),
            dbgprop_byte!("State", self.state),
            dbgprop_bool!("/HSHK asserted", self.hshk),
            dbgprop_bool!("Holdoff", self.holdoff),
            dbgprop_udec!("Blocks", self.blocks),
            dbgprop_udec!("First block", self.first_block),
            dbgprop_udec!("Queued responses", self.responses.len()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory disk image
    struct MemImage(Vec<u8>);

    impl DiskImage for MemImage {
        fn byte_len(&self) -> usize {
            self.0.len()
        }
        fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8> {
            self.0[offset..offset + length].to_vec()
        }
        fn write_bytes(&mut self, offset: usize, data: &[u8]) {
            self.0[offset..offset + data.len()].copy_from_slice(data);
        }
        fn media_bytes(&self) -> Option<&[u8]> {
            Some(&self.0)
        }
        fn image_path(&self) -> Option<&Path> {
            None
        }
    }

    /// Wire bytes of a Mac-to-device transfer of `payload`
    fn mac_wire(payload: &[u8], reply_groups: usize) -> Vec<u8> {
        let mut wire = vec![
            SYNC,
            0x80 | (payload.len() / 7) as u8,
            0x80 | reply_groups as u8,
        ];
        for g in payload.chunks_exact(7) {
            let enc = encode_group(g.try_into().unwrap());
            // The Mac sends the LSB byte first
            wire.push(enc[7]);
            wire.extend_from_slice(&enc[..7]);
        }
        wire
    }

    fn device(blocks: usize) -> DcdDevice {
        let data = (0..blocks * DCD_BLOCKSIZE)
            .map(|i| (i / DCD_BLOCKSIZE) as u8 ^ i as u8)
            .collect();
        DcdDevice::new(Box::new(MemImage(data)), true).unwrap()
    }

    /// Runs a command through the handshake and returns the decoded responses
    fn transact(dev: &mut DcdDevice, payload: &[u8], reply_groups: usize) -> Vec<Vec<u8>> {
        // Idle check, request
        dev.set_state(2);
        assert!(dev.sense(2), "/HSHK low before starting");
        dev.set_state(3);
        assert!(!dev.sense(3), "no /HSHK for request");
        dev.set_state(1);
        for b in mac_wire(payload, reply_groups) {
            dev.write_byte(b);
        }
        dev.write_byte(0);
        dev.write_byte(0);
        dev.set_state(3);
        assert!(dev.sense(3), "/HSHK not released after command");
        dev.set_state(2);

        let mut responses = vec![];
        while !dev.sense(2) {
            dev.set_state(3);
            dev.set_state(1);
            let mut wire = vec![];
            for _ in 0..100_000 {
                if let Some(b) = dev.tick(1, TICKS_PER_BYTE) {
                    wire.push(b);
                }
                if dev.phase == Phase::TxDone {
                    break;
                }
            }
            assert_eq!(wire[0], SYNC);
            let mut resp = vec![];
            for g in wire[1..].chunks_exact(8) {
                // The device sends the LSB byte last
                let mut mac_order = [0; 8];
                mac_order[0] = g[7];
                mac_order[1..].copy_from_slice(&g[..7]);
                resp.extend_from_slice(&decode_group(&mac_order));
            }
            dev.set_state(3);
            assert!(dev.sense(3), "/HSHK still low at end of response");
            dev.set_state(2);
            responses.push(resp);
        }
        responses
    }

    #[test]
    fn byte_time_follows_bus_frequency() {
        // 16.34us a byte: 128 clocks of C8M on the compact Macs, 256 of C16M on the
        // Portable and the Mac II family
        assert_eq!(ticks_per_byte(7_833_600), 128);
        assert_eq!(ticks_per_byte(15_667_200), 256);
    }

    #[test]
    fn group_roundtrip() {
        let p = [0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37];
        let enc = encode_group(&p);
        assert_eq!(enc, [0x98, 0x99, 0x99, 0x9A, 0x9A, 0x9B, 0x9B, 0xD5]);
        let mut mac = [0; 8];
        mac[0] = enc[7];
        mac[1..].copy_from_slice(&enc[..7]);
        assert_eq!(decode_group(&mac), p);

        // LSB of the first byte goes in bit 0
        assert_eq!(encode_group(&[1, 0, 0, 0, 0, 0, 0])[7], 0x81);
        assert_eq!(encode_group(&[0, 0, 0, 0, 0, 0, 1])[7], 0xC0);
    }

    #[test]
    fn wire_vectors() {
        // Status command, as captured by BMOW from a real Mac
        let status = seal_payload(vec![0x03, 0, 0, 0, 0, 0], 1);
        assert_eq!(status, [0x03, 0, 0, 0, 0, 0, 0xFD]);
        assert_eq!(
            mac_wire(&status, 49),
            [
                0xAA, 0x81, 0xB1, 0xC1, 0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0xFE
            ]
        );

        let read = seal_payload(vec![0x00, 0x02, 0x01, 0x23, 0x45, 0x00], 1);
        assert_eq!(read, [0x00, 0x02, 0x01, 0x23, 0x45, 0x00, 0x95]);
        assert_eq!(
            mac_wire(&read, 77),
            [
                0xAA, 0x81, 0xCD, 0xDC, 0x80, 0x81, 0x80, 0x91, 0xA2, 0x80, 0xCA
            ]
        );

        // Write response and NAK, device order
        assert_eq!(
            encode_group(
                &seal_payload(vec![0x81, 0x01, 0, 0, 0, 0], 1)
                    .try_into()
                    .unwrap()
            ),
            [0xC0, 0x80, 0x80, 0x80, 0x80, 0x80, 0xBF, 0x83]
        );
        assert_eq!(
            encode_group(
                &seal_payload(vec![0x7F, 0, 0, 0, 0, 0], 1)
                    .try_into()
                    .unwrap()
            ),
            [0xBF, 0x80, 0x80, 0x80, 0x80, 0x80, 0xC0, 0xC1]
        );
    }

    #[test]
    fn detection_senses() {
        let dev = device(16);
        assert!(dev.sense(7));
        assert!(dev.sense(6));
        assert!(!dev.sense(5));
        // Idle: /HSHK deasserted means a disk is in place
        assert!(dev.sense(2));
    }

    #[test]
    fn status() {
        let mut dev = device(1234);
        let resp = transact(&mut dev, &seal_payload(vec![0x03, 0, 0, 0, 0, 0], 1), 49);
        assert_eq!(resp.len(), 1);
        let r = &resp[0];
        assert_eq!(r.len(), 343);
        assert!(checksum_ok(r));
        assert_eq!(r[0], 0x83);
        assert_eq!(&r[2..6], &[0; 4]);
        assert_eq!(r[10], 0xE6);
        assert_eq!(&r[11..14], &[0x00, 0x04, 0xD2]);
        assert_eq!(r[326] as usize, b"Snow HD20".len());
        assert_eq!(dev.phase, Phase::Idle);
    }

    #[test]
    fn read_multiple() {
        let mut dev = device(16);
        let resp = transact(&mut dev, &seal_payload(vec![0x00, 3, 0, 0, 5, 0], 1), 77);
        assert_eq!(resp.len(), 3);
        for (i, r) in resp.iter().enumerate() {
            assert_eq!(r.len(), 539);
            assert!(checksum_ok(r));
            assert_eq!(r[0], 0x80);
            assert_eq!(r[1] as usize, 3 - i);
            assert_eq!(&r[2..6], &[0; 4]);
            let blk = 5 + i;
            let expected: Vec<u8> = (0..DCD_BLOCKSIZE)
                .map(|j| blk as u8 ^ (blk * DCD_BLOCKSIZE + j) as u8)
                .collect();
            assert_eq!(&r[26..538], &expected[..]);
        }
    }

    #[test]
    fn read_past_end_fails() {
        let mut dev = device(4);
        let resp = transact(&mut dev, &seal_payload(vec![0x00, 1, 0, 0, 4, 0], 1), 77);
        assert_eq!(resp[0][2], STATUS_FAILED);
        assert!(checksum_ok(&resp[0]));
    }

    #[test]
    fn read_stops_at_failed_block() {
        let mut dev = device(4);
        let resp = transact(&mut dev, &seal_payload(vec![0x00, 3, 0, 0, 3, 0], 1), 77);
        assert_eq!(resp.len(), 2);
        assert_eq!(&resp[0][..3], &[0x80, 3, 0]);
        assert_eq!(&resp[1][..3], &[0x80, 2, STATUS_FAILED]);
        assert_eq!(resp[1].len(), 539);
        assert!(checksum_ok(&resp[1]));
    }

    #[test]
    fn write_protected() {
        let data = vec![0x55; 8 * DCD_BLOCKSIZE];
        let mut dev = DcdDevice::new(Box::new(MemImage(data)), false).unwrap();
        let r = &transact(&mut dev, &seal_payload(vec![0x03, 0, 0, 0, 0, 0], 1), 49)[0];
        assert_eq!(
            r[10],
            chars::MOUNTABLE
                | chars::READABLE
                | chars::WRITE_PROTECTED
                | chars::ICON_INCLUDED
                | chars::DISK_IN_PLACE
        );

        let mut p = vec![0x01, 1, 0, 0, 2, 0];
        p.extend_from_slice(&[0; TAG_SIZE]);
        p.extend(std::iter::repeat_n(0xA0, DCD_BLOCKSIZE));
        let resp = transact(&mut dev, &seal_payload(p, 77), 1);
        assert_eq!(&resp[0][..3], &[0x81, 1, STATUS_FAILED]);
        assert_eq!(dev.read_block(2).unwrap(), vec![0x55; DCD_BLOCKSIZE]);
    }

    #[test]
    fn daisy_chain() {
        use crate::bus::{Address, BusMember};
        use crate::mac::swim::Swim;
        use crate::mac::swim::drive::DriveType;

        const LSTRBL: Address = 6 << 9;
        const LSTRBH: Address = 7 << 9;
        const DISABLE: Address = 8 << 9;
        const ENABLE: Address = 9 << 9;
        const EXTDRIVE: Address = 11 << 9;

        let mut swim = Swim::new(&[DriveType::GCR800K, DriveType::GCR800K], false, 16_000_000);
        swim.attach_dcd(device(4)).unwrap();
        swim.attach_dcd(device(8)).unwrap();
        swim.read(EXTDRIVE);
        swim.read(ENABLE);
        assert_eq!(swim.selected_dcd().unwrap().blocks(), 4);

        // One rising edge of LSTRB passes the enable on once, however often LSTRBH is hit
        for _ in 0..3 {
            swim.read(LSTRBH);
        }
        swim.read(LSTRBL);
        assert_eq!(swim.selected_dcd().unwrap().blocks(), 8);

        // Past the last HD20 the external floppy drive answers
        swim.read(LSTRBH);
        swim.read(LSTRBL);
        assert!(swim.selected_dcd().is_none());

        // Dropping the enable goes back to the head of the chain
        swim.read(DISABLE);
        swim.read(ENABLE);
        assert_eq!(swim.selected_dcd().unwrap().blocks(), 4);
    }

    #[test]
    fn write_with_continuation() {
        let mut dev = device(8);
        for (i, cmdbyte) in [(0usize, 0x01u8), (1, 0x41)] {
            let mut p = vec![cmdbyte, 2 - i as u8, 0, 0, 3, 0];
            p.extend_from_slice(&[0; TAG_SIZE]);
            p.extend(std::iter::repeat_n(0xA0 + i as u8, DCD_BLOCKSIZE));
            let resp = transact(&mut dev, &seal_payload(p, 77), 1);
            assert_eq!(resp.len(), 1);
            assert_eq!(&resp[0][..6], &[0x81, 2 - i as u8, 0, 0, 0, 0]);
        }
        assert_eq!(dev.read_block(3).unwrap(), vec![0xA0; DCD_BLOCKSIZE]);
        assert_eq!(dev.read_block(4).unwrap(), vec![0xA1; DCD_BLOCKSIZE]);
    }

    #[test]
    fn bad_checksum_naks() {
        let mut dev = device(4);
        let resp = transact(&mut dev, &[0x03, 0, 0, 0, 0, 0, 0x00], 49);
        assert_eq!(resp.len(), 1);
        assert_eq!(resp[0][0], cmd::NAK);
        assert_eq!(resp[0].len(), 7);
    }

    #[test]
    fn holdoff_receive() {
        let mut dev = device(4);
        let mut p = vec![0x01, 1, 0, 0, 1, 0];
        p.extend_from_slice(&[0; TAG_SIZE]);
        p.extend(std::iter::repeat_n(0x5A, DCD_BLOCKSIZE));
        let p = seal_payload(p, 77);
        let wire = mac_wire(&p, 1);

        dev.set_state(2);
        dev.set_state(3);
        dev.set_state(1);
        // Sync, counts, then the first two groups; holdoff after the 3rd byte of group 2
        for &b in &wire[..3 + 8 + 3] {
            dev.write_byte(b);
        }
        dev.set_state(0);
        for &b in &wire[3 + 8 + 3..3 + 16] {
            dev.write_byte(b);
        }
        dev.write_byte(0);
        dev.set_state(1);
        dev.write_byte(SYNC);
        for &b in &wire[3 + 16..] {
            dev.write_byte(b);
        }
        dev.set_state(3);
        dev.set_state(2);
        assert_eq!(dev.phase, Phase::TxPending);
        assert_eq!(dev.read_block(1).unwrap(), vec![0x5A; DCD_BLOCKSIZE]);
    }

    #[test]
    fn holdoff_send() {
        let mut dev = device(4);
        let mut p = seal_payload(vec![0x00, 1, 0, 0, 2, 0], 1);
        dev.set_state(2);
        dev.set_state(3);
        dev.set_state(1);
        for b in mac_wire(&p, 77) {
            dev.write_byte(b);
        }
        dev.set_state(3);
        dev.set_state(2);
        dev.set_state(3);
        dev.set_state(1);

        let mut wire = vec![];
        let mut ticks = 0;
        while dev.phase != Phase::TxDone {
            if let Some(b) = dev.tick(1, TICKS_PER_BYTE) {
                wire.push(b);
                // Holdoff after the 4th byte of the 3rd group (sync + 2 groups + 4)
                if wire.len() == 1 + 16 + 4 {
                    dev.set_state(0);
                }
            }
            ticks += 1;
            if dev.tx_paused {
                assert_eq!(wire.len(), 1 + 24);
                // Nothing arrives during holdoff
                for _ in 0..1000 {
                    assert_eq!(dev.tick(1, TICKS_PER_BYTE), None);
                }
                dev.set_state(1);
            }
            assert!(ticks < 1_000_000);
        }
        assert_eq!(wire[1 + 24], SYNC);
        wire.remove(1 + 24);
        p.clear();
        for g in wire[1..].chunks_exact(8) {
            let mut mac_order = [0; 8];
            mac_order[0] = g[7];
            mac_order[1..].copy_from_slice(&g[..7]);
            p.extend_from_slice(&decode_group(&mac_order));
        }
        assert_eq!(p.len(), 539);
        assert!(checksum_ok(&p));
        assert_eq!(&p[26..538], &dev.read_block(2).unwrap()[..]);
    }

    #[test]
    fn reset_aborts() {
        let mut dev = device(4);
        dev.set_state(2);
        dev.set_state(3);
        assert!(!dev.sense(3));
        dev.set_state(4);
        dev.set_state(6);
        dev.set_state(2);
        assert!(dev.sense(2));
        assert_eq!(dev.phase, Phase::Idle);
    }

    #[test]
    fn partition_map() {
        let mut img = vec![0u8; 200 * DCD_BLOCKSIZE];
        img[0..2].copy_from_slice(b"ER");
        let mut pm = |entry: usize, start: u32, count: u32, ptype: &[u8]| {
            let o = entry * DCD_BLOCKSIZE;
            img[o..o + 2].copy_from_slice(b"PM");
            img[o + 4..o + 8].copy_from_slice(&3u32.to_be_bytes());
            img[o + 8..o + 12].copy_from_slice(&start.to_be_bytes());
            img[o + 12..o + 16].copy_from_slice(&count.to_be_bytes());
            img[o + 48..o + 48 + ptype.len()].copy_from_slice(ptype);
        };
        pm(1, 1, 63, b"Apple_partition_map");
        pm(2, 64, 32, b"Apple_Driver43");
        pm(3, 96, 104, b"Apple_HFS");
        assert_eq!(find_volume(&MemImage(img.clone())), (96, 104));

        // Bare volume
        img[0..2].copy_from_slice(b"LK");
        assert_eq!(find_volume(&MemImage(img)), (0, 200));
    }
}
