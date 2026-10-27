//! The card's 5.25" Disk II: two 35-track drives behind the
//! `$C0E0`–`$C0EF` soft switches, with 16-sector DSK image support.
//!
//! Read-only for now: the selected drive streams the nibblized track
//! through an IWM-style shift register, and the Q6L/Q6H (`$C0EC`/
//! `$C0ED` — the firmware's `LDA $C08C,X` with X=$60) reads return the
//! data register.  The `$C0EF` write echoes into `$C0EE` for the
//! self-test's IWM round-trip check (the Gemini quirk the old stub
//! had; the real IWM mode/status registers are future work).
//!
//! The head stops on quarter-tracks; a map gives the track under each
//! (the WOZ image convention), and each track is a bit stream of its own
//! length.  A track nibblized from a sector image is 51 200 cells: the
//! field nibbles are 8 bits, the self-sync bytes are 10 bits (`1111111100`, MAME's
//! `ap2_dsk.cpp` 0x3FC) so the IWM can re-sync on the trailing zeros.
//! The encoding follows MAME's 16-sector save path: the address field
//! carries the PHYSICAL position (the firmware's sector tables seek
//! physical positions), the data comes from the file chunk the skew
//! table maps there, and the 6-and-2 data field uses the running-XOR
//! checksum nibble.

use serde::{Deserialize, Serialize};

use crate::tickable::Ticks;

/// Tracks in a sector image.
pub const TRACKS: usize = 35;
/// The track length in bytes of a nibblized sector-image track (8 cells
/// each).
pub const TRACK_BYTES: usize = 6400;
/// Bit cells per nibblized track: 51 200 cells of 4 µs = one turn at
/// 300 RPM.  Also the turn of a quarter-track with no track.
pub const CELLS: usize = TRACK_BYTES * 8;
/// Quarter-track positions of the head: 40 tracks, as in a WOZ image's
/// map.
pub const QUARTERS: usize = 160;

/// The DOS 3.3 16-sector order: the logical sector at each physical
/// position (MAME `ap2_dsk.cpp`).
const DOS_SKEW: [u8; 16] = [
    0x0, 0x7, 0xE, 0x6, 0xD, 0x5, 0xC, 0x4, 0xB, 0x3, 0xA, 0x2, 0x9, 0x1, 0x8, 0xF,
];
/// The ProDOS order (MAME `ap2_dsk.cpp`).
const PRODOS_SKEW: [u8; 16] = [
    0x0, 0x8, 0x1, 0x9, 0x2, 0xA, 0x3, 0xB, 0x4, 0xC, 0x5, 0xD, 0x6, 0xE, 0x7, 0xF,
];
/// The 6-and-2 forward table (MAME `flopimg.cpp`).
const GCR62: [u8; 64] = [
    0x96, 0x97, 0x9A, 0x9B, 0x9D, 0x9E, 0x9F, 0xA6, 0xA7, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF, 0xB2, 0xB3,
    0xB4, 0xB5, 0xB6, 0xB7, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xCB, 0xCD, 0xCE, 0xCF, 0xD3,
    0xD6, 0xD7, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF, 0xE5, 0xE6, 0xE7, 0xE9, 0xEA, 0xEB, 0xEC,
    0xED, 0xEE, 0xEF, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF,
];

/// The sector order of a `.dsk` file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DskOrder {
    /// DOS 3.3 order: chunk n holds sector n (the usual `.dsk`).
    Dos,
    /// ProDOS order: chunk i holds the ProDOS-order i-th sector.
    Prodos,
}

/// One track: a bit stream, MSB first, of `len` cells.
pub struct Track {
    /// The cells, 8 per byte, MSB first.
    bits: Box<[u8]>,
    /// The track length in cells.
    len: usize,
}

impl Track {
    /// The bit at cell `cell` (wrapping around the track).
    fn bit(&self, cell: usize) -> bool {
        let cell = cell % self.len;
        self.bits[cell >> 3] & (0x80 >> (cell & 7)) != 0
    }
}

/// A 5.25" disk: its tracks and the track under each quarter-track.
pub struct DskImage {
    /// The tracks, in the image's order.
    tracks: Vec<Track>,
    /// The index in `tracks` of the track under each quarter-track, or
    /// None where there is none.
    map: [Option<u8>; QUARTERS],
}

