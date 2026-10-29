//! The UniDisk 3.5 (A2M2053): a "smart" 3.5" drive on the SmartPort
//! bus, the card's external disk port.
//!
//! The drive has its own 65C02 and talks to the IIe (the bus
//! controller) in packets through the IWM, so it is modelled at the
//! packet level: it collects the bytes the IIe's IWM writes, answers
//! with a packet shifted bit by bit into the IWM's read side, and
//! drives the sense line as /ACK.  The disk is a 400K or 800K image of
//! 512-byte blocks in the drive's block order (a ProDOS-order image);
//! the GCR tracks underneath are not modelled.
//!
//! The bus and the drive's answers are described in SPEC.md §9.0.2 and
//! §9.0.3.  In short:
//!
//! - PH1 and PH3 both set enable the bus; PH0 is then REQ, from the
//!   IIe.  PH0 and PH2 set with PH1 and PH3 clear reset the devices,
//!   which then forget their bus IDs.
//! - /ACK is high while the drive waits for a packet or has its
//!   response ready, and low from the end of a packet until the IIe
//!   drops REQ.
//! - The IIe sends a command packet; WRITEBLOCK, CONTROL and WRITE
//!   follow it with a data packet.  The drive then answers every
//!   command with one packet: the error code in STAT, and the data
//!   (status list or block) as its contents.

use serde::{Deserialize, Serialize};

/// Bytes in a block.
pub const BLOCK_SIZE: usize = 512;
/// The tag bytes in front of a block's data, read and written by the
/// READ and WRITE commands (524-byte Macintosh blocks).
const TAG_SIZE: usize = 12;
/// The image sizes: a single-sided 400K disk and a double-sided 800K
/// one.
const IMAGE_SIZES: [usize; 2] = [800 * BLOCK_SIZE, 1600 * BLOCK_SIZE];

/// The packet begin and end marks.
const PBEGIN: u8 = 0xC3;
const PEND: u8 = 0xC8;
/// The bytes the drive sends before a packet: self-sync bytes (5
/// 10-bit `1111111100` patterns, twice) and the packet begin mark.
const PREAMBLE: [u8; 12] = [
    0xFF, 0x3F, 0xCF, 0xF3, 0xFC, 0xFF, 0x3F, 0xCF, 0xF3, 0xFC, 0xFF, PBEGIN,
];

/// The packet types.
const TYPE_COMMAND: u8 = 0x80;
const TYPE_STATUS: u8 = 0x81;
const TYPE_DATA: u8 = 0x82;
/// The bus ID of the IIe.
const IIE_ID: u8 = 0x80;

/// The error codes in a response's STAT byte.
const ERR_BADCMD: u8 = 0x01;
const ERR_BADPCNT: u8 = 0x04;
const ERR_BADCTL: u8 = 0x21;
const ERR_NOWRITE: u8 = 0x2B;
const ERR_BADBLOCK: u8 = 0x2D;
const ERR_OFFLINE: u8 = 0x2F;
/// The INIT status of the last smart device in the chain.  Nothing
/// smart is ever behind the drive here: a 5.25" drive does not ground
/// the daisy-chain pin the drive senses.
const STAT_LAST_DEVICE: u8 = 0x40;

/// The commands: their parameter count, and whether the IIe sends a
/// data packet after the command packet.
const COMMANDS: [(u8, bool); 10] = [
    (3, false), // STATUS
    (3, false), // READBLOCK
    (3, true),  // WRITEBLOCK
    (1, false), // FORMAT
    (3, true),  // CONTROL
    (2, false), // INIT
    (1, false), // OPEN
    (1, false), // CLOSE
    (4, false), // READ
    (4, true),  // WRITE
];

/// The drive's name in its device information block, padded with
/// spaces.
const DIB_NAME: &[u8; 16] = b"DISK 3.5        ";
/// The device type (3.5" disk), the subtype (removable, no extended
/// calls, no disk-switched errors) and the version word ($1000, low
/// byte first) of the device information block.
const DIB_TYPE: [u8; 4] = [0x01, 0x00, 0x00, 0x10];

/// Where the drive is in a bus transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Bus {
    /// Waiting for a packet, /ACK high.
    #[default]
    Listen,
    /// A packet that is not for this drive, or a bad one: wait for
    /// REQ to drop without answering.
    Ignore,
    /// A packet received: /ACK low until REQ drops.
    Received,
    /// The response is ready: /ACK high until REQ rises.
    Ready,
    /// Sending the response.
    Send,
    /// The response is sent: /ACK low until REQ drops.
    Sent,
}

/// The UniDisk 3.5 and its disk.
#[derive(Default, Serialize, Deserialize)]
pub struct UniDisk {
    /// The inserted disk image (skipped in save states: the host
    /// re-inserts it from the menu).
    #[serde(skip, default)]
    image: Option<Vec<u8>>,
    /// The inserted disk is write-protected.
    locked: bool,
    /// The bus ID given by INIT ($81 = the first device), 0 before.
    id: u8,
    /// Where the drive is in the current bus transaction.
    bus: Bus,
    /// REQ: PH0 while the bus is enabled.
    req: bool,
    /// The next packet is the data packet of the last command.
    want_data: bool,
    /// The packet being received, from the byte after the packet
    /// begin mark; `None` until that mark.
    rx: Option<Vec<u8>>,
    /// The destination ID and contents of the last command packet.
    cmd_dest: u8,
    cmd: Vec<u8>,
    /// The contents of the last data packet.
    data: Vec<u8>,
    /// The response on the wire, and the next bit to send.
    tx: Vec<u8>,
    tx_bit: usize,
}

/// Whether the phase lines enable the bus: PH1 and PH3 set.
pub fn bus_enabled(phases: u8) -> bool {
    phases & 0x0A == 0x0A
}

impl UniDisk {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a 400K or 800K block image.  Returns false (and leaves
    /// the drive empty) for any other size.
    pub fn insert(&mut self, data: Vec<u8>, locked: bool) -> bool {
        if !IMAGE_SIZES.contains(&data.len()) {
            return false;
        }
        self.image = Some(data);
        self.locked = locked;
        true
    }

    /// Ejects the disk.
    pub fn eject(&mut self) {
        self.image = None;
    }

    /// The inserted image, as written by the IIe.
    pub fn image(&self) -> Option<&[u8]> {
        self.image.as_deref()
    }

    /// The phase lines changed: the bus reset, and REQ.
    pub fn set_phases(&mut self, phases: u8) {
        if phases & 0x0F == 0x05 {
            self.bus_reset();
            return;
        }
        let req = bus_enabled(phases) && phases & 0x01 != 0;
        if req == self.req {
            return;
        }
        self.req = req;
        if req {
            self.req_on();
        } else {
            self.req_off();
        }
    }

    /// The bus reset restarts the drive: it forgets its ID and any
    /// transaction.  The disk stays in.
    fn bus_reset(&mut self) {
        *self = Self {
            image: self.image.take(),
            locked: self.locked,
            ..Self::default()
        };
    }

    fn req_on(&mut self) {
        match self.bus {
            // A new packet starts.
            Bus::Listen => self.rx = None,
            Bus::Ready => {
                self.bus = Bus::Send;
                self.tx_bit = 0;
            }
            _ => {}
        }
    }

    fn req_off(&mut self) {
        match self.bus {
            Bus::Received => {
                let with_data = COMMANDS.get(self.cmd[0] as usize).is_some_and(|c| c.1);
                if with_data && !self.want_data {
                    // Ready for the data packet.
                    self.want_data = true;
                    self.bus = Bus::Listen;
                } else {
                    self.want_data = false;
                    self.respond();
                }
            }
            Bus::Ignore | Bus::Sent => self.bus = Bus::Listen,
            // A packet cut short is dropped.
            Bus::Listen => self.rx = None,
            _ => {}
        }
    }

    /// The /ACK line: false while the drive pulls it low.
    pub fn ack(&self) -> bool {
        !matches!(self.bus, Bus::Received | Bus::Sent)
    }

    /// A byte the IIe's IWM finished writing while the bus is enabled.
    pub fn receive(&mut self, byte: u8) {
        if self.bus != Bus::Listen || !self.req {
            return;
        }
        let Some(rx) = &mut self.rx else {
            // The self-sync bytes before the packet begin mark.
            if byte == PBEGIN {
                self.rx = Some(Vec::new());
            }
            return;
        };
        rx.push(byte);
        if rx.len() < 7 {
            return;
        }
        if rx.len() < packet_len(rx) {
            return;
        }
        let raw = self.rx.take().unwrap_or_default();
        self.packet(&raw);
    }

    /// A complete packet: keep it if it is the one the drive waits for.
    fn packet(&mut self, raw: &[u8]) {
        let dest = raw[0];
        let ptype = raw[2];
        let want_type = if self.want_data {
            TYPE_DATA
        } else {
            TYPE_COMMAND
        };
        // A drive without an ID takes any command packet: that is how
        // INIT reaches it.
        let ours = self.id == 0 || dest == self.id;
        match decode(raw) {
            Some(contents) if ours && ptype == want_type => {
                if self.want_data {
                    self.data = contents;
                } else {
                    self.cmd_dest = dest;
                    self.cmd = contents;
                    // A short command packet reads as zeros.
                    self.cmd.resize(9, 0);
                }
                self.bus = Bus::Received;
            }
            _ => self.bus = Bus::Ignore,
        }
    }

    /// The next bit of the response for the IWM's read side (0 when
    /// not sending).
    pub fn next_bit(&mut self) -> bool {
        if self.bus != Bus::Send {
            return false;
        }
        let byte = self.tx[self.tx_bit / 8];
        let bit = byte & (0x80 >> (self.tx_bit % 8)) != 0;
        self.tx_bit += 1;
        if self.tx_bit == self.tx.len() * 8 {
            self.bus = Bus::Sent;
        }
        bit
    }

    /// Runs the command and gets its response ready.
    fn respond(&mut self) {
        let (stat, data) = self.command();
        self.tx = encode(self.id | 0x80, stat, &data);
        self.bus = Bus::Ready;
    }

    /// The command: returns the error code and the response contents.
    fn command(&mut self) -> (u8, Vec<u8>) {
        let cmd = self.cmd[0];
        let Some(&(params, _)) = COMMANDS.get(cmd as usize) else {
            return (ERR_BADCMD, Vec::new());
        };
        if self.cmd[1] != params {
            return (ERR_BADPCNT, Vec::new());
        }
        // The parameters after the unit number, from the third byte
        // of the parameter list: the buffer pointer (unused by the
        // drive), then the status code, control code or block number.
        match cmd {
            0 => self.status(self.cmd[4]),
            1 => self.read_block(block_number(&self.cmd[4..7])),
            2 => self.write_block(block_number(&self.cmd[4..7])),
            3 => self.format(),
            4 => self.control(self.cmd[4]),
            5 => self.init(),
            // READ and WRITE carry the byte count first.
            8 => self.read(block_number(&self.cmd[6..9])),
            9 => self.write(block_number(&self.cmd[6..9])),
            // OPEN and CLOSE are for character devices.
            _ => (ERR_BADCMD, Vec::new()),
        }
    }

    /// The general status: the status byte and the block count.
    fn general_status(&self) -> [u8; 4] {
        // A block device, read, write and format allowed; online with
        // a disk in.  The drive reports an 800K size with no disk in.
        let (status, blocks) = match &self.image {
            Some(image) if self.locked => (0xFC, image.len() / BLOCK_SIZE),
            Some(image) => (0xF8, image.len() / BLOCK_SIZE),
            None => (0xE8, 1600),
        };
        [status, blocks as u8, (blocks >> 8) as u8, 0]
    }

    fn status(&self, code: u8) -> (u8, Vec<u8>) {
        match code {
            0 => (0, self.general_status().to_vec()),
            // The device information block.
            3 => {
                let mut dib = self.general_status().to_vec();
                dib.push(8);
                dib.extend_from_slice(DIB_NAME);
                dib.extend_from_slice(&DIB_TYPE);
                (0, dib)
            }
            // UniDiskStat: the last soft error, the retries and the
            // registers after EXECUTE, none of which happen here.
            5 => (0, vec![0; 8]),
            _ => (ERR_BADCTL, Vec::new()),
        }
    }

    /// The error for an access to `block`, if any: no disk, a
    /// write-protected disk for a write, or a block past the end.
    fn block_error(&self, block: usize, write: bool) -> Option<u8> {
        let Some(image) = &self.image else {
            return Some(ERR_OFFLINE);
        };
        if write && self.locked {
            return Some(ERR_NOWRITE);
        }
        if block >= image.len() / BLOCK_SIZE {
            return Some(ERR_BADBLOCK);
        }
        None
    }

    fn read_block(&self, block: usize) -> (u8, Vec<u8>) {
        if let Some(err) = self.block_error(block, false) {
            return (err, Vec::new());
        }
        let Some(image) = &self.image else {
            return (ERR_OFFLINE, Vec::new());
        };
        let start = block * BLOCK_SIZE;
        (0, image[start..start + BLOCK_SIZE].to_vec())
    }

    /// Writes `data` (padded with zeros) to `block`.
    fn put_block(&mut self, block: usize, data: &[u8]) -> u8 {
        if let Some(err) = self.block_error(block, true) {
            return err;
        }
        let Some(image) = &mut self.image else {
            return ERR_OFFLINE;
        };
        let start = block * BLOCK_SIZE;
        let n = data.len().min(BLOCK_SIZE);
        image[start..start + n].copy_from_slice(&data[..n]);
        image[start + n..start + BLOCK_SIZE].fill(0);
        0
    }

    fn write_block(&mut self, block: usize) -> (u8, Vec<u8>) {
        let data = std::mem::take(&mut self.data);
        (self.put_block(block, &data), Vec::new())
    }

    /// FORMAT: a freshly formatted disk reads as zeros [INF].
    fn format(&mut self) -> (u8, Vec<u8>) {
        if let Some(err) = self.block_error(0, true) {
            return (err, Vec::new());
        }
        if let Some(image) = &mut self.image {
            image.fill(0);
        }
        (0, Vec::new())
    }

    /// READ: a 524-byte Macintosh block, the tags first.  The image
    /// has no tags, so they read as zeros.
    fn read(&self, block: usize) -> (u8, Vec<u8>) {
        let (err, data) = self.read_block(block);
        if err != 0 {
            return (err, data);
        }
        let mut out = vec![0; TAG_SIZE];
        out.extend_from_slice(&data);
        (0, out)
    }

    /// WRITE: a 524-byte Macintosh block; the tags are dropped.
    fn write(&mut self, block: usize) -> (u8, Vec<u8>) {
        let data = std::mem::take(&mut self.data);
        let block_data = data.get(TAG_SIZE..).unwrap_or(&[]);
        (self.put_block(block, block_data), Vec::new())
    }

    /// CONTROL.  The control list came in the data packet; none of the
    /// codes kept here uses it.
    ///
    /// The drive's firmware only takes the data packet for the codes
    /// it knows other than RESET; for RESET and codes past 7 it starts
    /// its response straight away, while the IIe sends the data packet
    /// it always sends (found in the drive and card firmware).  Here
    /// the data packet is always taken first, as the protocol
    /// describes.
    fn control(&mut self, code: u8) -> (u8, Vec<u8>) {
        match code {
            // RESET: the drive's defaults, nothing to do here.
            0 => (0, Vec::new()),
            // EJECT
            4 => {
                self.image = None;
                (0, Vec::new())
            }
            // The device control block, newline and interrupt codes
            // are not supported by the drive.  EXECUTE, SETADDRESS and
            // DOWNLOAD run 65C02 code in the drive's RAM, which is not
            // modelled: they fail like an unknown code.
            _ => (ERR_BADCTL, Vec::new()),
        }
    }

    /// INIT: a drive without an ID takes the destination ID.
    fn init(&mut self) -> (u8, Vec<u8>) {
        if self.id == 0 {
            self.id = self.cmd_dest;
            return (STAT_LAST_DEVICE, Vec::new());
        }
        (0, Vec::new())
    }
}

/// The length of a packet from the byte after the begin mark to the
/// end mark, from its counts.  `raw` holds at least the 7 header
/// bytes: destination, source, type, aux type, status, odd byte count
/// and group count.
fn packet_len(raw: &[u8]) -> usize {
    let odd = (raw[5] & 0x7F) as usize;
    let groups = (raw[6] & 0x7F) as usize;
    let mut len = 7 + groups * 8 + 3;
    if odd > 0 {
        len += 1 + odd;
    }
    len
}

/// The block number in a command packet (3 bytes, low first).
fn block_number(bytes: &[u8]) -> usize {
    bytes[0] as usize | (bytes[1] as usize) << 8 | (bytes[2] as usize) << 16
}

/// Decodes one group from the wire: a byte with the top bits of the
/// group's bytes (bit 6 for the first), then each byte's low 7 bits.
fn decode_group(wire: &[u8], out: &mut Vec<u8>) {
    let msbs = wire[0];
    for (i, b) in wire[1..].iter().enumerate() {
        out.push((b & 0x7F) | ((msbs << (i + 1)) & 0x80));
    }
}

/// Decodes a complete packet (from the byte after the begin mark):
/// the odd bytes first, then the groups of 7.  Returns the contents,
/// or `None` with a bad checksum or end mark.  The checksum is the
/// XOR of the 7 header bytes as sent and of the contents.
fn decode(raw: &[u8]) -> Option<Vec<u8>> {
    let odd = (raw[5] & 0x7F) as usize;
    let groups = (raw[6] & 0x7F) as usize;
    let mut contents = Vec::new();
    let mut pos = 7;
    if odd > 0 {
        decode_group(&raw[pos..pos + 1 + odd], &mut contents);
        pos += 1 + odd;
    }
    for _ in 0..groups {
        decode_group(&raw[pos..pos + 8], &mut contents);
        pos += 8;
    }
    let mut sum = 0;
    for b in raw[..7].iter().chain(contents.iter()) {
        sum ^= b;
    }
    // The checksum is sent as two bytes, the even bits then the odd
    // bits, each with the other bits set.
    let check = raw[pos] & ((raw[pos + 1] << 1) | 0x01);
    if check != sum || raw[pos + 2] != PEND {
        return None;
    }
    Some(contents)
}

/// Encodes one group: the top bits byte, then each byte with bit 7
/// set.
fn encode_group(bytes: &[u8], out: &mut Vec<u8>) {
    let mut msbs = 0x80;
    for (i, b) in bytes.iter().enumerate() {
        msbs |= (b & 0x80) >> (i + 1);
    }
    out.push(msbs);
    for b in bytes {
        out.push(b | 0x80);
    }
}

/// The wire bytes of a response from the drive with bus ID `src`.
/// The drive sends every response as a status packet, block data
/// included (found in the drive firmware; the IIe does not check the
/// type).
fn encode(src: u8, stat: u8, contents: &[u8]) -> Vec<u8> {
    let odd = contents.len() % 7;
    let groups = contents.len() / 7;
    let header = [
        IIE_ID,
        src,
        TYPE_STATUS,
        0x80,
        0x80 | stat,
        0x80 | odd as u8,
        0x80 | groups as u8,
    ];
    let mut out = PREAMBLE.to_vec();
    let mut sum = 0;
    for b in header.iter().chain(contents.iter()) {
        sum ^= b;
    }
    out.extend_from_slice(&header);
    if odd > 0 {
        encode_group(&contents[..odd], &mut out);
    }
    for group in contents[odd..].chunks(7) {
        encode_group(group, &mut out);
    }
    out.push(sum | 0xAA);
    out.push((sum >> 1) | 0xAA);
    out.push(PEND);
    out
}

#[cfg(test)]
mod tests {
    use super::super::disk::DiskII;
    use super::*;