impl DskImage {
    /// Builds the image from a 143 360-byte `.dsk` (35×16×256).
    pub fn load(bytes: &[u8], order: DskOrder) -> Option<Self> {
        if bytes.len() != TRACKS * 16 * 256 {
            return None;
        }
        let mut tracks = Vec::with_capacity(TRACKS);
        for t in 0..TRACKS {
            let mut buf = [0u8; TRACK_BYTES];
            encode_track(bytes, t, order, &mut buf);
            tracks.push(Track {
                bits: buf.to_vec().into_boxed_slice(),
                len: CELLS,
            });
        }
        Some(Self {
            tracks,
            map: whole_track_map(TRACKS),
        })
    }

    /// The track under quarter-track `quarter`, if any.
    fn track_at(&self, quarter: usize) -> Option<&Track> {
        let i = (*self.map.get(quarter)?)?;
        self.tracks.get(i as usize)
    }
}

/// The quarter-track map of whole tracks 0..`count`: track t is read
/// from quarter-tracks 4t-1 to 4t+1, and nothing between (the WOZ
/// reference's convention for images made from sectors).
fn whole_track_map(count: usize) -> [Option<u8>; QUARTERS] {
    let mut map = [None; QUARTERS];
    for (quarter, entry) in map.iter_mut().enumerate() {
        let track = (quarter + 1) / 4;
        if quarter % 4 != 2 && track < count {
            *entry = Some(track as u8);
        }
    }
    map
}

/// 4-and-4 encode of an address-field value: two nibbles with bit 7
/// set (MAME's `gcr4_encode` = `(va << 7) | va | 0xaaaa`).
fn gcr4(va: u8) -> (u8, u8) {
    let v = (va as u16) << 7 | va as u16 | 0xaaaa;
    ((v >> 8) as u8, v as u8)
}

/// A writing cursor into the fixed-size track bit stream.
struct BitWriter<'a> {
    out: &'a mut [u8; TRACK_BYTES],
    /// The bit-cell position (0..=CELLS).
    pos: usize,
}

impl BitWriter<'_> {
    fn put_bit(&mut self, bit: bool) {
        if self.pos < CELLS {
            if bit {
                self.out[self.pos >> 3] |= 0x80 >> (self.pos & 7);
            }
            self.pos += 1;
        }
    }

    /// One 8-bit nibble, MSB first.
    fn put_nibble(&mut self, b: u8) {
        for i in (0..8).rev() {
            self.put_bit(b & (1 << i) != 0);
        }
    }

    /// One 10-bit self-sync byte (`1111111100`, MAME's 0x3FC): the two
    /// trailing zeros give the IWM its re-sync boundary.
    fn put_sync(&mut self) {
        for _ in 0..8 {
            self.put_bit(true);
        }
        for _ in 0..2 {
            self.put_bit(false);
        }
    }

    fn put_syncs(&mut self, n: usize) {
        for _ in 0..n {
            self.put_sync();
        }
    }
}

/// One 16-sector track: 16 sectors with the classic layout, padded to
/// the full track with self-sync bytes (MAME `ap2_dsk.cpp`).
fn encode_track(img: &[u8], track: usize, order: DskOrder, out: &mut [u8; TRACK_BYTES]) {
    let skew = match order {
        DskOrder::Dos => &DOS_SKEW,
        DskOrder::Prodos => &PRODOS_SKEW,
    };
    let base = track * 4096;
    let mut cur = BitWriter { out, pos: 0 };
    for (i, &sector) in skew.iter().enumerate() {
        // The data: the file chunk the skew table maps to this
        // physical position (the file is in logical sector order).
        let data = &img[base + 256 * sector as usize..base + 256 * sector as usize + 256];

        // Address field: self-sync, prologue, 4-and-4 volume/track/
        // sector/checksum, epilogue, gap.  The sector number in the
        // address field is the PHYSICAL position i — the firmware's
        // sector tables seek physical positions.
        cur.put_syncs(13);
        cur.put_nibble(0xD5);
        cur.put_nibble(0xAA);
        cur.put_nibble(0x96);
        let (h, l) = gcr4(0xFE);
        cur.put_nibble(h);
        cur.put_nibble(l);
        let (h, l) = gcr4(track as u8);
        cur.put_nibble(h);
        cur.put_nibble(l);
        let (h, l) = gcr4(i as u8);
        cur.put_nibble(h);
        cur.put_nibble(l);
        let (h, l) = gcr4(0xFE ^ track as u8 ^ i as u8);
        cur.put_nibble(h);
        cur.put_nibble(l);
        cur.put_nibble(0xDE);
        cur.put_nibble(0xAA);
        cur.put_nibble(0xEB);
        cur.put_syncs(5);

        // Data field: self-sync, prologue, 343 6-and-2 nibbles,
        // epilogue, gap.
        cur.put_syncs(6);
        cur.put_nibble(0xD5);
        cur.put_nibble(0xAA);
        cur.put_nibble(0xAD);
        encode62(data, |b| cur.put_nibble(b));
        cur.put_nibble(0xDE);
        cur.put_nibble(0xAA);
        cur.put_nibble(0xEB);
        cur.put_syncs(5);
    }
    // The trailing gap (the last sync may be cut short by the track
    // end; the drive wraps at the seam, which the firmware never
    // reads across during boot).
    while cur.pos < CELLS {
        cur.put_sync();
    }
}

/// The 6-and-2 data field: 256 data bytes → 343 nibbles with the
/// running-XOR checksum (MAME `ap2_dsk.cpp`).
fn encode62(data: &[u8], mut put: impl FnMut(u8)) {
    let mut buf = [0u8; 257];
    buf[..256].copy_from_slice(data);
    let mut pval = 0u8;
    for i in 0..342 {
        let nval = if i >= 0x56 {
            buf[i - 0x56] >> 2
        } else {
            let mut v = ((buf[i] & 1) << 1)
                | ((buf[i] & 2) >> 1)
                | ((buf[i + 0x56] & 1) << 3)
                | ((buf[i + 0x56] & 2) << 1);
            if i < 0x54 {
                v |= ((buf[i + 0xAC] & 1) << 5) | ((buf[i + 0xAC] & 2) << 3);
            }
            v
        };
        put(GCR62[(nval ^ pval) as usize]);
        pval = nval;
    }
    put(GCR62[pval as usize]);
}

/// One 5.25" drive: a quarter-track stepper, the 300 RPM spindle and
/// the bit stream of the inserted image (or nothing).  The phase latch
/// is shared by both drives on the IWM (it lives in [`DiskII`]).
#[derive(Default, Serialize, Deserialize)]
pub struct Drive5 {
    /// The inserted image (skipped in save states: the host re-inserts
    /// it from the menu).
    #[serde(skip, default)]
    pub image: Option<DskImage>,
    /// The head position in quarter-tracks (0..QUARTERS).
    pos: i16,
    /// The bit-cell index into the current track.
    cell: usize,
}

/// The IWM bit-cell clock per 65C02 cycle in 16.16 fixed point: one
/// bit cell every 4 CPU cycles, so a byte takes 32 cycles as on the Apple
/// II.  The slow-mode IWM cell is 28 FCLK, 4 cycles of a 7 FCLK CPU;
/// the card is assumed to keep that ratio [INF].  The slot 5 SmartPort
/// send depends on it: it writes most packet bytes without polling
/// the handshake, 32 cycles apart.  In fast mode (÷8) the disk spins
/// twice as fast in real time.
const CELL_ADVANCE: Ticks = 0x4000;

/// The motor-off delay in 65C02 cycles: the IWM timer (mode bit 2
/// clear) runs 2^23 FCLK periods (MAME `iwm.cpp`), about 1.2 s, which
/// at 7 FCLK per CPU cycle is 2^23 / 7 cycles.  Loaders rely on it:
/// Lode Runner's switches the motor off and keeps reading the disk.
const MOTOR_OFF_DELAY: u32 = (1 << 23) / 7;

impl Drive5 {
    pub fn new() -> Self {
        Self::default()
    }

    /// The phase latch write: quarter-track stepping (MAME
    /// `floppy_image_device::seek_phase_w`).
    pub fn set_phase(&mut self, phases: u8) {
        let phase = phases & 0x0F;
        let req: i32 = match phase {
            0x1 => 0,
            0x3 => 1,
            0x2 => 2,
            0x6 => 3,
            0x4 => 4,
            0xC => 5,
            0x8 => 6,
            0x9 => 7,
            // The two-coil detent states don't move the head.
            _ => return,
        };
        let cur = self.pos as i32;
        // The opposite polarity of the same position: no move.
        if ((cur ^ req) & 7) == 4 {
            return;
        }
        let mut next = (cur & !7) | req;
        if next < cur - 4 {
            next += 8;
        } else if next > cur + 4 {
            next -= 8;
        }
        let old_len = self.track_len();
        self.pos = next.clamp(0, QUARTERS as i32 - 1) as i16;
        // The disk keeps turning: the head lands at the same angle on a
        // track of another length (the WOZ reference asks for this)
        let new_len = self.track_len();
        if new_len != old_len {
            self.cell = self.cell * new_len / old_len;
        }
    }