    /// The IIe side of the bus: the IWM soft switch accesses of the
    /// card's slot 5 SmartPort firmware, with its timing (one access
    /// per 4-cycle absolute instruction, polls with their branch).
    struct Iie {
        disk: DiskII,
        motor: bool,
    }

    impl Iie {
        fn new(drives_525: usize, unidisk: bool) -> Self {
            Self {
                disk: DiskII::new(drives_525, unidisk),
                motor: false,
            }
        }

        fn tick(&mut self, cycles: u64) {
            for _ in 0..cycles {
                self.disk.tick(1, self.motor);
            }
        }

        /// An access to `$C0E0`–`$C0EF` at the end of a 4-cycle
        /// instruction, as the card's `io_read`/`io_write` do it.
        fn access(&mut self, addr: u16, write: bool, val: u8) -> u8 {
            self.tick(4);
            match addr {
                0xC0E0..=0xC0E7 => {
                    self.disk
                        .phase_write(((addr - 0xC0E0) / 2) as usize, addr & 1 != 0);
                    0
                }
                0xC0E8 | 0xC0E9 => {
                    self.motor = addr == 0xC0E9;
                    0
                }
                0xC0EA | 0xC0EB => {
                    self.disk.selected = addr == 0xC0EB;
                    0
                }
                _ => {
                    let active = self.disk.active(self.motor);
                    self.disk.iwm_access(addr, write, val, active);
                    if write || addr & 1 != 0 {
                        0xFF
                    } else {
                        self.disk.iwm_read(active)
                    }
                }
            }
        }

        fn read(&mut self, addr: u16) -> u8 {
            self.access(addr, false, 0)
        }

        fn write(&mut self, addr: u16, val: u8) {
            self.access(addr, true, val);
        }

        /// Polls `addr` until bit 7 is set (`LDA addr / BPL`), at most
        /// `tries` times.
        fn poll(&mut self, addr: u16, tries: usize) -> Option<u8> {
            for _ in 0..tries {
                let v = self.read(addr);
                self.tick(2);
                if v & 0x80 != 0 {
                    return Some(v);
                }
            }
            None
        }

        /// The sense line (/ACK), through the status register.
        fn ack(&mut self) -> bool {
            self.read(0xC0EE) & 0x80 != 0
        }

        /// Sends a packet like `$C9A8`: bus enable, mode `$07`, drive 1
        /// and motor on, wait for /ACK high (50 tries), REQ, the sync
        /// bytes, then the packet.  The first byte of each group is
        /// written when the IWM is ready; the others follow 32 cycles
        /// apart without polling, like the firmware.  Then the
        /// underrun wait and the /ACK low wait (10 tries).  Returns
        /// whether the device took the packet.
        fn send(&mut self, wire: &[u8]) -> bool {
            self.read(0xC0E3);
            self.read(0xC0E7);
            self.set_mode(0x07);
            self.read(0xC0EA);
            self.read(0xC0E9);
            let mut ready = false;
            for _ in 0..50 {
                if self.ack() {
                    ready = true;
                    break;
                }
                self.tick(5);
            }
            if !ready {
                self.read(0xC0E0);
                return false;
            }
            self.read(0xC0E1);
            self.write(0xC0EF, 0xFF);
            for &b in [0x3F, 0xCF, 0xF3, 0xFC, 0xFF, PBEGIN]
                .iter()
                .chain(&wire[..7])
            {
                self.poll(0xC0EC, 1000).expect("the IWM never got ready");
                self.write(0xC0ED, b);
            }
            let mut pos = 7;
            let odd = (wire[5] & 0x7F) as usize;
            if odd > 0 {
                for &b in &wire[pos..pos + 1 + odd] {
                    self.poll(0xC0EC, 1000).expect("the IWM never got ready");
                    self.write(0xC0ED, b);
                }
                pos += 1 + odd;
            }
            for _ in 0..(wire[6] & 0x7F) {
                // The top bits byte unpolled, then the first byte when
                // ready, then six bytes 32 cycles apart.
                self.tick(28);
                self.write(0xC0ED, wire[pos]);
                self.poll(0xC0EC, 1000).expect("the IWM never got ready");
                self.write(0xC0ED, wire[pos + 1]);
                for &b in &wire[pos + 2..pos + 8] {
                    self.tick(28);
                    self.write(0xC0ED, b);
                }
                pos += 8;
            }
            for &b in &wire[pos..] {
                self.poll(0xC0EC, 1000).expect("the IWM never got ready");
                self.write(0xC0ED, b);
            }
            // The underrun wait, then the dummy write.
            for _ in 0..1000 {
                if self.read(0xC0EC) & 0x40 == 0 {
                    break;
                }
            }
            self.write(0xC0ED, 0);
            let mut acked = false;
            for _ in 0..9 {
                if !self.ack() {
                    acked = true;
                    break;
                }
                self.tick(5);
            }
            self.read(0xC0E0);
            self.read(0xC0EC);
            acked
        }