    /// The track under the head, if any.
    fn track(&self) -> Option<&Track> {
        self.image.as_ref()?.track_at(self.pos as usize)
    }

    /// The cells in one turn under the head.
    fn track_len(&self) -> usize {
        self.track().map_or(CELLS, |t| t.len)
    }

    /// Advances the spindle by one bit cell and returns the cell it
    /// leaves.
    pub fn next_bit(&mut self) -> bool {
        let bit = self.head_bit();
        self.cell += 1;
        if self.cell >= self.track_len() {
            self.cell = 0;
        }
        bit
    }

    /// The head bit at the current position (0 with no disk, or no
    /// track there).
    pub fn head_bit(&self) -> bool {
        self.track().is_some_and(|t| t.bit(self.cell))
    }
}

/// The IWM's register side, per MAME's `iwm.cpp`:
///
/// - Q6/Q7 are set or cleared by ANY access to `$C0EC`–`$C0EF` (the
///   even address clears, the odd sets — the soft-switch parity).
/// - The even-address reads return the data, status or handshake
///   register depending on Q6/Q7; the odd reads return `$FF`.
/// - A write with Q6 and Q7 set loads the mode register while the
///   motor is off, and the write-data register while it's on.
/// - The status register's bits 0–4 mirror the mode, bit 5 is the
///   active flag, and bit 7 is the drive's sense line
///   (write-protect OR phase 1 — the 5.25" drive detection).
/// - The handshake register: bit 7 = "ready for the next byte"
///   (set when the async write shifter consumes a byte), bit 6 = the
///   write underrun (set on entering the write mode, cleared on the
///   underrun and when the IWM goes idle).
///
/// The UniDisk (the external IWM device) is not modelled yet.
/// The IWM's read/write mode, following MAME's `m_rw`/`m_active`:
/// Q7 picks the write mode (set) or the read mode (clear) while the
/// motor is on; the motor off puts it back to idle.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
enum Rw {
    #[default]
    Idle,
    Read,
    Write,
}

#[derive(Serialize, Deserialize)]
struct Iwm {
    /// Q6: with Q7, selects the register an even access reads or writes.
    q6: bool,
    /// Q7: with Q6, selects the register; set for the write mode.
    q7: bool,
    /// The read/write mode (Q7 + the motor).
    rw: Rw,
    /// The mode register (bit 1 = async, bit 0 = latched).
    mode: u8,
    /// The data register.
    data: u8,
    /// The status register (bits 0–4 = the mode, bit 5 = active).
    status: u8,
    /// The handshake register (bit 7 = ready, bit 6 = underrun).
    handshake: u8,
    /// The self-synced byte framing.
    shift: u8,
    /// The bits shifted into `shift` since the last byte.
    bits: u8,
    /// The sync-mode data visibility in cells: 3 is stored when a
    /// byte completes and decremented in the same call and on each
    /// following cell, so the byte is visible for about two cells
    /// (~7 CPU cycles) — enough for the boot ROM's 7-cycle poll and
    /// gone before its 12-cycle re-read.
    hold: u8,
    /// The pending write byte (a Q6+Q7 write with the motor on), for
    /// the async write shifter.
    write_pending: Option<u8>,
    /// The write shifter's cell counter (every 8 cells it consumes
    /// the pending byte or flags the underrun).
    write_cells: u8,
    /// The byte the write shifter is sending.
    write_shift: Option<u8>,
}

impl Default for Iwm {
    fn default() -> Self {
        Self {
            q6: false,
            q7: false,
            rw: Rw::Idle,
            mode: 0,
            data: 0,
            status: 0,
            // MAME starts the handshake at $BF (ready + no underrun).
            handshake: 0xBF,
            shift: 0,
            bits: 0,
            hold: 0,
            write_pending: None,
            write_cells: 0,
            write_shift: None,
        }
    }
}

impl Iwm {
    /// One bit cell: `bit` is the read data line.  Returns the byte
    /// the write shifter finished sending in this cell, if any.
    fn cell(&mut self, bit: bool) -> Option<u8> {
        self.shift = (self.shift << 1) | bit as u8;
        self.bits += 1;
        if self.bits == 8 {
            if self.shift & 0x80 != 0 {
                self.data = self.shift;
                if self.mode & 0x02 == 0 {
                    // The sync mode: the byte is visible for about
                    // two cells (3 minus the same-call decrement
                    // below).
                    self.hold = 3;
                }
                self.shift = 0;
                self.bits = 0;
            } else {
                // The window's top bit is still 0: not a nibble start.
                // Keep the last 7 bits and shift on.
                self.bits = 7;
            }
        }
        if self.hold > 0 {
            self.hold -= 1;
            if self.hold == 0 {
                self.data = 0;
            }
        }

        // The async write shifter (MAME `sync()` MODE_WRITE): it runs
        // only in the write mode and only until the underrun stops it
        // (bit 6 clear — MAME's loop breaks on `!(m_whd & 0x40)`).
        // Every byte time the byte it was sending is out, and it
        // takes the pending write byte and raises the ready flag;
        // with no new byte (the latched mode) it flags the underrun
        // instead, like MAME's `SW_WINDOW_LOAD`.  The non-latched
        // mode underruns straight away, also like MAME (`data_w`
        // never clears bit 7 there).
        let mut sent = None;
        if self.rw == Rw::Write && self.mode & 0x02 != 0 && self.handshake & 0x40 != 0 {
            // Mode bit 3 (fast) halves the cell: a byte is 4 of our
            // 4-cycle cells.
            let cells = if self.mode & 0x08 != 0 { 4 } else { 8 };
            self.write_cells += 1;
            if self.write_cells >= cells {
                self.write_cells = 0;
                sent = self.write_shift.take();
                if self.mode & 0x01 != 0 && self.write_pending.is_some() {
                    self.write_shift = self.write_pending.take();
                    self.handshake |= 0x80;
                } else {
                    self.handshake &= !0x40;
                }
            }
        }
        sent
    }

    /// The access side effects for `$C0EC`–`$C0EF` (MAME `control()`):
    /// the Q6/Q7 parity toggle, the read/write mode transitions (Q7
    /// picks the write or read mode with the motor on; the underrun
    /// bit is set only on the transition into the write mode, and the
    /// data register clears on the transition into the read mode), and
    /// the mode/data load on a Q6+Q7 write.
    fn access(&mut self, addr: u16, write: bool, val: u8, motor: bool) {
        let set = addr & 1 != 0;
        match addr & 0x0E {
            0x0C => self.q6 = set,
            0x0E => self.q7 = set,
            _ => {}
        }
        if motor {
            self.status |= 0x20;
            if !self.q7 {
                // Q7 clear: the read mode.  Entering it clears the
                // data register (MAME `m_data = 0x00`), so the last
                // packet byte is not the first thing the next disk
                // read returns.
                if self.rw != Rw::Read {
                    self.rw = Rw::Read;
                    self.data = 0;
                }
            } else if self.rw != Rw::Write {
                // The transition into the write mode sets the
                // underrun bit (MAME `m_whd |= 0x40`); it is NOT
                // re-set on every access, so the probe's underrun
                // wait (`LDA $C0EC / AND #$40 / BNE`) can see it
                // clear.
                self.rw = Rw::Write;
                self.handshake |= 0x40;
                // The first byte loads on the next cell (MAME: 7 FCLK
                // after the start), then one byte per byte time.
                self.write_cells = 7;
                self.write_shift = None;
            }
        } else if self.rw != Rw::Idle {
            // The motor off idles the IWM and clears the underrun.
            // (The 1-second motor-off delay is still unmodelled.)
            self.rw = Rw::Idle;
            self.status &= !0x20;
            self.handshake &= !0x40;
        }
        if write && self.q6 && self.q7 {
            if motor {
                // The write-data register: queue the byte for the
                // write shifter; the latched mode then clears the
                // ready flag.
                self.data = val;
                self.write_pending = Some(val);
                if self.mode & 0x01 != 0 {
                    self.handshake &= 0x7F;
                }
            } else {
                // The mode register; the status mirrors its low bits.
                self.mode = val;
                self.status = (self.status & 0xE0) | (val & 0x1F);
            }
        }
    }

    /// An even-address read, selected by Q6/Q7.  `sense` is the
    /// drive's sense line (write-protect OR phase 1).
    fn read(&mut self, motor: bool, sense: bool) -> u8 {
        match (self.q6, self.q7) {
            (false, false) => {
                let v = if motor { self.data } else { 0xFF };
                // The async mode holds the byte until read, then
                // clears it.
                if self.mode & 0x02 != 0 {
                    self.data = 0;
                }
                v
            }
            (true, false) => (self.status & 0x7F) | ((sense as u8) << 7),
            (false, true) => self.handshake,
            (true, true) => 0xFF,
        }
    }
}