        /// The mode write of `$CDD5`: motor off, then the mode until
        /// the status register shows it.
        fn set_mode(&mut self, mode: u8) {
            self.read(0xC0E8);
            self.read(0xC0ED);
            for _ in 0..1000 {
                if self.read(0xC0EE) & 0x1F == mode {
                    return;
                }
                self.write(0xC0EF, mode);
            }
            panic!("the IWM mode never took");
        }

        /// Receives a packet like `$CB05`: wait for /ACK high, REQ,
        /// up to 30 bytes for the begin mark, the packet, then wait
        /// for /ACK low and drop REQ.  Returns the STAT byte and the
        /// contents, or `None` on a bad packet.
        fn receive(&mut self) -> Option<(u8, Vec<u8>)> {
            self.read(0xC0ED);
            for _ in 0..1_000_000 {
                if self.ack() {
                    break;
                }
            }
            self.read(0xC0E1);
            let mut found = false;
            for _ in 0..30 {
                if self.poll(0xC0EC, 1000)? == PBEGIN {
                    found = true;
                    break;
                }
            }
            if !found {
                self.read(0xC0E0);
                return None;
            }
            let mut raw = Vec::new();
            for _ in 0..7 {
                raw.push(self.poll(0xC0EC, 1000)?);
            }
            for _ in 7..packet_len(&raw) {
                raw.push(self.poll(0xC0EC, 1000)?);
            }
            self.read(0xC0ED);
            for _ in 0..1_000_000 {
                if !self.ack() {
                    break;
                }
            }
            self.read(0xC0E0);
            let contents = decode(&raw)?;
            Some((raw[4] & 0x7F, contents))
        }

        /// A command, its data packet if any, and the response.
        fn call(&mut self, dest: u8, cmd: &[u8; 9], data: Option<&[u8]>) -> Option<(u8, Vec<u8>)> {
            if !self.send(&iie_packet(dest, TYPE_COMMAND, cmd)) {
                return None;
            }
            if let Some(data) = data
                && !self.send(&iie_packet(dest, TYPE_DATA, data))
            {
                return None;
            }
            self.receive()
        }

        /// The bus reset of `$CEC1`: PH0 and PH2, then all phases off.
        fn bus_reset(&mut self) {
            for addr in [0xC0E0, 0xC0E2, 0xC0E4, 0xC0E6, 0xC0E1, 0xC0E5] {
                self.read(addr);
            }
            self.tick(1000);
            for addr in [0xC0E0, 0xC0E2, 0xC0E4, 0xC0E6] {
                self.read(addr);
            }
        }
    }

    /// A packet from the IIe, from the byte after the begin mark: the
    /// header, the contents and the checksum (the firmware's `$CD1A`
    /// and `$C9DF`–`$CAD1`).
    fn iie_packet(dest: u8, ptype: u8, contents: &[u8]) -> Vec<u8> {
        let odd = contents.len() % 7;
        let groups = contents.len() / 7;
        let mut out = vec![
            dest,
            IIE_ID,
            ptype,
            0x80,
            0x80,
            0x80 | odd as u8,
            0x80 | groups as u8,
        ];
        let mut sum = 0;
        for b in out.iter().chain(contents.iter()) {
            sum ^= b;
        }
        if odd > 0 {
            encode_group(&contents[..odd], &mut out);
        }
        for group in contents[odd..].chunks(7) {
            encode_group(group, &mut out);
        }
        out.push(sum | 0xAA);
        out.push((sum >> 1) | 0xAA);
        out.push(PEND);
        out
    }