/// The Disk II controller: the two drives, the drive select, the
/// phase latch and the IWM registers.
#[derive(Default, Serialize, Deserialize)]
pub struct DiskII {
    /// The 5.25" drives, in daisy-chain order; `None` is no drive
    /// connected (not an empty drive).
    pub drives: [Option<Drive5>; 2],
    /// The selected drive (false = drive 1, true = drive 2).
    pub selected: bool,
    /// The phase latch (4 bits), shared by both drives.
    phase: u8,
    /// Cycles the drive keeps spinning after the motor switch is off.
    spin_down: u32,
    /// The bit-cell clock accumulator (16.16).
    acc: Ticks,
    /// The IWM controller.
    iwm: Iwm,
}

impl DiskII {
    /// A controller with `count` 5.25" drives connected (0 to 2).
    pub fn new(count: usize) -> Self {
        let mut disk = Self::default();
        for drive in disk.drives.iter_mut().take(count) {
            *drive = Some(Drive5::new());
        }
        disk
    }

    /// Resets the controller and the drives. The drives keep their
    /// disks: the host menu inserts them, rebooting the 65C02 must not
    /// eject.
    pub fn reset(&mut self) {
        let mut disk = Self::default();
        for (new, old) in disk.drives.iter_mut().zip(self.drives.iter_mut()) {
            if let Some(old) = old {
                *new = Some(Drive5 {
                    image: old.image.take(),
                    ..Drive5::new()
                });
            }
        }
        *self = disk;
    }

    /// Inserts an image into a drive (0 or 1), or ejects it with
    /// `None`. Returns false if that drive is not connected.
    pub fn insert(&mut self, drive: usize, img: Option<DskImage>) -> bool {
        match self.drives.get_mut(drive) {
            Some(Some(drv)) => {
                drv.image = img;
                true
            }
            _ => false,
        }
    }

    /// The bus tick: run the IWM's bit cells while it is enabled.
    /// The selected drive's spindle turns with them.  The cells run
    /// with no drive too: the SmartPort send waits on the write
    /// shifter's ready and underrun flags with no time limit, even
    /// when nothing is connected (§9.0.1).
    pub fn tick(&mut self, cycles: Ticks, motor: bool) {
        if motor {
            // Mode bit 2 set: the motor stops immediately.
            self.spin_down = if self.iwm.mode & 0x04 == 0 {
                MOTOR_OFF_DELAY
            } else {
                0
            };
        } else {
            self.spin_down = self.spin_down.saturating_sub(cycles as u32);
        }
        if !self.active(motor) {
            return;
        }
        self.acc += cycles * CELL_ADVANCE;
        let n = self.acc >> 16;
        self.acc &= 0xFFFF;
        for _ in 0..n {
            self.cell();
        }
    }

    /// One bit cell: the selected drive's head bit goes to the IWM.
    fn cell(&mut self) {
        let bit = match &mut self.drives[self.selected as usize] {
            Some(drv) => drv.next_bit(),
            None => false,
        };
        self.iwm.cell(bit);
    }

    /// Whether the drive is enabled: the motor switch, or the motor-off
    /// delay still running.
    pub fn active(&self, motor: bool) -> bool {
        motor || self.spin_down > 0
    }

    /// A phase-line access (`$C0E0`–`$C0E7`): set or clear phase bit
    /// `p` (shared by both drives).
    pub fn phase_write(&mut self, p: usize, on: bool) {
        self.phase = self.phase & !(1 << p) | ((on as u8) << p);
        if let Some(drv) = &mut self.drives[self.selected as usize] {
            drv.set_phase(self.phase);
        }
    }

    /// Whether phase bit `p` is set.
    pub fn phase_bits(&self, p: usize) -> bool {
        self.phase & (1 << p) != 0
    }

    /// An IWM register access (`$C0EC`–`$C0EF`).
    pub fn iwm_access(&mut self, addr: u16, write: bool, val: u8, motor: bool) {
        self.iwm.access(addr, write, val, motor);
    }