    /// The bus reset and INIT of `$CE4D`: unit 1 takes ID $81, and
    /// says it is the last device.
    fn init(iie: &mut Iie) {
        iie.bus_reset();
        let (stat, contents) = iie
            .call(0x81, &[5, 2, 0, 0, 0, 0, 0, 0, 0], None)
            .expect("no answer to INIT");
        assert_eq!(stat, STAT_LAST_DEVICE);
        assert!(contents.is_empty());
    }

    fn image() -> Vec<u8> {
        let mut img = vec![0u8; 1600 * BLOCK_SIZE];
        for (i, b) in img.iter_mut().enumerate() {
            *b = (i / BLOCK_SIZE + i * 7) as u8;
        }
        img
    }

    #[test]
    fn packets_round_trip() {
        let mut data = vec![0u8; 512];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i * 37) as u8;
        }
        let wire = encode(0x81, 0x27, &data);
        let raw = &wire[PREAMBLE.len()..];
        assert_eq!(packet_len(raw), raw.len());
        assert_eq!(decode(raw), Some(data.clone()));
        assert_eq!(raw[4], 0x80 | 0x27);
        let mut bad = raw.to_vec();
        bad[20] ^= 0x01;
        assert_eq!(decode(&bad), None);
    }

    #[test]
    fn init_assigns_the_id() {
        let mut iie = Iie::new(0, true);
        init(&mut iie);
        // A second INIT for unit 2 gets no answer: there is no other
        // device, so the IIe counts one.
        assert!(!iie.send(&iie_packet(
            0x82,
            TYPE_COMMAND,
            &[5, 2, 0, 0, 0, 0, 0, 0, 0]
        )));
    }

    #[test]
    fn status_and_dib() {
        let mut iie = Iie::new(0, true);
        init(&mut iie);
        // No disk: offline, 800K.
        let (stat, st) = iie.call(0x81, &[0, 3, 0, 0, 0, 0, 0, 0, 0], None).unwrap();
        assert_eq!(stat, 0);
        assert_eq!(st, [0xE8, 0x40, 0x06, 0x00]);
        iie.disk.unidisk.as_mut().unwrap().insert(image(), true);
        let (stat, dib) = iie.call(0x81, &[0, 3, 0, 0, 3, 0, 0, 0, 0], None).unwrap();
        assert_eq!(stat, 0);
        assert_eq!(dib.len(), 25);
        assert_eq!(&dib[..5], &[0xFC, 0x40, 0x06, 0x00, 8]);
        assert_eq!(&dib[5..21], b"DISK 3.5        ");
        assert_eq!(&dib[21..], &[0x01, 0x00, 0x00, 0x10]);
        // An unknown status code.
        let (stat, _) = iie.call(0x81, &[0, 3, 0, 0, 1, 0, 0, 0, 0], None).unwrap();
        assert_eq!(stat, ERR_BADCTL);
    }

    #[test]
    fn read_and_write_blocks() {
        let mut iie = Iie::new(0, true);
        init(&mut iie);
        let (stat, _) = iie.call(0x81, &[1, 3, 0, 0, 0, 0, 0, 0, 0], None).unwrap();
        assert_eq!(stat, ERR_OFFLINE);

        let img = image();
        iie.disk
            .unidisk
            .as_mut()
            .unwrap()
            .insert(img.clone(), false);
        // READBLOCK 1599 (the last).
        let (stat, block) = iie
            .call(0x81, &[1, 3, 0, 0x08, 0x3F, 0x06, 0, 0, 0], None)
            .unwrap();
        assert_eq!(stat, 0);
        assert_eq!(block, &img[1599 * BLOCK_SIZE..1600 * BLOCK_SIZE]);
        // Past the end.
        let (stat, block) = iie
            .call(0x81, &[1, 3, 0, 0x08, 0x40, 0x06, 0, 0, 0], None)
            .unwrap();
        assert_eq!(stat, ERR_BADBLOCK);
        assert!(block.is_empty());

        // WRITEBLOCK 2, then read it back.
        let mut data = [0u8; BLOCK_SIZE];
        for (i, b) in data.iter_mut().enumerate() {
            *b = 0xFF - (i * 3) as u8;
        }
        let (stat, _) = iie
            .call(0x81, &[2, 3, 0, 0x08, 2, 0, 0, 0, 0], Some(&data))
            .unwrap();
        assert_eq!(stat, 0);
        let (stat, block) = iie
            .call(0x81, &[1, 3, 0, 0x08, 2, 0, 0, 0, 0], None)
            .unwrap();
        assert_eq!(stat, 0);
        assert_eq!(block, data);
        let image = iie.disk.unidisk.as_ref().unwrap().image().unwrap();
        assert_eq!(&image[2 * BLOCK_SIZE..3 * BLOCK_SIZE], &data);
        assert_eq!(&image[3 * BLOCK_SIZE..], &img[3 * BLOCK_SIZE..]);

        // READ: the 524-byte block with zero tags.
        let (stat, block) = iie
            .call(0x81, &[8, 4, 0, 0x08, 0x0C, 0x02, 2, 0, 0], None)
            .unwrap();
        assert_eq!(stat, 0);
        assert_eq!(&block[..TAG_SIZE], &[0; TAG_SIZE]);
        assert_eq!(&block[TAG_SIZE..], &data);

        // A write-protected disk.
        iie.disk.unidisk.as_mut().unwrap().insert(img, true);
        let (stat, _) = iie
            .call(0x81, &[2, 3, 0, 0x08, 2, 0, 0, 0, 0], Some(&data))
            .unwrap();
        assert_eq!(stat, ERR_NOWRITE);
    }

    #[test]
    fn control_eject() {
        let mut iie = Iie::new(0, true);
        init(&mut iie);
        iie.disk.unidisk.as_mut().unwrap().insert(image(), false);
        let (stat, _) = iie
            .call(0x81, &[4, 3, 0, 0, 4, 0, 0, 0, 0], Some(&[]))
            .unwrap();
        assert_eq!(stat, 0);
        assert!(iie.disk.unidisk.as_ref().unwrap().image().is_none());
        // DOWNLOAD is not supported.
        let (stat, _) = iie
            .call(0x81, &[4, 3, 0, 0, 7, 0, 0, 0, 0], Some(&[0xEA]))
            .unwrap();
        assert_eq!(stat, ERR_BADCTL);
    }

    /// Without a UniDisk the probe's send finds no /ACK: phase 1 holds
    /// a 5.25" drive's sense line high, and with nothing connected it
    /// floats high.  The IIe counts no devices.
    #[test]
    fn the_probe_without_a_unidisk() {
        for drives in [0, 1] {
            let mut iie = Iie::new(drives, false);
            iie.bus_reset();
            assert!(!iie.send(&iie_packet(
                0x81,
                TYPE_COMMAND,
                &[5, 2, 0, 0, 0, 0, 0, 0, 0]
            )));
        }
    }

    /// With the bus disabled, the 5.25" drive behind the UniDisk is
    /// drive 1 again: its sense line follows phase 1.
    #[test]
    fn the_525_drive_behind_the_unidisk() {
        let mut iie = Iie::new(1, true);
        init(&mut iie);
        // All phases off (`$CEFA`): drive 1's sense line, an empty
        // drive with phase 1 off, reads low.
        for addr in [0xC0E0, 0xC0E2, 0xC0E4, 0xC0E6] {
            iie.read(addr);
        }
        iie.read(0xC0ED);
        assert!(!iie.ack());
        // Bus enabled: the UniDisk's /ACK, high when idle.
        iie.read(0xC0E3);
        iie.read(0xC0E7);
        assert!(iie.ack());
    }
}