    /// An even-address IWM read (`$C0EC`/`$C0EE`), selected by Q6/Q7.
    pub fn iwm_read(&mut self, motor: bool) -> u8 {
        // The sense line: write-protect OR phase 1.  The model is
        // read-only, so an inserted disk is always write-protected.
        // With no drive connected the line reads high, as in MAME's
        // iwm.cpp (`!m_floppy`); whether the card pulls it up is not
        // documented.
        let sense = match &self.drives[self.selected as usize] {
            Some(drv) => drv.image.is_some() || self.phase & 0x02 != 0,
            None => true,
        };
        self.iwm.read(motor, sense)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inverse of the 6-and-2 forward table (index = nibble).
    fn gcr62_inv() -> [u8; 256] {
        let mut inv = [0u8; 256];
        for (i, &n) in GCR62.iter().enumerate() {
            inv[n as usize] = i as u8;
        }
        inv
    }

    /// Decodes a 343-nibble data field back to the 256 data bytes
    /// (the inverse of `encode62`: undo the running XOR, split the
    /// 2-bit and 6-bit streams).
    fn decode62(nibbles: &[u8]) -> [u8; 256] {
        let inv = gcr62_inv();
        let mut vals = [0u8; 343];
        let mut prev = 0u8;
        for i in 0..343 {
            let v = prev ^ inv[nibbles[i] as usize];
            vals[i] = v;
            prev = v;
        }
        let mut data = [0u8; 256];
        // The 2-bit stream (86 values) holds the low 2 bits of three
        // interleaved 0x56-byte streams, bit-swapped (MAME's
        // `decode_sector_data`); the 6-bit stream holds the plain low
        // 6 bits.
        const BIT_SWAP: [u8; 4] = [0, 2, 1, 3];
        for i in 0..0x56 {
            data[i] |= BIT_SWAP[(vals[i] & 0x03) as usize];
            data[i + 0x56] |= BIT_SWAP[((vals[i] >> 2) & 0x03) as usize];
            if i < 0x54 {
                data[i + 0xAC] |= BIT_SWAP[((vals[i] >> 4) & 0x03) as usize];
            }
        }
        for i in 0..256 {
            data[i] |= vals[i + 0x56] << 2;
        }
        data
    }

    #[test]
    fn data_field_round_trip() {
        // The 6-and-2 nibblize round-trips directly.
        let mut data = [0u8; 256];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i * 7 + 3) as u8;
        }
        let mut nibbles = [0u8; 343];
        let mut pos = 0;
        encode62(&data, |b| {
            nibbles[pos] = b;
            pos += 1;
        });
        assert_eq!(pos, 343);
        let back = decode62(&nibbles);
        assert_eq!(back, data);
    }

    /// The drive keeps spinning for about a second after the motor is
    /// switched off (mode bit 2 clear), then stops.
    #[test]
    fn the_motor_off_delay() {
        let img = vec![0u8; TRACKS * 4096];
        let mut disk = DiskII::new(1);
        disk.insert(0, DskImage::load(&img, DskOrder::Dos));
        disk.tick(100, true);
        disk.tick(500_000, false);
        assert!(disk.active(false));
        let cell = disk.drives[0].as_ref().unwrap().cell;
        disk.tick(100, false);
        assert_ne!(
            disk.drives[0].as_ref().unwrap().cell,
            cell,
            "still spinning"
        );
        disk.tick(800_000, false);
        assert!(!disk.active(false));
        let cell = disk.drives[0].as_ref().unwrap().cell;
        disk.tick(100, false);
        assert_eq!(disk.drives[0].as_ref().unwrap().cell, cell, "stopped");
    }

    #[test]
    fn whole_tracks_are_read_around_their_quarter_track() {
        let map = whole_track_map(TRACKS);
        assert_eq!(
            &map[0..8],
            &[
                Some(0),
                Some(0),
                None,
                Some(1),
                Some(1),
                Some(1),
                None,
                Some(2)
            ]
        );
        assert_eq!(map[4 * 34 + 1], Some(34));
        assert_eq!(map[4 * 34 + 2], None);
        assert_eq!(map[4 * 34 + 3], None, "no track 35");
    }

    /// The head keeps its angle when it moves to a track of another
    /// length.
    #[test]
    fn the_head_keeps_its_angle_across_tracks() {
        let mut img = DskImage::load(&vec![0u8; TRACKS * 4096], DskOrder::Dos).unwrap();
        img.tracks[1].len = CELLS / 2;
        let mut drive = Drive5::new();
        drive.image = Some(img);
        drive.cell = CELLS / 2;
        // Coil 1, then coil 2: the head moves to quarter-track 2 (no
        // track there), then 4 (track 1)
        drive.set_phase(0x2);
        drive.set_phase(0x4);
        assert_eq!(drive.pos, 4);
        assert_eq!(drive.cell, CELLS / 4);
    }

    #[test]
    fn the_iwm_streams_the_track() {
        // A 143 360-byte image whose sector 0 = 0..255; spin the drive
        // and read the stream like the Disk II firmware does (LDA
        // $C0EC polling, dedupe repeated register reads).
        let mut img = vec![0u8; TRACKS * 4096];
        for (i, b) in img.iter_mut().take(256).enumerate() {
            *b = i as u8;
        }
        let mut disk = DiskII::new(1);
        disk.insert(0, DskImage::load(&img, DskOrder::Dos));
        let mut runs: Vec<(u8, usize)> = Vec::new();
        for _ in 0..(CELLS * 9) / 8 + 16 {
            disk.tick(1, true);
            // The data register is selected with Q6=Q7=0: clear both
            // (the even accesses) before the read.
            disk.iwm_access(0xC0EC, false, 0, true);
            disk.iwm_access(0xC0EE, false, 0, true);
            let v = disk.iwm_read(true);
            if v != 0 {
                match runs.last_mut() {
                    Some((b, n)) if *b == v => *n += 1,
                    _ => runs.push((v, 1)),
                }
            }
        }
        // Zero reads are skipped, so repeated nibbles merge into one
        // run; the sector-0 address field is the run sequence: prologue, the
        // 4-and-4 volume FE, the track and position fields (four AA
        // nibbles — hence a 4x-long run), the checksum and epilogue.
        let values: Vec<u8> = runs.iter().map(|(b, _)| *b).collect();
        let want = [
            0xD5, 0xAA, 0x96, 0xFF, 0xFE, 0xAA, 0xFF, 0xFE, 0xDE, 0xAA, 0xEB,
        ];
        let pos = values
            .windows(want.len())
            .position(|w| w == want.as_slice())
            .expect("the address field run sequence never appeared");
        // The AA run covers the track + position fields: 4 nibbles.
        let (_, n_aa) = runs[pos + 5];
        let (_, n_fe) = runs[pos + 3];
        assert!(
            n_aa >= n_fe * 3 && n_aa <= n_fe * 5,
            "AA run {n_aa} vs FE run {n_fe}"
        );
        // The data prologue follows.
        let want2 = [0xD5, 0xAA, 0xAD];
        assert!(
            values.windows(3).skip(pos).any(|w| w == want2),
            "the data prologue never appeared"
        );
    }

    /// The write shifter runs with no drive connected: the SmartPort
    /// send's ready and underrun waits have no time limit.
    #[test]
    fn the_iwm_writes_without_a_drive() {
        let mut disk = DiskII::new(0);
        // Mode $07 (latched, async, immediate off) with the motor off.
        disk.iwm_access(0xC0ED, false, 0, false);
        disk.iwm_access(0xC0EF, true, 0x07, false);
        // A byte written with the motor on starts the write mode.
        disk.tick(1, true);
        disk.iwm_access(0xC0EF, true, 0xFF, true);
        // Q6 clear, Q7 set: the handshake register.
        disk.iwm_access(0xC0EC, false, 0, true);
        assert_eq!(disk.iwm_read(true) & 0xC0, 0x40, "the byte is pending");
        disk.tick(8, true);
        assert_eq!(disk.iwm_read(true) & 0xC0, 0xC0, "the byte loaded");
        disk.tick(64, true);
        assert_eq!(disk.iwm_read(true) & 0x40, 0, "the shifter underran");
    }

    /// The sense line of an empty drive follows phase 1; with no drive
    /// connected it reads high whatever the phases.
    #[test]
    fn sense_without_drive() {
        let mut disk = DiskII::new(1);
        // Q6 set, Q7 clear: the status register
        disk.iwm_access(0xC0ED, false, 0, false);
        assert_eq!(disk.iwm_read(false) & 0x80, 0, "empty drive");
        disk.phase_write(1, true);
        assert_ne!(disk.iwm_read(false) & 0x80, 0, "empty drive, phase 1");
        disk.phase_write(1, false);

        disk.selected = true;
        assert_ne!(disk.iwm_read(false) & 0x80, 0, "no drive");
        assert!(!disk.insert(1, None));
    }
}
