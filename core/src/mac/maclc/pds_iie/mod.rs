//! Apple IIe Card for the Macintosh LC family (PDS slot $E).
//!
//! The card combines a 65C02 CPU, the LSI Logic "Gemini" ASIC (a Mega II
//! derivative providing the Apple IIe memory model, soft switches and the
//! host-side register block), an IWM disk controller and 256 KB of RAM.
//! It has no IIe ROM chips: the IIe Startup application on the Mac loads
//! the ROM image (a patched enhanced IIe ROM, boot shim and vectors) into
//! card RAM through the Slinky and then releases the 65C02 from reset.
//! The Mac's 68020 acts as the service processor, polling the card's trap
//! registers, rendering the screen in the V8's 560×384 Apple II mode and
//! emulating the slot devices.
//!
//! `SPEC.md` in this directory describes the card, its host protocol and
//! the evidence behind each fact.
//!
//! # Card RAM layout (256 KB)
//!
//! The card has no IIe ROM chips: the IIe Startup application downloads
//! the images into the upper half through the Slinky (validated by the
//! self-test, `replays_the_iie_selftest`):
//!
//! | Offset    | Size  | Contents                                    |
//! |-----------|-------|---------------------------------------------|
//! | `0x00000` | 64 KB | IIe main RAM; physical `$C000`–`$EFFF` is   |
//! |           |       | LC bank 1, `$F000`–`$FFFF` LC bank 2        |
//! | `0x10000` | 64 KB | IIe aux RAM (same layout)                   |
//! | `0x20000` | 16 KB | the **main** ROM (A2EMonitorROM, CPU        |
//! |           |       | `$C000`–`$FFFF` = card addr − `$C000`; the  |
//! |           |       | normal view, RESET vector at `+0x3FFA`)     |
//! | `0x24000` | 16 KB | D1 (the alternate firmware bank: the        |
//! |           |       | NMI-IRQ receiver at `+0x1000` = CPU `$D000`,|
//! |           |       | INTCX `+0x03FA`, PFI `+0x1400`–`+0x36FF`,   |
//! |           |       | vectors `+0x3FFA`)                          |
//! | `0x28000` | 16 KB | D2 (the second alternate bank, `$C02B`      |
//! |           |       | bit 3)                                      |
//! | `0x2C000` | 16 KB | slot ROM bank (`$C100`–`$C7FF` slot ROMs,   |
//! |           |       | 5.25" `$C800` code at bank `$F000`)         |
//! | `0x30000` | 64 KB | upper-aux half (the Slinky's `$11` lane     |
//! |           |       | with `$0D` bit 3 set); `0x3FF00` = the      |
//! |           |       | second zero page (`$C02B` bit 4 + ALTZP)    |
//!
//! # The trap mechanism
//!
//! When the 65C02 touches an area that the Mac-side IIe Startup
//! application must see (display stores, video soft switches, slot I/O —
//! full list in Apple TN AIIE #10), the Gemini halts the CPU and latches
//! the address, the data and a trap code.  The host polls the register
//! block, handles the trap, and writes the ack.  A trapped
//! *read* re-executes the whole instruction once resumed, with the bus
//! answering the trapped read from the data-out register; a trapped
//! *write* completes the instruction and the CPU halts before the next
//! one (see [`cpu`] for the CPU-side contract).  On the `$20` bit-7 run
//! edge with bit 3 clear the reset vector fetch itself traps (address
//! `$FFFC`); the host sets bit 3 and acks to complete it.
//!
//! # Host register block
//!
//! Sixteen-bit big-endian registers at offsets `$00`–`$20` at the bottom
//! of every 1 MB of the card's PDS slot space (the card decodes A19-A0).
//! The card is a **16-bit port**: the declaration ROM sits on the odd bytes (byte lanes 1+3),
//! and the Slinky transfers once per bus cycle (a word moves main+aux
//! at the same address, one increment; a byte moves one lane, one
//! increment).
//!
//! | Offset | Read                        | Write                          |
//! |--------|-----------------------------|--------------------------------|
//! | `$01`  | trap code (0 = none)        | —                              |
//! | `$04`  | trap address                | —                              |
//! | `$08`  | video soft-switch byte      | —                              |
//! | `$09`  | bit 7 = keyboard strobe     | —                              |
//! | `$0C`  | trap data                   | **trap ack / resume**          |
//! | `$0D`  | control register (bit 7 AKD,| same (bit 3 = Slinky upper-    |
//! |        | 6 VBL, 4 ROM write, 3      | 128K select, bit 2 = aux       |
//! |        | upper-128K, 2 aux disable) | disable)                       |
//! | `$10`  | Slinky main lane            | Slinky main lane               |
//! | `$11`  | Slinky aux lane             | Slinky aux lane                |
//! | `$14`  | —                           | —                              |
//! | `$18`  | keyboard / data-out         | keyboard (bit 7 = strobe)      |
//! | `$19`  | —                           | read-trap answer (resumes)     |
//! | `$1C`  | Slinky address              | same                           |
//! | `$20`  | control reg 2 / 3           | same (bit 7 = run, 5 = IRQ,    |
//! |        |                             | 4 = motor, 3 = run/hold)       |
//!
//! The trap-code table, the `$0D`/`$20` bit meanings and the memory map
//! are validated by the self-test; the remaining provisional pieces are
//! marked in the source.

pub mod cpu;

use std::cell::RefCell;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::bus::C16M_FREQUENCY;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::tickable::{Tickable, Ticks};
#[cfg(feature = "iie-debug")]
use log::{Level, log_enabled};

use cpu::cpu::Cpu;
use cpu::{Address, Bus6502, ReadKind, Variant};

/// Settings of the Apple IIe Card, chosen with the machine
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IieCardSettings {
    /// The Egret's J17 "security" jumper is installed, which locks out
    /// the Option Panel configuration
    pub security_jumper: bool,
}

/// Debug output for the card, compiled in only with the `iie-debug`
/// cargo feature.
macro_rules! iie_debug {
    ($($arg:tt)*) => {
        #[cfg(feature = "iie-debug")]
        log::debug!($($arg)*);
    };
}
pub(crate) use iie_debug;

/// Size of the card RAM: 256 KB.
pub const RAM_SIZE: usize = 256 * 1024;

/// Offset of the IIe main RAM (64 KB).  `$0000`–`$BFFF` is plain RAM;
/// its physical `$C000`–`$FFFF` is the language-card RAM (the LC window
/// `$D000`–`$FFFF` reads physical `$C000`–`$EFFF`).
pub const MAIN_RAM: usize = 0x0_0000;
/// Offset of the IIe aux RAM (64 KB); same layout as the main bank.
pub const AUX_RAM: usize = 0x1_0000;
/// Offset of the upper-main half's first 16 KB (card `0x20000`): the
/// **main** ROM (A2EMonitorROM, CPU `$C000`–`$FFFF`) — the normal ROM
/// view, with the RESET vector at `+0x3FFA`.
pub const FW_MAIN: usize = 0x2_0000;
/// Offset of D1 (card `0x24000`): the alternate firmware bank — the
/// NMI-IRQ receiver at `+0x1000` (CPU `$D000`), INTCX at `+0x03FA`,
/// PFI at `+0x1400`–`+0x36FF`, vectors at `+0x3FFA`.
pub const FW_D1: usize = 0x2_4000;
/// Offset of D2 (card `0x28000`): the second alternate bank, selected
/// by `$C02B` bit 3.
pub const FW_D2: usize = 0x2_8000;
/// Offset of the slot ROM bank (16 KB, IIe `$C100`–`$C7FF` slot ROMs,
/// slot 1–7 `$C8xx` images at bank `$C800`+n·`$800`); the top quarter
/// of the upper-main half.
pub const FW_SLOT_ROMS: usize = 0x2_C000;
/// Base of the upper-aux half (64 KB), the Slinky's `$11` lane with
/// `$0D` bit 3 set.
pub const UPPER_AUX: usize = 0x3_0000;
/// The second zero page (`$C02B` bit 4 with ALTZP): the last 256 bytes
/// of the upper-aux half, otherwise unused.
pub const AUX2_ZP: usize = 0x3_FF00;

/// The card's declaration ROM: what the boot ROM's Slot Manager scan
/// reads from the top of PDS slot $E space to register the card in the
/// Slot Resource Table.
///
/// The real card's ROM has not been dumped, so this is a synthesized
/// 512-byte ROM in the standard Macintosh declaration ROM format
/// (Designing Cards and Drivers for the Macintosh Family): all
/// structures are big-endian in ascending address order, directory and
/// sResource entries are longwords `[id:8 | offset:24]` with signed
/// 24-bit ROM-byte offsets, and the format block carries the test
/// pattern $5A932BC7, byte lanes 1+3 and the rotating checksum the LC
/// ROM verifies.  It presents two sResources: a Board sResource
/// (category 1, type 0) and the card's own (category 10, type 21,
/// drvrSW 1 — what the IIe Startup INIT searches for).
/// Regenerated with `gen_decl_rom.py` in this directory.
///
/// Logical byte N (0 = top, address `$FEFFFFFF`) sits at slot-window
/// offset `0xFFFFF - 2N`, on byte lanes 1 and 3.
pub const DECL_ROM: [u8; 512] = [
    0x5A, 0x00, 0xC7, 0x2B, 0x93, 0x5A, 0x01, 0x01, 0xFD, 0x3E, 0x1D, 0x9D, 0x00, 0x02, 0x00, 0x00,
    0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x72, 0x61, 0x43, 0x20, 0x65, 0x49, 0x49, 0x20,
    0x65, 0x6C, 0x70, 0x70, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x15, 0x00, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x72, 0x61,
    0x00, 0x00, 0x00, 0xFF, 0x23, 0x00, 0x00, 0x02, 0x14, 0x00, 0x00, 0x01, 0x64, 0x72, 0x61, 0x6F,
    0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x3D, 0x00, 0x00, 0x21, 0x01, 0x00, 0x00, 0x20,
    0x23, 0x00, 0x00, 0x02, 0x1C, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xFF, 0x34, 0x00, 0x00, 0x80,
    0x0C, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// The paddle timers after a `$C070` trigger, in 65C02 cycles: a
/// centred joystick, 128 of the PREAD loop's ~11-cycle counts.  Nothing
/// is modelled on the game port yet, and a centred stick is what
/// software expects at rest (an empty port reads as fully deflected,
/// which joystick-mode games take for constant input).
const PADDLE_CENTRE: u16 = 1408;

/// The 65C02's clock in normal mode: a IIe's average, 14.31818 MHz × 65 / 912
/// (Apple: 1.02 MHz)
const NORMAL_HZ: i64 = 1_020_484;
/// Its effective clock in fast mode, `$C02B` bit 2 (Apple: 1.9 MHz). The fast
/// clock is probably 14.31818 MHz / 7 (2.046 MHz), with the Gemini's RAM
/// refresh stealing cycles, as on the IIGS [INF]
const FAST_HZ: i64 = 1_900_000;

/// C16M, which clocks the card, in Hz
const C16M_HZ: i64 = C16M_FREQUENCY as i64;

/// What kind of access trapped the 65C02.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum TrapKind {
    /// Store to a text/lores or hires page, with the bank, page and
    /// mode that determine the trap code.
    Display { aux: bool, page2: bool, hires: bool },
    /// Write to the 80STORE / 80COL / ALTCHARSET switches
    SoftSwitch,
    /// `$C050`–`$C057` display switches (read or write)
    DisplaySwitch,
    /// `$C058`–`$C05F` annunciators (read or write)
    Annunciator,
    /// `$C061`–`$C063` game button reads (`$21` bit 1)
    Button,
    /// Slot I/O (`$C090`–`$C0DF`, `$C0F0`–`$C0FF`)
    SlotIo,
    /// `$C070`–`$C07F` — the host-service cells: PDLTRIG, the `$C072`
    /// `$5A`/`$00` handshake, the `$C078`–`$C07C` register mailbox
    /// (A, X, Y, P, S) and `$C07D` the service-call number.  Writes
    /// deliver the data via `$0C`, reads are answered via `$19`.
    Mailbox,
    /// `$CFFF` C8-space deselect (any access: clears INTC8ROM and the
    /// selected slot)
    C8Switch,
    /// Slot 6 motor switch (`$C0E8` off / `$C0E9` on, read or write)
    Motor { on: bool },
}

impl TrapKind {
    /// The trap kind for a given trapped address.
    fn of(addr: u16) -> Self {
        match addr {
            0xC050..=0xC057 => Self::DisplaySwitch,
            0xC058..=0xC05F => Self::Annunciator,
            0xC061..=0xC063 => Self::Button,
            0xC070..=0xC07F => Self::Mailbox,
            0xC0E8 => Self::Motor { on: false },
            0xC0E9 => Self::Motor { on: true },
            0xCFFF => Self::C8Switch,
            _ => Self::SlotIo,
        }
    }

    /// Trap code as seen in the host register `$01`, per the self-test
    /// (`Conf` 1000/1001) and the driver's dispatch table.  Bits 7:6
    /// come from `$0D` bits 1:0; the driver masks with `& $3F`.
    ///
    /// - Display stores: `$02` text / `$04` hires, `+$08` aux,
    ///   `+$20` page 2, `+$01` while `$20` bit 4 (motor) is set.
    /// - `$20`/`$21` the `$C0xx` switch writes/reads (data via `$0C`,
    ///   answers via `$19`).
    /// - `$08`/`$09` slot I/O write/read, compacted address.
    /// - `$38`/`$28` motor on/off (read or write).
    fn code(self, write: bool, ctrl: u8, ctrl2: u8) -> u8 {
        let hi = (ctrl & 0x03) << 6;
        let base = match self {
            Self::Display { aux, page2, hires } => {
                // text = $02, hires = $04; +$08 aux, +$20 page 2,
                // +$01 while `$20` bit 4 (motor) is set.
                let mut c = if hires { 0x04 } else { 0x02 };
                if aux {
                    c |= 0x08;
                }
                if page2 {
                    c |= 0x20;
                }
                if ctrl2 & 0x10 != 0 {
                    c |= 0x01;
                }
                c
            }
            Self::SoftSwitch => 0x20,
            Self::DisplaySwitch
            | Self::Annunciator
            | Self::Button
            | Self::Mailbox
            | Self::C8Switch => {
                if write {
                    0x20
                } else {
                    0x21
                }
            }
            Self::SlotIo => {
                if write {
                    0x08
                } else {
                    0x09
                }
            }
            Self::Motor { on } => {
                if on {
                    0x38
                } else {
                    0x28
                }
            }
        };
        base | hi
    }
}

/// A latched trap, as reported through the host register block.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct Trap {
    /// The trap code (see [`TrapKind::code`]).
    code: u8,
    /// The address as the host sees it in `$04` (slot traps carry the
    /// compacted form).
    addr: u16,
    /// The IIe address that trapped — the retry for a trapped read
    /// matches on this, not on the reported form.
    raw_addr: u16,
    /// Data written by a trapped write (0 for a trapped read).
    data: u8,
    /// The trapped access was a write.
    write: bool,
}

/// The Apple IIe soft switches: the state the 65C02 itself sets by
/// touching `$C000`–`$C0FF`, the slot ROM space and `$CFFF`, held by the
/// Gemini as the IIe's MMU and IOU would.  A soft reset puts them all
/// back to [`SoftSwitches::default`].  The rest of the card's state
/// (memory, the Mac host registers, the interrupt lines, peripherals)
/// is in [`BusState`].
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
struct SoftSwitches {
    // --- Memory configuration ($C000-$C00F)
    /// 80STORE: PAGE2 selects main or aux for text page 1 (and hi-res page 1 with HIRES).
    store80: bool,
    /// RAMRD: reads of `$0200`–`$BFFF` come from aux RAM.
    ramrd: bool,
    /// RAMWRT: writes to `$0200`–`$BFFF` go to aux RAM.
    ramwrt: bool,
    /// INTCXROM: `$C100`–`$CFFF` shows the internal ROM, not the slot ROMs.
    intcxrom: bool,
    /// ALTZP: the zero page, the stack and the language card use aux RAM.
    altzp: bool,
    /// INTC3ROM (SLOTC3ROM off): `$C300`–`$C3FF` shows the internal 80-column firmware.
    intc3rom: bool,
    /// 80COL: the display shows 80 columns.
    col80: bool,
    /// ALTCHARSET: the alternate character set (MouseText, no flashing).
    altchar: bool,

    // --- Expansion ROM space ($C100-$CFFF)
    /// INTC8ROM: `$C800`–`$CFFF` shows the internal ROM (set by any
    /// `$C300`–`$C3FF` access with INTC3ROM on, cleared by any `$CFFF`
    /// access).
    intc8rom: bool,
    /// The slot whose `$C8xx` image shows at `$C800`–`$CFFF` (selected
    /// by any `$Cn00`–`$CnFF` access with INTCXROM off; 0 = none).
    cnxx_slot: u8,

    // --- Display ($C050-$C05F)
    /// TEXT: text mode (`$C051`), graphics when off (`$C050`).
    text: bool,
    /// MIXED: four lines of text below the graphics.
    mixed: bool,
    /// PAGE2: display page 2 (with 80STORE, the main/aux select instead).
    page2: bool,
    /// HIRES: hi-res graphics rather than lo-res.
    hires: bool,
    /// DHIRES: double hi-res (the inverse of AN3, `$C05E` on).
    dhires: bool,
    /// AN0–AN3: the game port's annunciator outputs (`$C058`–`$C05F`).
    an: [bool; 4],

    // --- Language card ($C080-$C08F)
    /// The `$D000`–`$DFFF` window shows LC bank 2 rather than bank 1.
    lc_bank2: bool,
    /// Reads of `$D000`–`$FFFF` come from LC RAM, not the ROM.
    lc_read: bool,
    /// Writes to `$D000`–`$FFFF` go to LC RAM.
    lc_write: bool,
    /// The IIe prewrite rule: two accesses to the odd ($C088+) LC
    /// switches in a row enable LC RAM writes.
    lc_prewrite: u8,

    // --- Card-specific
    /// `$C02B` control register: bit 7 set in IRQINTCX, bit 6 INT CX ROM,
    /// bit 5 enable C8-space writes, bit 4 second aux LC bank, bit 3 alt
    /// firmware bank (0 = D1, 1 = D2), bit 2 CPU speed (1 = fast),
    /// bit 1 HDSEL, bit 0 /3.5DISK.
    c02b: u8,
    /// `$C072` latch: the firmware writes `$5A` or `$00` (paired with
    /// bits 4/5 of `$C02B`).
    c072: u8,
    /// `$C029` switched in the alternate firmware bank (D1/D2).
    alt_fw: bool,
    /// A `$C028` write with bit 7 set selects a non-RAM ROM view that
    /// reads `$FF` (shim `$2143` needs `$FFFA` to change).  [INF]
    /// cleared by the next `$C028`/`$C029` access.
    non_ram_bank: bool,
    /// `$C030` speaker toggle (not played yet).
    speaker: bool,
}

impl Default for SoftSwitches {
    fn default() -> Self {
        Self {
            store80: false,
            ramrd: false,
            ramwrt: false,
            // SLOTCXROM at reset (the slot ROMs show at $C100+; the
            // firmware's own INIT thunk sets $C007 before it jumps
            // there, see its $C20A exit).
            intcxrom: false,
            altzp: false,
            intc3rom: true,
            col80: false,
            altchar: false,
            intc8rom: false,
            cnxx_slot: 0,
            // All display switches start OFF: the Mac-side driver's
            // displayed-mode copy starts at 0 ("graphics") and only
            // recomputes after a switch trap that changes the switch
            // copy, so register $08 must read $00 until the 65C02
            // runs — with TEXT on at reset the app would never
            // recompute the mode and every full redraw would come out
            // lo-res.
            text: false,
            mixed: false,
            page2: false,
            hires: false,
            dhires: false,
            an: [false; 4],
            lc_bank2: true,
            // After reset the LC windows read the ROM image until the
            // LC switches enable RAM reads.
            lc_read: false,
            lc_write: false,
            lc_prewrite: 0,
            c02b: 0,
            c072: 0,
            alt_fw: false,
            non_ram_bank: false,
            speaker: false,
        }
    }
}

/// The Mac host registers that the 65C02's side of the card uses (the
/// host block's control registers).  The host-only ones, such as the
/// data-out register, are in [`PdsIIeCard`].
#[derive(Serialize, Deserialize, Default)]
struct HostRegs {
    /// The `$0D` control register: bit 7 AKD (any key down, read at
    /// `$C010`), bit 6 VBL (read at `$C019`), bit 4 ROM write enable,
    /// bit 3 Slinky upper-128K select, bit 2 aux memory disable,
    /// bits 1:0 echoed in the trap code's bits 7:6.
    ctrl: u8,
    /// The `$20` control register: bit 7 run, bit 5 IRQ to the 65C02,
    /// bit 4 the host's motor state (display traps report odd codes),
    /// bit 3 "normal run" (allows the reset vector fetch to complete).
    ctrl2: u8,
    /// The `$21` control register (host register `$20` low byte): the
    /// **display-trap enable mask** (§6.3 of the spec) — bit 7 hi-res
    /// page 1, 6 hi-res page 2, 5 text page 1, 4 text page 2, 3 aux
    /// text, 2 aux hi-res; bits 1:0 preserved but unused.
    ctrl3: u8,
}

/// Everything the 65C02's bus needs to answer an access: the card RAM,
/// the soft switches, the Mac host registers, the Gemini's trap and interrupt
/// logic and the IIe peripherals.  Shared between the 65C02 side
/// ([`PdsIIeBus`]) and the host side ([`PdsIIeCard`]).
#[derive(Serialize, Deserialize, Default)]
struct BusState {
    // --- Memory and switches
    /// The 256 KB card RAM.
    ram: Vec<u8>,
    /// The IIe soft switches.
    sw: SoftSwitches,

    // --- Mac host registers
    /// The host registers the 65C02's side uses.
    host: HostRegs,

    // --- Traps and run control
    /// The latched trap; the 65C02 is suspended while this is set.
    trap: Option<Trap>,
    /// A resumed trapped read: the address and the value (from the host's
    /// data-out register) to return when the re-executed instruction
    /// re-reads it.
    retry: Option<(u16, u8)>,
    /// The reset-vector-fetch trap is pending: latched on the run edge
    /// while `$20` bit 3 is clear, completed when the host sets bit 3
    /// and acks.
    pending_boot: bool,
    /// `$20` bit 3 went clear while the CPU was running: the 65C02 is
    /// held at its next instruction, without latching a trap.  Released
    /// when bit 3 is set again.  [INF]
    hold: bool,

    // --- Interrupts
    /// An NMI/IRQ is being taken: the vector fetch (`$FFFA`–`$FFFF`)
    /// and the receiver code (`$D000`–`$D3FF`) run in the alternate
    /// firmware bank (D1/D2 per `$C02B` bit 3).  The rest of the ROM
    /// window stays in the `$C028`/`$C029`-selected bank, so the
    /// receiver's `nn` operand read at the call site (in the main
    /// ROM, `$FAB4` etc.) works.  Cleared by an RTI or a `$C028`
    /// access.  [INF] the exact window and the NMI path's switch
    /// back — its receiver ends with a bare RTI.
    int_bank: bool,
    /// The IRQ line to the 65C02 (`$20` bit 5).
    irq: bool,
    /// The NMI line to the 65C02: the Gemini asserts it when the 65C02
    /// fetches a `$02` opcode (a `$02 nn` host call, spec §7), until
    /// the 65C02 takes the NMI.
    nmi: bool,

    // --- Peripherals
    /// Last key pressed (7 bits).
    kbd: u8,
    /// The keyboard strobe: a key is waiting (`$C000` bit 7, cleared by `$C010`).
    kbd_strobe: bool,
    /// Paddle timer counters (reset by `$C070`, counted down per cycle).
    paddle: [u16; 4],
    /// Open-Apple / closed-Apple / shift button inputs (bit 7 read).
    btn: [bool; 3],
    /// Slot 6 drive motor (set by the trapped `$C0E8`/`$C0E9` accesses).
    motor_on: bool,

    // --- Debug log state (iie-debug)
    /// Last logged 65C02 I/O access (0 = read, 1 = write, address,
    /// value), to deduplicate the I/O log.
    #[cfg(feature = "iie-debug")]
    last_io: Option<(u8, u16, u8)>,
    /// Last logged bank-served read (address, tag), to deduplicate the
    /// bank log.
    #[cfg(feature = "iie-debug")]
    last_bank: Option<(u16, u8)>,
    /// Last logged RAM-bank read (address, 0 = main, 1 = aux), to
    /// deduplicate the RAM bank log.
    #[cfg(feature = "iie-debug")]
    last_ram: Option<(u16, u8)>,
}

impl BusState {
    /// Whether the given access traps the CPU (TN AIIE #10 table, with
    /// the screen holes, motor and display-mode exceptions).
    fn trap_kind(&self, addr: u16, write: bool) -> Option<TrapKind> {
        let screen_hole = (addr & 0x0078) == 0x0078;
        match addr {
            // Display area stores (writes only), gated on the `$21`
            // enable mask: bit 7 hi-res p1, 6 hi-res p2, 5 text p1,
            // 4 text p2, 3 aux text, 2 aux hi-res — where "aux" is
            // the bank the store actually lands in (the Technical
            // Note's "only what's on screen" rules come from the host
            // programming this mask, spec §6.3).  The code carries
            // the aux bank, the page and the mode, and `$20` bit 4
            // (motor) as bit 0 — the host's driver only marks those
            // cells dirty and redraws them at motor-off.  Only the screen holes
            // are exempt.  The self-test's `$21=$FC` enables every
            // area.
            0x0400..=0x0BFF | 0x2000..=0x5FFF if write => {
                if screen_hole {
                    return None;
                }
                let aux = self.aux_side(addr, true);
                let text_range = addr < 0x2000;
                let page2 = if text_range {
                    addr >= 0x0800
                } else {
                    addr >= 0x4000
                };
                let bit = match (text_range, page2, aux) {
                    (true, false, false) => 5,  // text page 1
                    (true, true, false) => 4,   // text page 2
                    (false, false, false) => 7, // hi-res page 1
                    (false, true, false) => 6,  // hi-res page 2
                    (true, _, true) => 3,       // aux text/lo-res
                    (false, _, true) => 2,      // aux hi-res
                };
                if self.host.ctrl3 & (1 << bit) == 0 {
                    return None;
                }
                Some(TrapKind::Display {
                    aux,
                    page2,
                    hires: !text_range,
                })
            }
            // 80STORE / 80COL / ALTCHARSET writes
            0xC000..=0xC001 | 0xC00C..=0xC00F if write => Some(TrapKind::SoftSwitch),
            // Display switches, annunciators, the host-service cells
            // ($C070–$C07F: PDLTRIG, the $C072 $5A handshake, the
            // $C078–$C07C register mailbox, $C07D the service-call
            // number), the motor and the C8 switch.
            0xC050..=0xC05F | 0xC070..=0xC07F | 0xC0E8..=0xC0E9 | 0xCFFF => {
                Some(TrapKind::of(addr))
            }
            // The game buttons, while `$21` bit 1 is set: the host
            // answers from the Mac's modifier keys.  "Restart IIe"
            // answers `$C061` with `$FF` once, so RESET sees Open-Apple
            // down and cold-starts, then clears the bit (code_5 0x4C6,
            // 0x1BB8).
            0xC061..=0xC063 if !write && self.host.ctrl3 & 0x02 != 0 => Some(TrapKind::Button),
            // Slot 1–7 hardware (except the slot-6 IWM)
            0xC090..=0xC0DF | 0xC0F0..=0xC0FF => Some(TrapKind::SlotIo),
            _ => None,
        }
    }

    /// Whether an access below `$C000` addresses the auxiliary bank.
    fn aux_side(&self, addr: u16, write: bool) -> bool {
        // `$0D` bit 2 disables the aux memory (a 64K IIe).  [INF] The
        // self-test only proves it disables ALTZP; extending it to
        // RAMRD/RAMWRT/80STORE is a guess.
        if self.host.ctrl & 0x04 != 0 {
            return false;
        }
        match addr {
            0x0000..=0x01FF => self.sw.altzp,
            0x0200..=0xBFFF => {
                if self.sw.store80 {
                    // 80STORE on (spec §4.4): PAGE2 selects the bank
                    // for the display page-1 ranges — `$0400`–`$07FF`
                    // always, `$2000`–`$3FFF` only with HIRES — and no
                    // longer selects the displayed page.  RAMRD/RAMWRT
                    // keep controlling everything else; in particular
                    // `$0800`–`$0BFF` and `$4000`–`$5FFF` do NOT move
                    // to aux.  The 80-column firmware writes the even
                    // columns with `STA $C055 / STA $04xx` (aux) and
                    // the odd with `STA $C054 / STA $04xx` (main)
                    // (main ROM `$CCD5`–`$CCE2`); the trap code reports
                    // the bank the store lands in (`$8A` vs `$82`).
                    match addr {
                        0x0400..=0x07FF => self.sw.page2,
                        0x2000..=0x3FFF if self.sw.hires => self.sw.page2,
                        _ => {
                            if write {
                                self.sw.ramwrt
                            } else {
                                self.sw.ramrd
                            }
                        }
                    }
                } else if write {
                    self.sw.ramwrt
                } else {
                    self.sw.ramrd
                }
            }
            _ => false,
        }
    }

    fn ram_read(&self, addr: u16) -> u8 {
        let base = self.ram_base(addr, false);
        self.ram[base + addr as usize]
    }

    /// The language-card window's physical index in the ALTZP-selected
    /// bank.  Two layouts behind `LC_BANK2_AT_C000`: the model's
    /// original guess (bank 1 = 12K at `$C000`, bank 2 = 4K at
    /// `$F000`) and the MAME / IIgs Mega II layout (spec §14: bank 2
    /// at `$C000`, bank 1 at `$D000`, `$E000` at `$E000`).
    /// `$E000`–`$FFFF` is bank 1's RAM whichever bank `$D000` selects.
    fn lc_index(&self, addr: u16) -> usize {
        let base = if self.sw.altzp { AUX_RAM } else { MAIN_RAM };
        if addr < 0xE000 && self.sw.lc_bank2 {
            base + if LC_BANK2_AT_C000 { 0xC000 } else { 0xF000 } + addr as usize - 0xD000
        } else {
            base + if LC_BANK2_AT_C000 { 0xD000 } else { 0xC000 } + addr as usize - 0xD000
        }
    }

    /// A side-effect-free read of the byte at `addr`, for the
    /// instruction trace (the opcode the 65C02 is about to fetch).
    /// The `$C0xx` window is the only side-effectful range, and the
    /// CPU never fetches instructions from it.
    #[cfg(feature = "iie-debug")]
    fn peek(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0xBFFF => self.ram_read(addr),
            0xC000..=0xC0FF => 0xFF,
            0xC100..=0xFFFF => self.rom_read(addr),
        }
    }

    /// Which physical bank serves a read of the ROM/RAM window, for
    /// the bank log (mirrors the `rom_read` decisions):
    /// 0 = ROM (selected bank), 1 = slot ROM, 2 = slot `$C8xx` image,
    /// 3 = main LC bank 1, 4 = main LC bank 2, 5 = aux LC bank 1,
    /// 6 = aux LC bank 2.
    #[cfg(feature = "iie-debug")]
    fn bank_tag(&self, addr: u16) -> u8 {
        match addr {
            0xC100..=0xC2FF | 0xC400..=0xC7FF if !self.sw.intcxrom => 1,
            0xC300..=0xC3FF if !self.sw.intc3rom => 1,
            0xC800..=0xCFFF if !self.sw.intcxrom && !self.sw.intc8rom && self.sw.cnxx_slot != 0 => {
                2
            }
            0xD000..=0xFFFF => {
                if self.sw.lc_read {
                    let aux = self.sw.altzp as u8;
                    if addr < 0xE000 && self.sw.lc_bank2 {
                        4 + aux * 2 // main/aux LC bank 2
                    } else {
                        3 + aux * 2 // main/aux LC bank 1
                    }
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    fn ram_write(&mut self, addr: u16, val: u8) {
        let base = self.ram_base(addr, true);
        self.ram[base + addr as usize] = val;
    }

    /// The card-RAM base for a `$0000`–`$BFFF` access.  `$C02B` bit 4
    /// with ALTZP selects a second zero page (shim `$2198`).
    ///
    /// [INF] The LC RAM layout (bank 1 `$D000` at physical `$C000`,
    /// `$E000` at `$D000`, bank 2 at `$F000`) passes the self-test but
    /// is unverified; the Mega II layout (bank 2 at `$D000`, bank 1 at
    /// `$C000`, `$E000` at `$E000`) is the other candidate.
    fn ram_base(&self, addr: u16, write: bool) -> usize {
        // [INF] `$C02B` bit 4 with ALTZP moves the zero page to the
        // second zero page at the physical top of the RAM; the
        // self-test only checks `$00`, so whether the stack follows
        // (or the whole aux moves to upper aux) is a guess.  Keeping
        // the stack on the ALTZP side avoids the old rule's
        // out-of-bounds indexing.
        if addr < 0x0100 && self.sw.altzp && self.sw.c02b & 0x10 != 0 {
            return AUX2_ZP;
        }
        if self.aux_side(addr, write) {
            AUX_RAM
        } else {
            MAIN_RAM
        }
    }

    /// The active ROM bank base for `$C000`–`$FFFF` reads, or `None`
    /// for the non-RAM `$FF` view (a `$C028` write with bit 7).
    fn rom_base(&self, addr: u16) -> Option<usize> {
        // During an interrupt only the vectors and the receiver area
        // come from the alternate bank; the rest (incl. the `$02`
        // call sites in `$F000`–`$FFFF`) stays in the selected bank.
        let int_window = self.int_bank && matches!(addr, 0xFFFA..=0xFFFF | 0xD000..=0xD3FF);
        if int_window || self.sw.alt_fw {
            Some(if self.sw.c02b & 0x08 != 0 {
                FW_D2
            } else {
                FW_D1
            })
        } else if self.sw.non_ram_bank {
            None
        } else {
            Some(FW_MAIN)
        }
    }

    /// The `$CFFF`/`$Cnxx` side effects of a CPU access (§4.4): any
    /// `$CFFF` access deselects the C8 space (INTC8ROM off, slot
    /// cleared — the classic "read `$CFFF` to release the card ROM"),
    /// and any `$Cn00`–`$CnFF` access with INTCXROM off selects slot n
    /// (for slot 3 only with SLOTC3ROM on, i.e. INTC3ROM off).  A
    /// `$C3xx` access with INTC3ROM on sets INTC8ROM instead — on the
    /// real IIe this happens regardless of INTCXROM (Sather; MAME
    /// `apple2e.cpp` `c300_int_r`).
    fn c8_select(&mut self, addr: u16) {
        if addr == 0xCFFF {
            self.sw.intc8rom = false;
            self.sw.cnxx_slot = 0;
        } else if matches!(addr, 0xC100..=0xC7FF) {
            let n = ((addr >> 8) & 0x0F) as u8;
            if n == 3 && self.sw.intc3rom {
                self.sw.intc8rom = true;
            } else if !self.sw.intcxrom {
                self.sw.cnxx_slot = n;
            }
        }
    }

    /// The RAM index that serves a `$C800`–`$CFFF` access: the internal
    /// ROM image when INTCXROM or INTC8ROM is set, otherwise the
    /// selected slot's `$C8xx` image.  With no slot selected the
    /// model deliberately falls back to the internal ROM (MAME
    /// returns the floating bus, which this model does not have; the
    /// app answers the trapped `$CFFF` reads anyway).
    /// `base` is the active ROM bank (already resolved, non-`None`).
    fn c8_index(&self, addr: u16, base: usize) -> usize {
        let n = self.sw.cnxx_slot as usize;
        if self.sw.intcxrom || self.sw.intc8rom || n == 0 {
            base + addr as usize - 0xC000
        } else {
            FW_SLOT_ROMS + n * 0x800 + (addr as usize - 0xC800)
        }
    }

    /// Reads `$C100`–`$FFFF` outside the soft-switch window: the slot-ROM
    /// areas, the ROM image and the language-card windows.
    ///
    /// The card's ROM is the 16 KB `$C000`–`$FFFF` image the Mac
    /// downloaded into the upper-main half (`FW_MAIN`, or D1/D2 after a
    /// `$C029` access).  The LC window `$D000`–`$FFFF` reads LC RAM of
    /// the ALTZP-selected bank when the LC switches enable RAM reads,
    /// otherwise the ROM image.
    fn rom_read(&self, addr: u16) -> u8 {
        let base = self.rom_base(addr);
        let Some(base) = base else {
            return 0xFF;
        };
        match addr {
            // Slot-ROM areas: the slot-ROM bank when the internal ROM is
            // switched off.
            0xC100..=0xC2FF | 0xC400..=0xC7FF if !self.sw.intcxrom => {
                self.ram[FW_SLOT_ROMS + addr as usize - 0xC000]
            }
            0xC300..=0xC3FF if !self.sw.intc3rom => self.ram[FW_SLOT_ROMS + addr as usize - 0xC000],
            // The C8 space: the internal ROM image when INTCXROM or
            // INTC8ROM is set (or no slot has been selected), otherwise
            // the selected slot's `$C8xx` image (slot n at bank
            // `$C800`+n·`$800` — slot 6 = the 5.25" Disk II code).
            0xC800..=0xCFFF => self.ram[self.c8_index(addr, base)],
            // The LC window: bank 1 = 12K (`$D000`–`$FFFF`), bank 2 =
            // 4K (`$D000`–`$DFFF`), at the physical top of the ALTZP
            // bank, when the LC switches enable RAM; otherwise the ROM
            // image.  With bank 1 selected, `$D000` shows bank 1.
            // [INF] the physical placement: only the Slinky can see
            // it, and MAME's IIgs model (spec §14) puts bank 2 at
            // `$C000` and bank 1 at `$D000` instead — this model keeps
            // its own layout until the Slinky traffic proves one.
            0xD000..=0xFFFF => {
                if self.sw.lc_read {
                    self.ram[self.lc_index(addr)]
                } else {
                    self.ram[base + addr as usize - 0xC000]
                }
            }
            _ => self.ram[base + addr as usize - 0xC000],
        }
    }

    /// Language card switch (any access): even addresses enable reading
    /// and odd addresses enable writing; `$C088`+ selects bank 1.  The
    /// IIe prewrite rule needs two accesses to the odd switches before
    /// LC RAM writes are enabled, and stays latched until reset
    /// (Sather, Understanding the Apple IIe).
    fn lc_switch(&mut self, addr: u16) {
        let a = addr & 0x0F;
        if a & 0x08 != 0 {
            self.sw.lc_prewrite = (self.sw.lc_prewrite + 1).min(2);
        }
        self.sw.lc_bank2 = a & 0x08 == 0;
        self.sw.lc_read = matches!(a & 0x03, 0 | 3);
        self.sw.lc_write = matches!(a & 0x03, 1 | 3);
    }

    /// Reads the soft-switch window `$C000`–`$C0FF` (side effects!).
    fn io_read(&mut self, addr: u16) -> u8 {
        let b7 = |v: bool| if v { 0x80 } else { 0x00 };
        match addr {
            // Keyboard: `$C000` returns the key with the strobe in bit 7
            // (no side effect); `$C010` returns AKD (`$0D` bit 7) in
            // bit 7 and clears the strobe.
            0xC000 => self.kbd | if self.kbd_strobe { 0x80 } else { 0 },
            0xC010 => {
                self.kbd_strobe = false;
                b7(self.host.ctrl & 0x80 != 0)
            }
            // The display switches are read-sensitive on the IIe: any
            // access (read or write) triggers the switch.  The card's
            // own firmware relies on it — the clear/scroll routine
            // flips PAGE2 with `BIT $C055` / `BIT $C054` around its
            // `STA ($28),Y` stores (main ROM `$CCD5`–`$CCE2`), which
            // selects the aux or main bank for the store.  The read
            // itself traps and the host answers it; the value returned
            // here never reaches the CPU.
            0xC050 => {
                self.sw.text = false;
                0
            }
            0xC051 => {
                self.sw.text = true;
                0
            }
            0xC052 => {
                self.sw.mixed = false;
                0
            }
            0xC053 => {
                self.sw.mixed = true;
                0
            }
            0xC054 => {
                self.sw.page2 = false;
                0
            }
            0xC055 => {
                self.sw.page2 = true;
                0
            }
            0xC056 => {
                self.sw.hires = false;
                0
            }
            0xC057 => {
                self.sw.hires = true;
                0
            }
            // Soft-switch state reads: bit 7 is the state, bits 0-6
            // the keyboard latch (the IIe returns it there — TN AIIE
            // #10).
            0xC011 => b7(self.sw.lc_bank2) | self.kbd & 0x7F,
            0xC012 => b7(self.sw.lc_read) | self.kbd & 0x7F,
            0xC013 => b7(self.sw.ramrd) | self.kbd & 0x7F,
            0xC014 => b7(self.sw.ramwrt) | self.kbd & 0x7F,
            // Bit 7 = 1 when the internal ROM is selected (the
            // $FBB4 thunk saves it with BIT/PHP and the $C20A exit
            // restores it with PLP/BMI/STA $C006).
            0xC015 => b7(self.sw.intcxrom) | self.kbd & 0x7F,
            0xC016 => b7(self.sw.altzp) | self.kbd & 0x7F,
            0xC017 => b7(!self.sw.intc3rom) | self.kbd & 0x7F,
            0xC018 => b7(self.sw.store80) | self.kbd & 0x7F,
            0xC019 => b7(self.host.ctrl & 0x40 != 0) | self.kbd & 0x7F,
            0xC01A => b7(self.sw.text) | self.kbd & 0x7F,
            0xC01B => b7(self.sw.mixed) | self.kbd & 0x7F,
            0xC01C => b7(self.sw.page2) | self.kbd & 0x7F,
            0xC01D => b7(self.sw.hires) | self.kbd & 0x7F,
            0xC01E => b7(self.sw.altchar) | self.kbd & 0x7F,
            0xC01F => b7(self.sw.col80) | self.kbd & 0x7F,
            // Firmware bank selects (any access switches; the IRQ
            // path's BIT $C028 returns to the main bank).
            0xC028 => {
                self.sw.alt_fw = false;
                self.sw.non_ram_bank = false;
                self.int_bank = false;
                self.kbd
            }
            0xC029 => {
                self.sw.alt_fw = true;
                self.sw.non_ram_bank = false;
                self.kbd
            }
            // Card control register.
            0xC02B => self.sw.c02b,
            // Speaker toggle.
            0xC030 => {
                self.sw.speaker = !self.sw.speaker;
                self.kbd
            }
            // Game I/O: the buttons when not trapped (`$21` bit 1
            // clear), and the paddles.
            0xC061..=0xC063 => b7(self.btn[(addr - 0xC061) as usize]),
            0xC064..=0xC067 => b7(self.paddle[(addr - 0xC064) as usize] > 0),
            // Language card switches.
            0xC080..=0xC08F => {
                self.lc_switch(addr);
                self.kbd
            }
            // The drive motor switch acts on any access.
            0xC0E8 | 0xC0E9 => {
                self.motor_on = addr == 0xC0E9;
                b7(self.motor_on)
            }
            // Unused I/O reads return the last key pressed (no floating
            // bus on the card — M.G., "Problematic Software").
            _ => self.kbd,
        }
    }

    /// Writes the soft-switch window `$C000`–`$C0FF` (side effects!).
    fn io_write(&mut self, addr: u16, val: u8) {
        match addr {
            0xC000 => self.sw.store80 = false,
            0xC001 => self.sw.store80 = true,
            0xC002 => self.sw.ramrd = false,
            0xC003 => self.sw.ramrd = true,
            0xC004 => self.sw.ramwrt = false,
            0xC005 => self.sw.ramwrt = true,
            // $C006 = SETSLOTCXROM (the $C100-$CFFF range shows the
            // slot ROMs); $C007 = SETINTCXROM (the internal ROM).
            0xC006 => self.sw.intcxrom = false,
            0xC007 => self.sw.intcxrom = true,
            0xC008 => self.sw.altzp = false,
            0xC009 => self.sw.altzp = true,
            0xC00A => self.sw.intc3rom = true,
            0xC00B => self.sw.intc3rom = false,
            0xC00C => self.sw.col80 = false,
            0xC00D => self.sw.col80 = true,
            0xC00E => self.sw.altchar = false,
            0xC00F => self.sw.altchar = true,
            0xC010 => self.kbd_strobe = false,
            // TAPEOUT: no cassette I/O on the card.
            0xC020 => {}
            0xC028 => {
                self.sw.alt_fw = false;
                self.sw.non_ram_bank = val & 0x80 != 0;
            }
            0xC029 => {
                self.sw.alt_fw = true;
                self.sw.non_ram_bank = false;
            }
            0xC02B => self.sw.c02b = val,
            0xC050 => self.sw.text = false,
            0xC051 => self.sw.text = true,
            0xC052 => self.sw.mixed = false,
            0xC053 => self.sw.mixed = true,
            0xC054 => self.sw.page2 = false,
            0xC055 => self.sw.page2 = true,
            0xC056 => self.sw.hires = false,
            0xC057 => self.sw.hires = true,
            0xC058 => self.sw.an[0] = false,
            0xC059 => self.sw.an[0] = true,
            0xC05A => self.sw.an[1] = false,
            0xC05B => self.sw.an[1] = true,
            0xC05C => self.sw.an[2] = false,
            0xC05D => self.sw.an[2] = true,
            // $C05E/$C05F: annunciator 3, doubled as DHIRES on/off in
            // 80-column mode.
            0xC05E => {
                self.sw.an[3] = false;
                self.sw.dhires = true;
            }
            0xC05F => {
                self.sw.an[3] = true;
                self.sw.dhires = false;
            }
            0xC070 => self.paddle = [PADDLE_CENTRE; 4],
            // $C072: firmware writes $5A (paired with setting $C02B
            // bits 4/5) or $00 (paired with clearing them).
            0xC072 => self.sw.c072 = val,
            // Aux memory-card bank select: host-emulated pseudo-device.
            0xC073 => {}
            // IOUDIS: not implemented on the card (TN AIIE #10).
            0xC07E | 0xC07F => {}
            0xC080..=0xC08F => self.lc_switch(addr),
            0xC0E8 => self.motor_on = false,
            0xC0E9 => self.motor_on = true,
            // `$CFFF` writes just deselect the C8 space (see
            // `c8_select`, applied before the trap in `write`).
            _ => {}
        }
    }

    /// Latches a trap (keeps the first one until the host resumes).
    /// The reported address follows the driver's convention: slot
    /// traps carry the compacted `(slot << 4) | register`, everything
    /// else the IIe address.  The code carries `$0D` bits 1:0 in bits
    /// 7:6 and, for display stores, the `$20` bit 4 motor state.
    fn latch(&mut self, kind: TrapKind, addr: u16, data: u8, write: bool) {
        if self.trap.is_none() {
            // A new trap supersedes any pending retry answer (the
            // retry is scoped to the instruction being re-executed).
            self.retry = None;
            let reported = match kind {
                TrapKind::SlotIo => (((addr >> 4) & 0x07) << 4) | (addr & 0x000F),
                _ => addr,
            };
            self.trap = Some(Trap {
                code: kind.code(write, self.host.ctrl, self.host.ctrl2),
                addr: reported,
                raw_addr: addr,
                data,
                write,
            });
        }
    }

    /// A 65C02 bus read, with the trap check.
    fn read(&mut self, addr: u16) -> u8 {
        // A re-executed instruction re-reads the trapped address:
        // answer it from the retry latch.  The answer is kept for the
        // whole instruction — INC/DEC and the other read-modify-write
        // instructions read the operand twice, and both reads must be
        // answered (the slot 7 memory-card probe does
        // `DEC $BFF8,X` on a trapped address).  The
        // card clears the latch when the instruction completes (tick)
        // or a new trap latches.
        if let Some((a, v)) = self.retry
            && a == addr
        {
            return v;
        }

        // The C8-space selection applies in hardware even to trapped
        // accesses (see `c8_select`).  The retry path above returns
        // before this: the re-executed instruction already applied the
        // side effects on its first (trapping) pass, and `$CFFF`
        // deselection is idempotent anyway.
        self.c8_select(addr);

        let v = if let Some(kind) = self.trap_kind(addr, false) {
            // The access still takes effect where the Gemini applies it
            // in hardware: the read-sensitive display switches.
            if matches!(addr, 0xC000..=0xC0FF) {
                self.io_read(addr);
            }
            self.latch(kind, addr, 0, false);
            0xFF
        } else {
            match addr {
                0x0000..=0xBFFF => self.ram_read(addr),
                // The soft-switch window; the rest of the ROM image
                // ($C100+) reads through rom_read.
                0xC000..=0xC0FF => self.io_read(addr),
                0xC100..=0xFFFF => self.rom_read(addr),
            }
        };

        // The 65C02's own accesses to its devices, and which physical
        // bank the ROM/RAM window served (deduplicated per (address,
        // value) to keep the loops small).
        #[cfg(feature = "iie-debug")]
        if log_enabled!(Level::Debug) {
            if matches!(addr, 0xC000..=0xC0FF) && self.last_io != Some((0, addr, v)) {
                self.last_io = Some((0, addr, v));
                log::debug!("IIe Card io read {:04X} -> {:02X}", addr, v);
            }
            if matches!(addr, 0x0200..=0xBFFF) {
                // Which RAM bank served the access (the aux_side rule),
                // to catch bank-mapping errors in relocated code.
                let tag = if self.aux_side(addr, false) { 1 } else { 0 };
                if self.last_ram != Some((addr, tag)) {
                    self.last_ram = Some((addr, tag));
                    log::debug!(
                        "IIe Card ram read {:04X} <- {:02X} ({})",
                        addr,
                        v,
                        if tag == 0 { "main" } else { "aux" }
                    );
                }
            }
            if matches!(addr, 0xD000..=0xFFFF) {
                let tag = self.bank_tag(addr);
                if self.last_bank != Some((addr, tag)) {
                    self.last_bank = Some((addr, tag));
                    log::debug!(
                        "IIe Card bank read {:04X} <- {:02X} ({})",
                        addr,
                        v,
                        BANK_NAMES[tag as usize]
                    );
                }
            }
        }
        v
    }

    /// A 65C02 bus write, with the trap check.
    fn write(&mut self, addr: u16, val: u8) {
        // The C8-space selection applies in hardware even to trapped
        // accesses (see `c8_select`).
        self.c8_select(addr);

        if let Some(kind) = self.trap_kind(addr, true) {
            // The access takes effect and is latched for the host.
            if matches!(addr, 0x0400..=0x0BFF | 0x2000..=0x5FFF) {
                self.ram_write(addr, val);
            } else {
                self.io_write(addr, val);
            }
            self.latch(kind, addr, val, true);
            #[cfg(feature = "iie-debug")]
            if log_enabled!(Level::Debug)
                && matches!(addr, 0xC000..=0xC0FF)
                && self.last_io != Some((1, addr, val))
            {
                self.last_io = Some((1, addr, val));
                log::debug!("IIe Card io write {:04X} <- {:02X}", addr, val);
            }
            return;
        }

        match addr {
            0x0000..=0xBFFF => self.ram_write(addr, val),
            0xC000..=0xC0FF => self.io_write(addr, val),
            // The LC write windows, or the ROM view write-through.
            0xD000..=0xFFFF => {
                if self.sw.lc_write && self.sw.lc_prewrite >= 2 {
                    let i = self.lc_index(addr);
                    self.ram[i] = val;
                } else if self.host.ctrl & 0x10 != 0 {
                    // `$0D` bit 4 (ROM write enable): the write lands in
                    // the active ROM bank (the PFI code writes its own
                    // alternate bank this way, after the `$C072` `$5A`
                    // handshake; shim `$20F6`).
                    if let Some(base) = self.rom_base(addr) {
                        self.ram[base + addr as usize - 0xC000] = val;
                    }
                }
            }
            // C8-space writes with `$C02B` bit 5 set land in the served
            // image.  The evidence is the PFI: it sets `$C02B` bits
            // 4+5 and then uses `$C800`–`$CFFF` as scratch RAM (it
            // saves zero page `$80`–`$93` to `$CF94,X`, among others).
            // No slot firmware writes its C8 space — the slot-2
            // `INC $CFA0` is part of a copyright string.  The
            // interrupt entry has already set INTCXROM by then, so the
            // writes land in the internal C8.
            0xC800..=0xCFFF => {
                if self.sw.c02b & 0x20 != 0
                    && let Some(base) = self.rom_base(addr)
                {
                    let i = self.c8_index(addr, base);
                    self.ram[i] = val;
                    #[cfg(feature = "iie-debug")]
                    if log_enabled!(Level::Debug) {
                        // Catch a write landing in the main ROM's C8
                        // pages while only the interrupt-window bank
                        // is active — that would corrupt the
                        // 80-column firmware.
                        let int_win =
                            self.int_bank && matches!(addr, 0xFFFA..=0xFFFF | 0xD000..=0xD3FF);
                        if (FW_MAIN + 0x800..FW_MAIN + 0x1000).contains(&i) {
                            log::debug!(
                                "IIe Card c8 write {:04X} <- {:02X} (main ROM C8, int_win {})",
                                addr,
                                val,
                                int_win
                            );
                        }
                    }
                }
            }
            // Writes to the ROM image area are ignored (read-only to
            // the 65C02; the host loads it through the Slinky).
            _ => {}
        }
        #[cfg(feature = "iie-debug")]
        if log_enabled!(Level::Debug)
            && matches!(addr, 0xC000..=0xC0FF)
            && self.last_io != Some((1, addr, val))
        {
            self.last_io = Some((1, addr, val));
            log::debug!("IIe Card io write {:04X} <- {:02X}", addr, val);
        }
    }

    /// The packed video-switch byte for the host register `$08` (the
    /// self-test checks `$FF` after the eight "set" writes and `$00`
    /// after the eight "clear" writes).  The bit order is the driver's
    /// (its switch handlers set/clear the mirror bits at init,
    /// code_5 0x2D4): bit 7 ALTCHAR, 6 TEXT, 5 HIRES, 4 AN3, 3 80COL,
    /// 2 MIXED, 1 PAGE2, 0 80STORE.
    fn video_switches(&self) -> u8 {
        (self.sw.altchar as u8) << 7
            | (self.sw.text as u8) << 6
            | (self.sw.hires as u8) << 5
            | (self.sw.an[3] as u8) << 4
            | (self.sw.col80 as u8) << 3
            | (self.sw.mixed as u8) << 2
            | (self.sw.page2 as u8) << 1
            | (self.sw.store80 as u8)
    }

    /// Soft reset (a released 65C02 reset or a Mac-side reset).
    fn soft_reset(&mut self) {
        self.sw = SoftSwitches::default();
        self.motor_on = false;
        self.trap = None;
        self.retry = None;
        self.kbd = 0;
        self.kbd_strobe = false;
        self.paddle = [0; 4];
        self.btn = [false; 3];
        self.irq = false;
        self.nmi = false;
        self.int_bank = false;
        self.hold = false;
    }
}

/// The 65C02's view of the card: the IIe memory model, the soft switches
/// and the Gemini trap logic.
///
/// Owned by the [`Cpu`]; the mutable state lives behind a `RefCell`
/// because [`Bus6502::read`] takes `&self` but has side effects (traps,
/// keyboard strobe, switches).
#[derive(Serialize, Deserialize)]
pub struct PdsIIeBus {
    /// The card's state, shared with the host side.
    state: RefCell<BusState>,
}

impl PdsIIeBus {
    fn new() -> Self {
        Self {
            state: RefCell::new(BusState {
                ram: vec![0; RAM_SIZE],
                ..BusState::default()
            }),
        }
    }
}

impl Bus6502 for PdsIIeBus {
    fn read(&self, addr: Address, kind: ReadKind) -> u8 {
        let mut s = self.state.borrow_mut();
        let val = s.read(addr as u16);
        // The Gemini sees the opcode fetch (SYNC).  To the 65C02, `$02`
        // is a 2-byte NOP: it runs it and takes the NMI after it, so the
        // stacked return address is just past the `nn` operand, which
        // the receiver reads back.  [INF] when exactly the Gemini
        // asserts NMI; it only has to be before the NOP ends.
        if kind == ReadKind::Opcode && val == 0x02 {
            s.nmi = true;
        }
        val
    }

    fn write(&mut self, addr: Address, val: u8) {
        self.state.borrow_mut().write(addr as u16, val);
    }

    fn get_nmi(&mut self) -> bool {
        self.state.borrow().nmi
    }

    fn get_int(&mut self) -> bool {
        self.state.borrow().irq
    }

    /// The Gemini routes NMI/IRQ vector fetches and receiver code to
    /// the alternate firmware bank; an RTI or a `$C028` access
    /// switches back (hypothesis for the NMI path).
    fn set_int_bank(&mut self, on: bool) {
        let mut s = self.state.borrow_mut();
        // Taking an interrupt: the NMI, if asserted, is the one taken
        // (it has priority over the IRQ), so release it.
        if on {
            s.nmi = false;
        }
        if s.int_bank != on {
            iie_debug!(
                "IIe Card interrupt bank {}",
                if on {
                    "on (alternate firmware)"
                } else {
                    "off (main)"
                }
            );
            s.int_bank = on;
        }
    }

    /// The 65C02 addresses 16 bits.
    fn get_mask(&self) -> Address {
        0xFFFF
    }

    /// Peripheral (non-CPU) time in 65C02 cycles: the paddle timers.
    fn tick(&mut self, cycles: Ticks) -> Result<Ticks> {
        let mut s = self.state.borrow_mut();
        for p in &mut s.paddle {
            let c = (*p).min(cycles as u16);
            *p -= c;
        }
        Ok(cycles)
    }

    fn is_suspended(&self) -> bool {
        self.state.borrow().trap.is_some()
    }
}

/// The Apple IIe Card for the Macintosh LC family: the 65C02 and its bus,
/// the host register block, and the C16M clock divider.
#[derive(Serialize, Deserialize)]
pub struct PdsIIeCard {
    /// The 65C02, which owns the card's bus.
    pub(crate) cpu: Cpu<PdsIIeBus>,

    /// Data-out (`$19`): the value returned for a trapped read.
    data_out: u8,
    /// Slinky address (`$1C`).
    mem_addr: u16,
    /// C16M ticks received, scaled by the CPU clock in Hz, less the 65C02
    /// cycles run, scaled by C16M in Hz. It goes below zero when an
    /// instruction takes more cycles than the ticks received.
    clock_acc: i64,
    /// Last (code, address) pair logged for a trap, to keep the
    /// repeated-trap debug output to the distinct cases.
    #[cfg(feature = "iie-debug")]
    #[serde(skip, default)]
    last_trap_log: (u8, u16),
    /// Instruction trace ring: the last
    /// `TRACE_DEPTH` executed instructions, dumped to the log when the
    /// 65C02 executes a BRK (a firmware crash or a deliberate
    /// software call into the monitor) or first enters the `$C8xx`
    /// disk-ROM space or the `$A000`–`$BFFF` RAM (a jump into
    /// unloaded memory).
    #[cfg(feature = "iie-debug")]
    #[serde(skip, default)]
    trace: std::collections::VecDeque<(u16, u8, u8, u8, u8, u8)>,
    /// The previous recorded PC, for edge-triggering the trace dumps.
    #[cfg(feature = "iie-debug")]
    #[serde(skip, default)]
    trace_prev_pc: Option<u16>,
    /// The full instruction stream, for offline analysis: written to the
    /// file named by `SNOW_IIE_TRACE`, restarted at every 65C02 boot (a
    /// line per instruction makes the boot very slow).
    #[cfg(feature = "iie-debug")]
    #[serde(skip, default)]
    trace_file: Option<std::fs::File>,
}

/// How many instructions the BRK trace ring keeps.
#[cfg(feature = "iie-debug")]
const TRACE_DEPTH: usize = 8192;

/// The language-card physical layout (the A/B test switch): false =
/// this model's original guess (bank 1 at `$C000`, bank 2 at `$F000`),
/// true = the MAME / IIgs Mega II layout (spec §14: bank 2 at `$C000`,
/// bank 1 at `$D000`, `$E000` at `$E000`).
const LC_BANK2_AT_C000: bool = false;

/// The bank names for the bank log (`bank_tag`).
#[cfg(feature = "iie-debug")]
const BANK_NAMES: [&str; 7] = ["rom", "slot", "slot-c8", "lc1", "lc2", "aux-lc1", "aux-lc2"];

impl PdsIIeCard {
    /// Creates a card with its 65C02 stopped.  The host loads the
    /// firmware and sets control register 2 bit 7 to run it.
    pub fn new() -> Self {
        let cpu = Cpu::new(PdsIIeBus::new(), Variant::M65C02);
        Self {
            cpu,
            data_out: 0,
            mem_addr: 0,
            clock_acc: 0,
            #[cfg(feature = "iie-debug")]
            last_trap_log: (0, 0),
            #[cfg(feature = "iie-debug")]
            trace: std::collections::VecDeque::new(),
            #[cfg(feature = "iie-debug")]
            trace_prev_pc: None,
            #[cfg(feature = "iie-debug")]
            trace_file: None,
        }
    }

    /// Reads one bus cycle of `len` bytes (1 or 2) from the host
    /// register block.  The Slinky (`$10`/`$11`) transfers once per
    /// cycle: a word cycle moves main and aux at the same address, a
    /// byte cycle moves one lane — either way the pointer advances
    /// once.  Returns `None` outside the block.
    pub fn host_read(&mut self, offset: u16, len: u32) -> Option<u32> {
        // The Slinky handles the cycle as a whole.
        if offset & !1 == 0x10 {
            let s = self.cpu.bus.state.borrow();
            let (bm, ba) = self.slinky_bases();
            let a = self.mem_addr as usize;
            let v: u32 = match (len, offset & 1) {
                (2.., _) => ((s.ram[bm + a] as u32) << 8) | s.ram[ba + a] as u32,
                (_, 0) => s.ram[bm + a] as u32,
                _ => s.ram[ba + a] as u32,
            };
            // What the host reads back through the Slinky (the driver
            // renders the IIe screen this way): the lane addresses, the
            // `$0D` half and the values.
            iie_debug!(
                "IIe Card slinky read {:04X} (main {:02X} aux {:02X}, half {})",
                a,
                s.ram[bm + a],
                s.ram[ba + a],
                s.host.ctrl & 0x08
            );
            self.mem_addr = self.mem_addr.wrapping_add(1);
            return Some(v);
        }

        let mut v = 0u32;
        for i in 0..len {
            v = v << 8 | u32::from(self.host_read_byte(offset.wrapping_add(i as u16))?);
        }

        // A display trap taken with the motor bit set (an odd display
        // code) is released by the read of its address, not by an ack:
        // the driver's motor-on handlers read `$04` to mark the cell
        // dirty and never ack (code_5 0x1F5E, 0x2106), and the
        // self-test reads `$04` and then waits for the next store's
        // code before its ack (Conf 1000 0x148).
        if (offset..offset + len as u16).contains(&0x05) {
            let mut s = self.cpu.bus.state.borrow_mut();
            if s.trap
                .is_some_and(|t| t.code & 0x01 != 0 && t.code & 0x06 != 0)
            {
                s.trap = None;
            }
        }
        Some(v)
    }

    /// One byte of the host register block (big-endian 16-bit registers
    /// at the even offsets).
    fn host_read_byte(&self, offset: u16) -> Option<u8> {
        let reg = offset & !1;
        let hi = offset & 1 == 0;
        let s = self.cpu.bus.state.borrow();
        let v: u16 = match reg {
            // Bits 7:6 come from the current $0D bits 1:0, so a $0D
            // write that lands while the trap is latched shows up in
            // the next code read (the self-test relies on this: the
            // 68k sets $0D=3 right after the ack).
            0x00 => s
                .trap
                .map_or(0, |t| (t.code & 0x3F) | ((s.host.ctrl & 0x03) << 6))
                as u16,
            0x04 => s.trap.map_or(0, |t| t.addr),
            // $08: the packed video-switch byte (high byte); $09 bit 7
            // = the keyboard strobe (low byte).
            0x08 => {
                if hi {
                    (s.video_switches() as u16) << 8
                } else {
                    (s.kbd_strobe as u16) << 7
                }
            }
            // $0C high byte: trap data; $0D low byte: the control
            // register.
            0x0C => {
                if hi {
                    (s.trap.map_or(0, |t| t.data) as u16) << 8
                } else {
                    s.host.ctrl as u16
                }
            }
            0x14 => 0,
            0x18 => (self.kbd_byte(&s) as u16) << 8 | self.data_out as u16,
            0x1C => self.mem_addr,
            0x20 => (s.host.ctrl2 as u16) << 8 | s.host.ctrl3 as u16,
            _ => return None,
        };
        Some(if hi { (v >> 8) as u8 } else { v as u8 })
    }

    /// Writes one bus cycle of `len` bytes to the host register block.
    /// The Slinky transfers once per cycle (see [`Self::host_read`]).
    pub fn host_write(&mut self, offset: u16, len: u32, val: u32) -> Option<()> {
        if offset & !1 == 0x10 {
            let (bm, ba) = self.slinky_bases();
            let a = self.mem_addr as usize;
            {
                let mut s = self.cpu.bus.state.borrow_mut();
                match (len, offset & 1) {
                    (2.., _) => {
                        s.ram[bm + a] = (val >> 8) as u8;
                        s.ram[ba + a] = val as u8;
                    }
                    (_, 0) => s.ram[bm + a] = val as u8,
                    _ => s.ram[ba + a] = val as u8,
                }
            }
            self.mem_addr = self.mem_addr.wrapping_add(1);
            return Some(());
        }

        for i in 0..len {
            let off = offset.wrapping_add(i as u16);
            let b = (val >> (8 * (len - 1 - i))) as u8;
            self.host_write_byte(off, b)?;
        }
        Some(())
    }

    /// One byte of the host register block.
    fn host_write_byte(&mut self, offset: u16, val: u8) -> Option<()> {
        let reg = offset & !1;
        let hi = offset & 1 == 0;
        match reg {
            0x00 | 0x04 | 0x08 | 0x14 => {}
            0x0C => {
                if hi {
                    // $0C high byte: the trap ack/resume.  Release the
                    // trapped CPU; a trapped read re-executes and
                    // consumes the data-out value.
                    let mut s = self.cpu.bus.state.borrow_mut();
                    if s.pending_boot {
                        if s.host.ctrl2 & 0x08 != 0 {
                            // $20 bit 3 set: the fetch completes and the
                            // CPU boots.
                            s.pending_boot = false;
                            s.trap = None;
                            drop(s);
                            self.cpu.reset();
                            return Some(());
                        }
                        // Bit 3 not set yet: keep the fetch trapped (the
                        // CPU was never reset at the run edge, so a
                        // normal ack would let it run from a stale PC).
                        return Some(());
                    }
                    // An odd display trap waits for the read of its
                    // address instead (see `host_read`).
                    if s.trap
                        .is_some_and(|t| t.code & 0x01 != 0 && t.code & 0x06 != 0)
                    {
                        return Some(());
                    }
                    if let Some(t) = s.trap.take()
                        && !t.write
                    {
                        s.retry = Some((t.raw_addr, self.data_out));
                    }
                } else {
                    // $0D low byte: the control register.
                    let mut s = self.cpu.bus.state.borrow_mut();
                    s.host.ctrl = val;
                }
            }
            0x18 => {
                if hi {
                    let mut s = self.cpu.bus.state.borrow_mut();
                    s.kbd = val & 0x7F;
                    s.kbd_strobe = true;
                    // Dump the stack on the keypress, to see the call
                    // chain the 65C02 has when it processes the key.
                    #[cfg(feature = "iie-debug")]
                    if log_enabled!(Level::Debug) {
                        log::debug!(
                            "IIe Card keypress {:02X}: stack top (S {:02X}):",
                            val,
                            self.cpu.regs.s
                        );
                        for i in 0..24 {
                            let a = 0x0100 + ((self.cpu.regs.s as usize + 1 + i) & 0xFF);
                            log::debug!("  {:04X} {:02X}", a, s.ram[MAIN_RAM + a]);
                        }
                    }
                } else {
                    self.data_out = val;
                    // The answer completes a trapped read: the CPU
                    // resumes and the re-executed access consumes it
                    // (the driver's read handlers write $19 without a
                    // separate ack).
                    let mut s = self.cpu.bus.state.borrow_mut();
                    if let Some(t) = s.trap
                        && !t.write
                    {
                        s.retry = Some((t.raw_addr, val));
                        s.trap = None;
                    }
                }
            }
            0x1C => {
                if hi {
                    self.mem_addr = (self.mem_addr & 0x00FF) | (val as u16) << 8;
                } else {
                    self.mem_addr = (self.mem_addr & 0xFF00) | val as u16;
                }
            }
            0x20 => {
                if hi {
                    let mut s = self.cpu.bus.state.borrow_mut();
                    let running = s.host.ctrl2 & 0x80 != 0;
                    let held = running && s.host.ctrl2 & 0x08 == 0;
                    s.host.ctrl2 = val;
                    if !running && val & 0x80 != 0 {
                        // The 0→1 edge of bit 7: reset the IIe state
                        // and start the 65C02.  While bit 3 is clear
                        // the reset vector fetch traps first (address
                        // $FFFC); the host sets bit 3 and acks to let
                        // the fetch complete.
                        s.soft_reset();
                        // Start a fresh full instruction trace per boot.
                        #[cfg(feature = "iie-debug")]
                        {
                            self.trace_file = std::env::var_os("SNOW_IIE_TRACE")
                                .and_then(|p| std::fs::File::create(p).ok());
                        }
                        if val & 0x08 != 0 {
                            drop(s);
                            self.cpu.reset();
                        } else {
                            s.pending_boot = true;
                            // The code is made up: the self-test and the
                            // driver check only the address ($FFFC).
                            s.trap = Some(Trap {
                                code: 0x21 | ((s.host.ctrl & 0x03) << 6),
                                addr: 0xFFFC,
                                raw_addr: 0xFFFC,
                                data: 0,
                                write: false,
                            });
                        }
                    } else if running && val & 0x80 == 0 {
                        // Stopping the 65C02 resets the IIe state too
                        // [INF]: "Restart IIe" (code_5 0x450) stops it,
                        // reads $08 into the driver's switch copy, then
                        // starts it, so $08 must read $00 by then. With
                        // the switches of the previous run (TEXT on), the
                        // banner came out lo-res.
                        s.soft_reset();
                    } else if running && val & 0x80 != 0 {
                        // Still running: bit 3 = CPU run/hold [INF]:
                        // clear holds the 65C02 at its next bus cycle
                        // (the driver brackets every `$21` rewrite with
                        // it, code_5 0x81A); set releases it.  A trap
                        // latched before the hold stays pending.
                        if held && val & 0x08 != 0 {
                            s.hold = false;
                        } else if !held && val & 0x08 == 0 {
                            s.hold = true;
                        }
                    }
                } else {
                    let mut s = self.cpu.bus.state.borrow_mut();
                    s.host.ctrl3 = val;
                }
            }
            _ => return None,
        }
        Some(())
    }

    /// The card-RAM bases of the two Slinky lanes, per `$0D` bit 3: the
    /// lower 128 KB (main + aux) or the upper 128 KB (the ROM images
    /// and the upper-aux half).
    fn slinky_bases(&self) -> (usize, usize) {
        if self.cpu.bus.state.borrow().host.ctrl & 0x08 != 0 {
            (FW_MAIN, UPPER_AUX)
        } else {
            (MAIN_RAM, AUX_RAM)
        }
    }

    /// The keyboard byte as the host would read it from register `$18`.
    fn kbd_byte(&self, s: &BusState) -> u8 {
        s.kbd | if s.kbd_strobe { 0x80 } else { 0 }
    }

    /// Whether the 65C02 is currently suspended in a trap.
    pub fn suspended(&self) -> bool {
        self.cpu.bus.state.borrow().trap.is_some()
    }

    /// The latched trap code (0 when not trapped).
    pub fn trap_code(&self) -> u8 {
        self.cpu.bus.state.borrow().trap.map_or(0, |t| t.code)
    }

    /// The latched trap address.
    pub fn trap_addr(&self) -> u16 {
        self.cpu.bus.state.borrow().trap.map_or(0, |t| t.addr)
    }

    /// Loads firmware into a bank through the host path, at the load
    /// address from a Monx resource header (the firmware offset,
    /// relative to card `0x20000`): `$0000`–`$3FFF` = the main ROM
    /// (FW_MAIN), `$4000`–`$7FFF` = D1, `$8000`–`$BFFF` = D2,
    /// `$C000`–`$FFFF` = the slot ROM bank.
    pub fn load_firmware(&mut self, load_addr: u16, data: &[u8]) -> Result<()> {
        let win = (load_addr & 0xC000) as usize;
        let base = match win {
            0x0000 => FW_MAIN,
            0x4000 => FW_D1,
            0x8000 => FW_D2,
            0xC000 => FW_SLOT_ROMS,
            _ => anyhow::bail!("no firmware bank for load address ${load_addr:04X}"),
        };
        let start = base + (load_addr as usize - win);
        anyhow::ensure!(
            start + data.len() <= base + 0x4000,
            "firmware write crosses a bank boundary"
        );
        let mut s = self.cpu.bus.state.borrow_mut();
        s.ram[start..start + data.len()].copy_from_slice(data);
        Ok(())
    }

    /// Resets the card.  A hard reset also clears the card RAM; either
    /// way the 65C02 is stopped until the host sets control register 2
    /// bit 7.
    pub fn reset(&mut self, hard: bool) {
        {
            let mut s = self.cpu.bus.state.borrow_mut();
            if hard {
                s.ram.fill(0);
            }
            s.soft_reset();
            s.host.ctrl = 0;
            s.host.ctrl2 = 0x00;
            s.host.ctrl3 = 0;
            s.pending_boot = false;
        }
        self.data_out = 0;
        self.mem_addr = 0;
        self.clock_acc = 0;
        self.cpu.reset();
    }

    /// The 65C02 and its bus, for the debugger and tests.
    pub fn cpu(&self) -> &Cpu<PdsIIeBus> {
        &self.cpu
    }
}

impl Default for PdsIIeCard {
    fn default() -> Self {
        Self::new()
    }
}

impl Tickable for PdsIIeCard {
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        let mut s = self.cpu.bus.state.borrow_mut();
        // `$20` bit 5 asserts the 65C02's IRQ (host-driven).
        s.irq = s.host.ctrl2 & 0x20 != 0;
        // Stopped until control register 2 bit 7 is set.
        let run = s.host.ctrl2 & 0x80 != 0;
        let fast = s.sw.c02b & 0x04 != 0;
        drop(s);
        if !run {
            return Ok(ticks);
        }

        let hz = if fast { FAST_HZ } else { NORMAL_HZ };

        self.clock_acc += ticks as i64 * hz;
        while self.clock_acc >= C16M_HZ {
            self.clock_acc -= C16M_HZ;
            #[cfg(feature = "iie-debug")]
            let prev = {
                let s = self.cpu.bus.state.borrow();
                // The instruction about to run, for the BRK trace ring.
                let trace = log_enabled!(Level::Debug).then(|| {
                    (
                        self.cpu.regs.pc,
                        s.peek(self.cpu.regs.pc),
                        self.cpu.regs.c as u8,
                        self.cpu.regs.x as u8,
                        self.cpu.regs.y as u8,
                        self.cpu.regs.s as u8,
                    )
                });
                (s.trap.is_some(), s.int_bank, trace)
            };
            // `$20` bit 3 hold [INF]: while held the CPU makes no bus
            // cycles.  Nothing is latched: the driver saves the `$01`
            // it reads during the hold and dispatches it afterwards
            // as a pending trap (code_5 0x82C, 0x175E), so a hold
            // with no trap pending must read 0.
            let held = self.cpu.bus.state.borrow().hold;
            if !held {
                // One CPU cycle's worth of host time was charged on the
                // loop entry; charge the rest of the instruction's cycles
                // (step() returns the count; 0 while suspended).
                let cyc = self.cpu.step()?;
                self.clock_acc -= cyc.saturating_sub(1) as i64 * C16M_HZ;
                if cyc > 0 {
                    // An instruction completed: a trapped-read answer
                    // (retry) is scoped to the re-executed instruction
                    // only — RMW instructions read the trapped address
                    // twice before finishing, so it cannot be consumed
                    // on first use.  Clear it at the instruction
                    // boundary (the next opcode fetch).
                    let mut s = self.cpu.bus.state.borrow_mut();
                    s.retry = None;
                }
                // An instruction actually executed: record it in the
                // trace ring and dump the ring on a BRK (a firmware
                // crash or a deliberate call into the monitor) or on
                // the first entry into the $C8xx disk-ROM space or the
                // $A000-$BFFF RAM (a jump into unloaded memory).
                #[cfg(feature = "iie-debug")]
                if let Some((pc, op, a, x, y, sp)) = prev.2
                    && cyc > 0
                {
                    // The full instruction stream, for offline analysis.
                    if let Some(f) = self.trace_file.as_mut() {
                        use std::io::Write;
                        let _ = writeln!(
                            f,
                            "{:04X} {:02X} A {:02X} X {:02X} Y {:02X} S {:02X} P {:02X}",
                            pc, op, a, x, y, sp, self.cpu.regs.p
                        );
                    }
                    let prev_pc = self.trace_prev_pc.replace(pc);
                    let entered = |lo: u16, hi: u16| {
                        (lo..=hi).contains(&pc) && prev_pc.is_none_or(|p| !(lo..=hi).contains(&p))
                    };
                    self.trace.push_back((pc, op, a, x, y, sp));
                    if self.trace.len() > TRACE_DEPTH {
                        self.trace.pop_front();
                    }
                    if op == 0x00
                        || entered(0xC800, 0xCFFF)
                        || entered(0xA000, 0xBFFF)
                        || entered(0x4000, 0x5FFF)
                    {
                        log::debug!(
                            "IIe Card trace dump at {:04X} (op {:02X}, A {:02X} X {:02X} Y {:02X} S {:02X}) — last instructions:",
                            pc,
                            op,
                            a,
                            x,
                            y,
                            sp
                        );
                        for (p, o, ra, rx, ry, rs) in &self.trace {
                            log::debug!(
                                "IIe Card trace {:04X} {:02X} A {:02X} X {:02X} Y {:02X} S {:02X}",
                                p,
                                o,
                                ra,
                                rx,
                                ry,
                                rs
                            );
                        }
                        self.trace.clear();
                    }
                }
            }
            #[cfg(feature = "iie-debug")]
            {
                let s = self.cpu.bus.state.borrow();
                if s.int_bank && !prev.1 {
                    log::debug!("IIe Card interrupt taken, pc {:04X}", self.cpu.regs.pc);
                }
                if let Some(t) = s.trap
                    && !prev.0
                    && (t.code, t.addr) != self.last_trap_log
                {
                    self.last_trap_log = (t.code, t.addr);
                    log::debug!(
                        "IIe Card trap {:02X} @ {:04X} from pc {:04X}",
                        t.code,
                        t.addr,
                        self.cpu.regs.pc
                    );
                }
            }
        }
        Ok(ticks)
    }
}

impl Debuggable for PdsIIeCard {
    fn get_debug_properties(&self) -> DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_byte, dbgprop_byte_bin, dbgprop_header, dbgprop_udec, dbgprop_word};

        let r = &self.cpu.regs;
        let s = self.cpu.bus.state.borrow();
        vec![
            dbgprop_header!("Apple IIe Card"),
            dbgprop_udec!("CPU cycles", self.cpu.cycles),
            dbgprop_word!("PC", r.pc),
            dbgprop_byte!("A", r.c as u8),
            dbgprop_byte!("X", r.x as u8),
            dbgprop_byte!("Y", r.y as u8),
            dbgprop_byte!("S", r.s as u8),
            dbgprop_byte_bin!("P", r.p),
            dbgprop_byte_bin!("$C02B", s.sw.c02b),
            dbgprop_byte_bin!("Ctrl 2", s.host.ctrl2),
            dbgprop_byte_bin!("Ctrl 3", s.host.ctrl3),
            dbgprop_byte!("Trap code", s.trap.map_or(0, |t| t.code)),
            dbgprop_word!("Trap address", s.trap.map_or(0, |t| t.addr)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a big-endian 16-bit value to a host register as one word
    /// cycle (the card is a 16-bit port).
    fn hw16(card: &mut PdsIIeCard, reg: u16, val: u16) {
        card.host_write(reg, 2, val as u32).unwrap();
    }

    /// Reads a big-endian 16-bit host register as one word cycle.
    fn hr16(card: &mut PdsIIeCard, reg: u16) -> u16 {
        card.host_read(reg, 2).unwrap() as u16
    }

    /// Loads a 65C02 program into main RAM through the Slinky: the
    /// address is set once and the bytes are streamed, exercising the
    /// per-cycle auto-increment.
    fn load_program(card: &mut PdsIIeCard, addr: u16, bytes: &[u8]) {
        hw16(card, 0x1C, addr);
        for b in bytes {
            card.host_write(0x10, 1, *b as u32).unwrap();
        }
    }

    /// Starts a card running a program at `addr` (the $20 bit 7 edge
    /// with bit 3 set boots the 65C02 directly, without the
    /// reset-vector-fetch trap; the PC is then forced for the test).
    fn run_program(card: &mut PdsIIeCard, addr: u16) {
        hw16(card, 0x20, 0x8800);
        card.cpu.reset_pc(addr);
    }

    /// Runs the 65C02 for `steps` CPU instructions worth of time.
    fn run(card: &mut PdsIIeCard, steps: usize) {
        for _ in 0..steps * 16 {
            card.tick(16, ()).unwrap();
        }
    }

    #[test]
    fn host_register_block() {
        let mut card = PdsIIeCard::new();

        // Slinky: one transfer per bus cycle — a word cycle moves main
        // and aux at the same address, a byte cycle moves one lane —
        // with one address increment per cycle.
        hw16(&mut card, 0x1C, 0x0400);
        card.host_write(0x10, 2, 0x55AA).unwrap();
        assert_eq!(hr16(&mut card, 0x1C), 0x0401);
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0x0400], 0x55);
            assert_eq!(s.ram[AUX_RAM + 0x0400], 0xAA);
        }
        // Byte accesses move one lane each, also one increment.
        hw16(&mut card, 0x1C, 0x0400);
        card.host_write(0x10, 1, 0x11).unwrap();
        card.host_write(0x11, 1, 0x22).unwrap();
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0x0400], 0x11);
            assert_eq!(s.ram[AUX_RAM + 0x0401], 0x22);
        }
        assert_eq!(hr16(&mut card, 0x1C), 0x0402);
        // Reads increment too.
        hw16(&mut card, 0x1C, 0x0400);
        assert_eq!(card.host_read(0x10, 1).unwrap(), 0x11);
        assert_eq!(card.host_read(0x11, 1).unwrap(), 0x22);
        assert_eq!(hr16(&mut card, 0x1C), 0x0402);

        // Keyboard through $18 high byte.
        card.host_write(0x18, 1, 0x41).unwrap();
        assert_eq!(card.host_read(0x18, 1).unwrap(), 0xC1); // key | strobe
        // Control registers round-trip.
        hw16(&mut card, 0x20, 0x1234);
        assert_eq!(hr16(&mut card, 0x20), 0x1234);
    }

    #[test]
    fn slinky_upper_half_follows_0d_bit_3() {
        let mut card = PdsIIeCard::new();

        // Lower half (bit 3 clear): main + aux.
        hw16(&mut card, 0x1C, 0x0400);
        card.host_write(0x10, 2, 0x55AA).unwrap();
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0x0400], 0x55);
            assert_eq!(s.ram[AUX_RAM + 0x0400], 0xAA);
            assert_eq!(s.ram[FW_MAIN + 0x0400], 0x00);
        }

        // Upper half (bit 3 set): image 0 (upper-main) and upper-aux.
        card.host_write(0x0D, 1, 0x08).unwrap();
        hw16(&mut card, 0x1C, 0x0400);
        card.host_write(0x10, 2, 0x5A5A).unwrap();
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[FW_MAIN + 0x0400], 0x5A);
            assert_eq!(s.ram[UPPER_AUX + 0x0400], 0x5A);
            assert_eq!(s.ram[MAIN_RAM + 0x0400], 0x55);
        }
    }

    #[test]
    fn display_write_traps_and_updates_ram() {
        let mut card = PdsIIeCard::new();
        // LDA #$41 / STA $0400 / JMP $1006
        load_program(
            &mut card,
            0x1000,
            &[0xA9, 0x41, 0x8D, 0x00, 0x04, 0x4C, 0x06, 0x10],
        );
        run_program(&mut card, 0x1000);
        // $21 = 0x20: text page 1 stores trap (everything else is
        // gated off until the host programs the mask).
        card.host_write(0x21, 1, 0x20).unwrap();

        run(&mut card, 8);
        assert!(card.suspended());
        assert_eq!(card.trap_code(), 0x02); // display store (text p1, main)
        assert_eq!(card.trap_addr(), 0x0400);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x41); // trap data
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0x0400], 0x41); // RAM updated
        }

        // Ack through $0C: the CPU halts before the JMP.
        card.host_write(0x0C, 1, 0x00).unwrap();
        assert!(!card.suspended());
        card.tick(16, ()).unwrap();
        assert_eq!(card.cpu.regs.pc, 0x1006);
    }

    #[test]
    fn video_switch_byte_packs_in_the_drivers_bit_order() {
        let mut card = PdsIIeCard::new();
        // Reset default: every display switch off, so $08 reads $00
        // until the 65C02 sets the real mode (the app's displayed-mode
        // copy starts at 0 and only recomputes after a switch change;
        // TEXT on at reset would strand it in lo-res).
        assert_eq!(card.host_read(0x08, 1).unwrap(), 0x00);
        {
            let mut s = card.cpu.bus.state.borrow_mut();
            s.sw.altchar = true; // bit 7
            s.sw.text = true; // bit 6
            s.sw.hires = false; // bit 5 clear
            s.sw.an[3] = true; // bit 4 (AN3 on = DHIRES off)
            s.sw.col80 = true; // bit 3
            s.sw.mixed = true; // bit 2
            s.sw.page2 = true; // bit 1
            s.sw.store80 = true; // bit 0
        }
        assert_eq!(card.host_read(0x08, 1).unwrap(), 0xDF);
        {
            let mut s = card.cpu.bus.state.borrow_mut();
            s.sw.hires = true; // bit 5
        }
        assert_eq!(card.host_read(0x08, 1).unwrap(), 0xFF);
    }

    /// "Restart IIe" reads $08 between stopping and starting the 65C02:
    /// the switches of the previous run must be gone by then.
    #[test]
    fn stopping_the_65c02_resets_the_switches() {
        let mut card = PdsIIeCard::new();
        card.host_write(0x20, 1, 0x88).unwrap();
        card.cpu.bus.state.borrow_mut().sw.text = true;
        assert_eq!(card.host_read(0x08, 1).unwrap(), 0x40);
        card.host_write(0x20, 1, 0x08).unwrap();
        assert_eq!(card.host_read(0x08, 1).unwrap(), 0x00);
    }

    #[test]
    fn display_traps_gate_on_21() {
        let mut card = PdsIIeCard::new();
        // LDA #$41 / STA $2000 (hi-res p1) / STA $0400 (text p1) /
        // JMP $1008
        load_program(
            &mut card,
            0x1000,
            &[
                0xA9, 0x41, 0x8D, 0x00, 0x20, 0x8D, 0x00, 0x04, 0x4C, 0x08, 0x10,
            ],
        );
        run_program(&mut card, 0x1000);

        // $21 = 0: nothing traps; both stores land in RAM.
        run(&mut card, 16);
        assert!(!card.suspended());
        assert_eq!(card.cpu.regs.pc, 0x1008);
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0x2000], 0x41);
            assert_eq!(s.ram[MAIN_RAM + 0x0400], 0x41);
        }

        // $21 = 0x80 (hi-res p1): the $2000 store traps ($04); the
        // $0400 text store (bit 5 clear) still does not.
        card.cpu.reset_pc(0x1000);
        card.host_write(0x21, 1, 0x80).unwrap();
        run(&mut card, 16);
        assert!(card.suspended());
        assert_eq!(card.trap_code(), 0x04);
        assert_eq!(card.trap_addr(), 0x2000);
        card.host_write(0x0C, 1, 0x00).unwrap();
        run(&mut card, 16);
        assert!(!card.suspended());
        assert_eq!(card.cpu.regs.pc, 0x1008);

        // Aux landing: with RAMWRT aux the $0400 store lands in aux,
        // so the gate bit is 3 (aux text), not 5 — the model must key
        // on the bank the store actually lands in.
        load_program(
            &mut card,
            0x1000,
            &[
                0xA9, 0x41, 0x8D, 0x05, 0xC0, 0x8D, 0x00, 0x04, 0x4C, 0x08, 0x10,
            ],
        );
        run_program(&mut card, 0x1000);
        card.host_write(0x21, 1, 0x08).unwrap();
        run(&mut card, 16);
        assert!(card.suspended());
        assert_eq!(card.trap_code(), 0x0A); // text + aux
        assert_eq!(card.trap_addr(), 0x0400);
    }

    #[test]
    fn store80_page2_selects_the_display_page_banks() {
        let mut card = PdsIIeCard::new();

        // One pass through the 80STORE/PAGE2/HIRES combinations the
        // spec §4.4 rule covers (each switch write traps $20):
        //   LDA #$41
        //   STA $C001  80STORE on
        //   STA $C055  PAGE2 on
        //   STA $C057  HIRES on
        //   STA $0400  → aux   (0x0A)
        //   STA $0800  → main  (0x02)  [$0800 does NOT move to aux]
        //   STA $2000  → aux   (0x0C)  [PAGE2 + HIRES]
        //   STA $4000  → main  (0x04)  [$4000 does NOT move to aux]
        //   STA $C054  PAGE2 off
        //   STA $0400  → main  (0x02)
        //   STA $0800  → main  (0x02)
        //   STA $2000  → main  (0x04)
        //   STA $4000  → main  (0x04)
        //   STA $C056  HIRES off
        //   STA $C055  PAGE2 on
        //   STA $2000  → main  (0x04)  [no HIRES: PAGE2 no longer moves it]
        //   STA $C000  80STORE off
        //   STA $0400  → main  (0x02)  [no 80STORE: PAGE2 has no effect]
        //   JMP $1035
        let mut prog = vec![0xA9, 0x41];
        for ins in [
            &[0x8D, 0x01, 0xC0][..],
            &[0x8D, 0x55, 0xC0][..],
            &[0x8D, 0x57, 0xC0][..],
            &[0x8D, 0x00, 0x04][..],
            &[0x8D, 0x00, 0x08][..],
            &[0x8D, 0x00, 0x20][..],
            &[0x8D, 0x00, 0x40][..],
            &[0x8D, 0x54, 0xC0][..],
            &[0x8D, 0x00, 0x04][..],
            &[0x8D, 0x00, 0x08][..],
            &[0x8D, 0x00, 0x20][..],
            &[0x8D, 0x00, 0x40][..],
            &[0x8D, 0x56, 0xC0][..],
            &[0x8D, 0x55, 0xC0][..],
            &[0x8D, 0x00, 0x20][..],
            &[0x8D, 0x00, 0xC0][..],
            &[0x8D, 0x00, 0x04][..],
        ] {
            prog.extend_from_slice(ins);
        }
        prog.extend_from_slice(&[0x4C, 0x35, 0x10]); // JMP $1035 (self-loop)
        assert_eq!(prog.len(), 56);
        load_program(&mut card, 0x1000, &prog);
        run_program(&mut card, 0x1000);
        // $21 = $FC: every display area traps (text p1/p2, hires
        // p1/p2, aux text, aux hires) — after the boot word write.
        card.host_write(0x21, 1, 0xFC).unwrap();

        for code in [
            0x20, 0x20, 0x20, // 80STORE, PAGE2, HIRES on
            0x0A, 0x22, 0x0C, 0x24, // 0400 aux / 0800 main / 2000 aux / 4000 main
            0x20, // PAGE2 off
            0x02, 0x22, 0x04, 0x24, // all main
            0x20, 0x20, // HIRES off, PAGE2 on
            0x04, // 2000 main again
            0x20, // 80STORE off
            0x02, // 0400 main
        ] {
            wait_trap(&mut card);
            assert_eq!(card.trap_code(), code);
            card.host_write(0x0C, 1, 0x00).unwrap();
        }

        // The RAM itself: the PAGE2-on stores landed in aux, the rest
        // in main.
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[AUX_RAM + 0x0400], 0x41);
            assert_eq!(s.ram[MAIN_RAM + 0x0400], 0x41);
            assert_eq!(s.ram[AUX_RAM + 0x2000], 0x41);
            assert_eq!(s.ram[MAIN_RAM + 0x2000], 0x41);
            assert_eq!(s.ram[MAIN_RAM + 0x0800], 0x41);
            assert_eq!(s.ram[AUX_RAM + 0x0800], 0x00);
            assert_eq!(s.ram[MAIN_RAM + 0x4000], 0x41);
            assert_eq!(s.ram[AUX_RAM + 0x4000], 0x00);
        }
    }

    #[test]
    fn display_switch_reads_flip_the_switch() {
        let mut card = PdsIIeCard::new();
        // The card firmware's clear/scroll routine flips PAGE2 with
        // `BIT $C055` / `BIT $C054` (main ROM $CCD5-$CCE2): the reads
        // must select the bank for the following store.
        //   STA $C001  (80STORE on)          → trap $20
        //   BIT $C055  (PAGE2 on)            → trap $21 @ C055
        //   LDA #$41 / STA $0400             → aux → trap $0A
        //   BIT $C054  (PAGE2 off)           → trap $21 @ C054
        //   LDA #$42 / STA $0400             → main → trap $02
        //   JMP $1015
        load_program(
            &mut card,
            0x1000,
            &[
                0xA9, 0x00, 0x8D, 0x01, 0xC0, // STA $C001
                0x2C, 0x55, 0xC0, // BIT $C055
                0xA9, 0x41, 0x8D, 0x00, 0x04, // STA $0400
                0x2C, 0x54, 0xC0, // BIT $C054
                0xA9, 0x42, 0x8D, 0x00, 0x04, // STA $0400
                0x4C, 0x15, 0x10,
            ],
        );
        run_program(&mut card, 0x1000);
        // $21 = $3C: text p1 (main + aux) stores trap.
        card.host_write(0x21, 1, 0x3C).unwrap();

        wait_trap(&mut card);
        assert_eq!(card.trap_code(), 0x20); // STA $C001
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!((card.trap_code(), card.trap_addr()), (0x21, 0xC055));
        card.host_write(0x19, 1, 0x00).unwrap(); // answer the read
        wait_trap(&mut card);
        assert_eq!((card.trap_code(), card.trap_addr()), (0x0A, 0x0400)); // aux
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!((card.trap_code(), card.trap_addr()), (0x21, 0xC054));
        card.host_write(0x19, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!((card.trap_code(), card.trap_addr()), (0x02, 0x0400)); // main
        card.host_write(0x0C, 1, 0x00).unwrap();

        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[AUX_RAM + 0x0400], 0x41);
            assert_eq!(s.ram[MAIN_RAM + 0x0400], 0x42);
        }
    }

    #[test]
    fn ctrl2_bit3_holds_and_releases_the_cpu() {
        let mut card = PdsIIeCard::new();
        // NOP / NOP / JMP $1000 (a 3-instruction loop)
        load_program(&mut card, 0x1000, &[0xEA, 0xEA, 0x4C, 0x00, 0x10]);
        run_program(&mut card, 0x1000);
        run(&mut card, 2);

        // $20 bit 3 clear while running: held at the next
        // instruction, with no trap reported ($01 reads 0).
        card.host_write(0x20, 1, 0x80).unwrap();
        run(&mut card, 4);
        assert_eq!(card.trap_code(), 0);
        let held_pc = card.cpu.regs.pc;
        run(&mut card, 4);
        assert_eq!(card.cpu.regs.pc, held_pc); // still held

        // Bit 3 set again: released, no ack.
        card.host_write(0x20, 1, 0x88).unwrap();
        assert!(!card.suspended());
        run(&mut card, 1);
        assert_ne!(card.cpu.regs.pc, held_pc); // advancing again
    }

    /// The game buttons trap while `$21` bit 1 is set: "Restart IIe"
    /// answers `$C061` with `$FF` (Open-Apple down) so that RESET
    /// cold-starts.  With the bit clear they read the card's own state.
    #[test]
    fn button_reads_trap_with_ctrl3_bit_1() {
        // LDA $C061 / JMP $1003
        let program = [0xAD, 0x61, 0xC0, 0x4C, 0x03, 0x10];

        let mut card = PdsIIeCard::new();
        load_program(&mut card, 0x1000, &program);
        run_program(&mut card, 0x1000);
        card.cpu.bus.state.borrow_mut().host.ctrl3 = 0x02;
        run(&mut card, 8);
        assert!(card.suspended());
        assert_eq!(card.trap_code(), 0x21);
        assert_eq!(card.trap_addr(), 0xC061);
        card.host_write(0x19, 1, 0xFF).unwrap();
        for _ in 0..4 {
            card.tick(16, ()).unwrap();
        }
        assert_eq!(card.cpu.regs.c as u8, 0xFF);
        assert_eq!(card.cpu.regs.pc, 0x1003);

        let mut card = PdsIIeCard::new();
        load_program(&mut card, 0x1000, &program);
        run_program(&mut card, 0x1000);
        run(&mut card, 8);
        assert!(!card.suspended());
        assert_eq!(card.cpu.regs.c as u8, 0x00);
    }

    /// The 65C02 runs at its clock rate however finely the host time is
    /// given to it.
    #[test]
    fn cpu_cycles_follow_host_time() {
        // NOP / NOP / NOP / JMP $1000
        let program = [0xEA, 0xEA, 0xEA, 0x4C, 0x00, 0x10];
        for (fast, hz) in [(false, NORMAL_HZ), (true, FAST_HZ)] {
            let mut card = PdsIIeCard::new();
            load_program(&mut card, 0x1000, &program);
            run_program(&mut card, 0x1000);
            if fast {
                card.cpu.bus.state.borrow_mut().sw.c02b |= 0x04;
            }
            let start = card.cpu.cycles;
            // A tenth of a second
            for _ in 0..C16M_FREQUENCY / 10 {
                card.tick(1, ()).unwrap();
            }
            let cycles = (card.cpu.cycles - start) as i64;
            assert!((cycles - hz / 10).abs() <= 3, "{cycles} cycles at {hz} Hz");
        }
    }

    #[test]
    fn trapped_read_retries_with_data_out() {
        let mut card = PdsIIeCard::new();
        // LDA $C05C (trapped option-panel speed read) / JMP $1003
        load_program(&mut card, 0x1000, &[0xAD, 0x5C, 0xC0, 0x4C, 0x03, 0x10]);
        run_program(&mut card, 0x1000);

        run(&mut card, 8);
        assert!(card.suspended());
        assert_eq!(card.trap_code(), 0x21); // switch read
        assert_eq!(card.trap_addr(), 0xC05C);

        // Host: answer with the option-panel speed (bit 2 = fast).  The
        // $19 write itself completes the trapped read.
        card.host_write(0x19, 1, 0x04).unwrap();
        assert!(!card.suspended());
        for _ in 0..4 {
            card.tick(16, ()).unwrap();
        }
        // The LDA re-executed and consumed the data-out value.
        assert_eq!(card.cpu.regs.c as u8, 0x04);
        assert_eq!(card.cpu.regs.pc, 0x1003);
        // The retry latch is consumed.
        assert!(card.cpu.bus.state.borrow().retry.is_none());
    }

    /// A trapped read inside a read-modify-write instruction: the
    /// answer must also cover the instruction's *second* operand read
    /// (the slot 7 memory-card probe does `DEC $BFF8,X` on a trapped
    /// address).  The retry latch is cleared at
    /// the instruction boundary, not on first use.
    #[test]
    fn trapped_read_covers_the_whole_rmw_instruction() {
        let mut card = PdsIIeCard::new();
        // INC $C078 (mailbox read + write, both trapped) / JMP $1003
        load_program(&mut card, 0x1000, &[0xEE, 0x78, 0xC0, 0x4C, 0x03, 0x10]);
        run_program(&mut card, 0x1000);

        // The operand read traps; the host answers $41.
        run(&mut card, 12);
        assert!(card.suspended());
        assert_eq!(card.trap_addr(), 0xC078);
        card.host_write(0x19, 1, 0x41).unwrap();
        assert!(!card.suspended());

        // The re-run: the first read takes the answer, the 65C02
        // re-read takes it again, and the writeback traps with the
        // incremented value — no second read trap.
        run(&mut card, 12);
        assert!(card.suspended());
        assert_eq!(card.trap_addr(), 0xC078);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x42);
        card.host_write(0x0C, 1, 0x00).unwrap(); // ack the write

        // The instruction completed: the retry latch is cleared and
        // the program advanced.
        run(&mut card, 8);
        assert_eq!(card.cpu.regs.pc, 0x1003);
        assert!(card.cpu.bus.state.borrow().retry.is_none());
    }

    #[test]
    fn mailbox_trap_carries_data_both_ways() {
        let mut card = PdsIIeCard::new();
        // LDA #$5A / STA $C07D (mailbox write) / LDA $C07D / JMP $1008
        load_program(
            &mut card,
            0x1000,
            &[
                0xA9, 0x5A, 0x8D, 0x7D, 0xC0, // LDA #$5A / STA $C07D
                0xAD, 0x7D, 0xC0, // LDA $C07D
                0x4C, 0x08, 0x10, // JMP $1008 (self-loop)
            ],
        );
        run_program(&mut card, 0x1000);

        // The STA $C07D traps: the written byte is the trap data.
        run(&mut card, 8);
        assert!(card.suspended());
        assert_eq!(card.trap_code(), 0x20);
        assert_eq!(card.trap_addr(), 0xC07D);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x5A);
        card.host_write(0x0C, 1, 0x00).unwrap(); // ack

        // The LDA $C07D traps as a read; the host's $19 answer
        // completes it.
        run(&mut card, 6);
        assert!(card.suspended());
        assert_eq!(card.trap_code(), 0x21);
        card.host_write(0x19, 1, 0x5A).unwrap();
        run(&mut card, 6);
        assert_eq!(card.cpu.regs.c as u8, 0x5A);
        assert_eq!(card.cpu.regs.pc, 0x1008);
    }

    #[test]
    fn firmware_is_write_protected() {
        let mut card = PdsIIeCard::new();
        card.load_firmware(0x3FFC, &[0x62, 0xFA]).unwrap(); // reset vector $FA62

        // After reset the LC window reads the ROM image (empty here).
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[FW_MAIN + 0x1000], 0x00);
            assert_eq!(s.ram[MAIN_RAM + 0xC000], 0x00);
        }

        // LDA #$55 / STA $D000 (no LC write enabled) / JMP $0805.
        load_program(
            &mut card,
            0x0800,
            &[0xA9, 0x55, 0x8D, 0x00, 0xD0, 0x4C, 0x05, 0x08],
        );
        run_program(&mut card, 0x0800);
        run(&mut card, 8);
        {
            let s = card.cpu.bus.state.borrow();
            // The write went nowhere: the LC RAM is untouched.
            assert_eq!(s.ram[MAIN_RAM + 0xC000], 0x00);
        }

        // The IIe prewrite rule: two accesses to the odd ($C088+)
        // switches in a row, then the write switch — STA $D000 lands in
        // the LC RAM (physical main $C000).
        load_program(
            &mut card,
            0x0800,
            &[
                0xA9, 0x0B, 0x8D, 0x8B, 0xC0, // LDA #$0B / STA $C08B
                0x8D, 0x8B, 0xC0, // STA $C08B (prewrite rule)
                0xA9, 0x55, 0x8D, 0x00, 0xD0, // LDA #$55 / STA $D000
                0x4C, 0x0C, 0x08, // JMP $080C
            ],
        );
        card.cpu.reset_pc(0x0800);
        run(&mut card, 20);
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0xC000], 0x55);
            // The ROM image is untouched.
            assert_eq!(s.ram[FW_MAIN + 0x1000], 0x00);
        }
    }

    #[test]
    fn held_in_reset_until_released() {
        let mut card = PdsIIeCard::new();
        card.load_firmware(0x3FFC, &[0x62, 0x7A]).unwrap(); // main, $FFFC

        // Stopped (bit 7 clear): nothing executes.
        let cycles = card.cpu.cycles;
        for _ in 0..10 {
            card.tick(16, ()).unwrap();
        }
        assert_eq!(card.cpu.cycles, cycles);

        // The $20 bit 7 0→1 edge boots the CPU at the reset vector of
        // ROM image 0.
        hw16(&mut card, 0x20, 0x8800);
        assert_eq!(card.cpu.regs.pc, 0x7A62);
        card.tick(16, ()).unwrap();
        assert!(card.cpu.cycles > 0);

        // Clearing bit 7 stops it again.
        hw16(&mut card, 0x20, 0x0000);
        let cycles = card.cpu.cycles;
        for _ in 0..10 {
            card.tick(16, ()).unwrap();
        }
        assert_eq!(card.cpu.cycles, cycles);
    }

    /// Replays IIe Startup's load sequence: the RAM test as
    /// word fills + per-lane byte readbacks in both Slinky halves, the
    /// upper-half ROM images with their vectors, the boot program in
    /// plain main RAM, and the $20 bit 7 edge booting into a trap.
    #[test]
    fn replays_the_boot_sequence_and_traps() {
        let mut card = PdsIIeCard::new();

        // RAM test, upper half first ($0D bit 3 = 1): 64K word writes
        // of $FFFF, then 64K byte reads per lane.
        card.host_write(0x0D, 1, 0x08).unwrap();
        hw16(&mut card, 0x1C, 0x0000);
        for _ in 0..0x10000 {
            card.host_write(0x10, 2, 0xFFFF).unwrap();
        }
        hw16(&mut card, 0x1C, 0x0000);
        for _ in 0..0x10000 {
            assert_eq!(card.host_read(0x10, 1).unwrap(), 0xFF);
        }
        hw16(&mut card, 0x1C, 0x0000);
        for _ in 0..0x10000 {
            assert_eq!(card.host_read(0x11, 1).unwrap(), 0xFF);
        }
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[FW_MAIN], 0xFF);
            assert_eq!(s.ram[UPPER_AUX], 0xFF);
            assert_eq!(s.ram[MAIN_RAM], 0x00); // lower half untouched
        }

        // Lower half ($0D bit 3 = 0), same test with $0000.
        card.host_write(0x0D, 1, 0x00).unwrap();
        hw16(&mut card, 0x1C, 0x0000);
        for _ in 0..0x10000 {
            card.host_write(0x10, 2, 0x0000).unwrap();
        }
        hw16(&mut card, 0x1C, 0x0000);
        for _ in 0..0x10000 {
            assert_eq!(card.host_read(0x10, 1).unwrap(), 0x00);
        }
        hw16(&mut card, 0x1C, 0x0000);
        for _ in 0..0x10000 {
            assert_eq!(card.host_read(0x11, 1).unwrap(), 0x00);
        }

        // The upper-half ROM images: vectors at image 0 $3FFA and image
        // 1 $7FFA (NMI $D000, RESET $2000, IRQ $2003), the NMI handler
        // at image 1 $5000.
        card.host_write(0x0D, 1, 0x08).unwrap();
        for (addr, bytes) in [
            (0x3FFA, &[0x00, 0xD0, 0x00, 0x20, 0x03, 0x20][..]),
            (0x7FFA, &[0x00, 0xD0, 0x00, 0x20, 0x03, 0x20][..]),
            (0x5000, &[0xA9, 0x02, 0x8D, 0x7D, 0xC0, 0x40][..]),
        ] {
            hw16(&mut card, 0x1C, addr);
            for b in bytes {
                card.host_write(0x10, 1, *b as u32).unwrap();
            }
        }

        // The boot program in plain main RAM at $2000: LDA #$01 /
        // STA $0400 (display store, trapped) / JMP $2005.
        card.host_write(0x0D, 1, 0x00).unwrap();
        hw16(&mut card, 0x1C, 0x2000);
        for b in [0xA9, 0x01, 0x8D, 0x00, 0x04, 0x4C, 0x05, 0x20] {
            card.host_write(0x10, 1, b as u32).unwrap();
        }
        // Patch the main ROM's reset vector (CPU $FFFC ← slinky
        // $3FFC; the RESET vector fetch reads the main bank).
        card.host_write(0x0D, 1, 0x08).unwrap();
        hw16(&mut card, 0x1C, 0x3FFC);
        card.host_write(0x10, 1, 0x00).unwrap();
        card.host_write(0x10, 1, 0x20).unwrap();
        card.host_write(0x0D, 1, 0x00).unwrap();

        // The $20 bit 7 edge (bit 3 set) boots: reset from the main
        // image's $FFFC → $2000; $21 = 0x20 so the text store traps.
        hw16(&mut card, 0x20, 0x8800);
        card.host_write(0x21, 1, 0x20).unwrap();
        assert_eq!(card.cpu.regs.pc, 0x2000);
        run(&mut card, 12);
        assert!(card.suspended());
        assert_eq!(card.trap_code(), 0x02); // display store (text p1, main)
        assert_eq!(card.trap_addr(), 0x0400);
        // Ack through $0C: the CPU resumes past the trapped write.
        card.host_write(0x0C, 1, 0x00).unwrap();
        assert!(!card.suspended());
        card.tick(16, ()).unwrap();
        assert_eq!(card.cpu.regs.pc, 0x2005);
    }

    #[test]
    fn alt_firmware_bank_switch() {
        // $C029 switches to the alternate bank: D1 (c02b bit 3 clear)
        // or D2 (set).  The D1/D2 banks hold only the interrupt
        // receivers, INTCX, PFI and the vectors.
        let mut card = PdsIIeCard::new();
        card.load_firmware(0x6000, &[0xAB]).unwrap(); // D1's $E000
        card.load_firmware(0xA000, &[0x77]).unwrap(); // D2's $E000

        // LDA $C029 / LDA $E000 (D1) / LDA #$08 / STA $C02B / LDA
        // $C029 / LDA $E000 (D2) / JMP $1010
        load_program(
            &mut card,
            0x1000,
            &[
                0xAD, 0x29, 0xC0, 0xAD, 0x00, 0xE0, // D1 → 0xAB
                0xA9, 0x08, 0x8D, 0x2B, 0xC0, // select D2
                0xAD, 0x29, 0xC0, 0xAD, 0x00, 0xE0, // D2 → 0x77
                0x4C, 0x11, 0x10,
            ],
        );
        run_program(&mut card, 0x1000);
        run(&mut card, 8);
        assert_eq!(card.cpu.regs.c as u8, 0x77);
        assert_eq!(card.cpu.regs.pc, 0x1011);

        // $C028 switches back to the main ROM.
        let mut card = PdsIIeCard::new();
        card.load_firmware(0x3FFC, &[0xCD, 0xEF]).unwrap(); // main, $FFFC
        load_program(
            &mut card,
            0x1000,
            &[0xAD, 0x28, 0xC0, 0xAD, 0xFC, 0xFF, 0x4C, 0x06, 0x10],
        );
        run_program(&mut card, 0x1000);
        run(&mut card, 8);
        assert_eq!(card.cpu.regs.c as u8, 0xCD);
    }

    #[test]
    fn host_call_raises_nmi() {
        // D1's NMI vector ($FFFA) points at its receiver at $D000:
        // LDA #$55 / JMP $D002.
        let mut card = PdsIIeCard::new();
        card.load_firmware(0x7FFA, &[0x00, 0xD0]).unwrap();
        card.load_firmware(0x5000, &[0xA9, 0x55, 0x4C, 0x02, 0xD0])
            .unwrap();

        // A $02 read as an operand or as data is not a host call:
        // LDA #$02 / LDA $0900 (holding $02) / JMP $0805.
        load_program(
            &mut card,
            0x0800,
            &[0xA9, 0x02, 0xAD, 0x00, 0x09, 0x4C, 0x05, 0x08],
        );
        load_program(&mut card, 0x0900, &[0x02]);
        run_program(&mut card, 0x0800);
        run(&mut card, 8);
        assert_eq!(card.cpu.regs.c as u8, 0x02);
        assert_eq!(card.cpu.regs.pc, 0x0805);

        // `02 42` / JMP $0802: the NMI is taken after the 2-byte NOP,
        // with the return address $0802 on the stack.
        load_program(&mut card, 0x0800, &[0x02, 0x42, 0x4C, 0x02, 0x08]);
        run_program(&mut card, 0x0800);
        run(&mut card, 8);
        assert_eq!(card.cpu.regs.c as u8, 0x55);
        assert_eq!(card.cpu.regs.pc, 0xD002);
        let s = card.cpu.bus.state.borrow();
        assert_eq!(s.ram[MAIN_RAM + 0x01FF], 0x08);
        assert_eq!(s.ram[MAIN_RAM + 0x01FE], 0x02);
        assert!(!s.nmi);
    }

    #[test]
    fn screen_holes_do_not_trap() {
        let mut card = PdsIIeCard::new();
        // LDA #$41 / STA $0478 (screen hole) / JMP $1008 (self-loop)
        load_program(
            &mut card,
            0x1000,
            &[
                0xA9, 0x41, 0x8D, 0x78, 0x04, 0x4C, 0x08, 0x10, 0x4C, 0x08, 0x10,
            ],
        );
        run_program(&mut card, 0x1000);
        for _ in 0..8 {
            card.tick(16, ()).unwrap();
        }
        assert!(!card.suspended());
        assert_eq!(card.cpu.regs.pc, 0x1008);
    }

    /// The synthesized declaration ROM has the format block the LC
    /// ROM's scan verifies and the two sResources the Slot Manager and
    /// the IIe Startup INIT look for.
    #[test]
    fn declaration_rom_layout() {
        // Format block (big-endian in ascending address order; logical
        // 0 = the top byte)
        assert_eq!(DECL_ROM[0], 0x5A); // byte lanes 1+3
        assert_eq!(DECL_ROM[1], 0x00); // reserved
        assert_eq!(&DECL_ROM[2..6], &[0xC7, 0x2B, 0x93, 0x5A]); // test pattern
        assert_eq!(DECL_ROM[6], 0x01); // format 1
        assert_eq!(DECL_ROM[7], 0x01); // revision
        assert_eq!(&DECL_ROM[16..20], &[0x00, 0xFF, 0xFF, 0x00]); // dir offset
        // Directory: [id:8|offset:24] longwords at 275/271/267, ids
        // ascending, 0xFF ending the list (logical 275 = the id byte).
        assert_eq!(&DECL_ROM[272..276], &[0x0C, 0x00, 0x00, 0x01]);
        assert_eq!(&DECL_ROM[268..272], &[0x34, 0x00, 0x00, 0x80]);
        assert_eq!(&DECL_ROM[264..268], &[0x00, 0x00, 0x00, 0xFF]);
        // Board sResource (category 1, type 0) and IIe sResource
        // (category 10, type 21, drvrSW 1) sType data blocks
        assert_eq!(
            &DECL_ROM[228..236],
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00]
        );
        assert_eq!(
            &DECL_ROM[192..200],
            &[0x00, 0x00, 0x01, 0x00, 0x15, 0x00, 0x0A, 0x00]
        );
        // PRAMInitData: length long counts itself (10) + 6 zero bytes
        assert_eq!(
            &DECL_ROM[181..191],
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn motor_switches_trap_with_their_codes() {
        let mut card = PdsIIeCard::new();
        // LDA #$01 / STA $C0E9 (motor on) / LDA $C0E9 / STA $C0E8 /
        // JMP $100B (self-loop)
        load_program(
            &mut card,
            0x1000,
            &[
                0xA9, 0x01, 0x8D, 0xE9, 0xC0, // motor on → $38
                0xAD, 0xE9, 0xC0, // read → $38
                0x8D, 0xE8, 0xC0, // motor off → $28
                0x4C, 0x0B, 0x10, // JMP $100B (self-loop)
            ],
        );
        run_program(&mut card, 0x1000);

        for code in [0x38, 0x38, 0x28] {
            wait_trap(&mut card);
            assert_eq!(card.trap_code(), code);
            card.host_write(0x0C, 1, 0x00).unwrap();
        }
        run(&mut card, 8);
        assert_eq!(card.cpu.regs.pc, 0x100B);
        {
            let s = card.cpu.bus.state.borrow();
            assert!(!s.motor_on);
        }
    }

    /// Runs the 65C02 until it traps (or panics after a while).
    fn wait_trap(card: &mut PdsIIeCard) {
        for _ in 0..100_000 {
            if card.suspended() {
                return;
            }
            card.tick(16, ()).unwrap();
        }
        panic!("the 65C02 never trapped");
    }

    /// The C8 space follows the selected slot (§4.4, MAME
    /// `apple2e.cpp`): any `$Cn00`–`$CnFF` access with INTCXROM off
    /// selects slot n's `$C8xx` image, any `$CFFF` access deselects
    /// (back to the internal ROM, and clearing INTC8ROM), and a
    /// `$C3xx` access with INTC3ROM on sets INTC8ROM (internal C8).
    /// Also: `$C02B` bit 5 enables C8-space writes into the served
    /// image, and `$C02B` bit 4 + ALTZP moves only the zero page.
    #[test]
    fn c8_space_follows_the_selected_slot() {
        let mut card = PdsIIeCard::new();
        // Internal ROM's C8 pages: $C800 = $AA.
        card.load_firmware(0x0800, &[0xAA]).unwrap();
        // Slot 2/5/7 `$C8xx` images: $C800 = $22/$55/$77.
        card.load_firmware(0xC000 + 2 * 0x800, &[0x22]).unwrap();
        card.load_firmware(0xC000 + 5 * 0x800, &[0x55]).unwrap();
        card.load_firmware(0xC000 + 7 * 0x800, &[0x77]).unwrap();

        // Part 1: LDA $C500 (select slot 5) / LDA $C800 / STA $40 /
        // LDA $C200 (select slot 2) / LDA $C800 / STA $41 /
        // LDA #$20 / STA $C02B (bit 5: C8 writes) /
        // LDA #$EE / STA $C900 (lands in slot 2's image) /
        // LDA #$10 / STA $C02B (bit 4) / STA $C009 (ALTZP on) /
        // LDA #$5B / STA $0000 (second zero page) /
        // LDA #$5A / STA $0100 (the stack stays on the ALTZP side) /
        // JMP $1035.
        let prog = [
            0xAD, 0x00, 0xC5, 0xAD, 0x00, 0xC8, 0x85, 0x40, 0xAD, 0x00, 0xC2, 0xAD, 0x00, 0xC8,
            0x85, 0x41, 0xA9, 0x20, 0x8D, 0x2B, 0xC0, 0xA9, 0xEE, 0x8D, 0x00, 0xC9, 0xA9, 0x10,
            0x8D, 0x2B, 0xC0, 0x8D, 0x09, 0xC0, 0xA9, 0x5B, 0x85, 0x00, 0xA9, 0x5A, 0x8D, 0x00,
            0x01, 0x4C, 0x2B, 0x10,
        ];
        load_program(&mut card, 0x1000, &prog);
        run_program(&mut card, 0x1000);
        run(&mut card, 300);
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0x40], 0x55);
            assert_eq!(s.ram[MAIN_RAM + 0x41], 0x22);
            // The C8 write went into slot 2's image (offset $100).
            assert_eq!(s.ram[FW_SLOT_ROMS + 2 * 0x800 + 0x100], 0xEE);
            // The second zero page took $0000; the stack page stays on
            // the normal ALTZP side (the old `addr < 0x0200` rule
            // indexed past the 256 KB RAM here).
            assert_eq!(s.ram[AUX2_ZP], 0x5B);
            assert_eq!(s.ram[AUX_RAM + 0x100], 0x5A);
        }

        // Part 2: clear `$C02B` bit 4 and ALTZP (each part is
        // self-contained), then LDA $CFFF (deselect — the read traps,
        // the host answers 0) / LDA $C800 / STA $42 → the internal ROM
        // ($AA).
        let prog2 = [
            0xA9, 0x00, 0x8D, 0x2B, 0xC0, 0x8D, 0x08, 0xC0, 0xAD, 0xFF, 0xCF, 0xAD, 0x00, 0xC8,
            0x85, 0x42, 0x4C, 0x10, 0x10,
        ];
        load_program(&mut card, 0x1000, &prog2);
        run_program(&mut card, 0x1000);
        wait_trap(&mut card);
        card.host_write(0x19, 1, 0x00).unwrap();
        run(&mut card, 100);
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0x42], 0xAA);
        }

        // Part 3: STA $C00A (INTC3ROM on) / LDA $C300 (sets INTC8ROM —
        // even with INTCXROM on, the IIe rule) / LDA $C800 / STA $43 →
        // internal ROM.
        let prog3 = [
            0xA9, 0x00, 0x8D, 0x2B, 0xC0, 0x8D, 0x08, 0xC0, 0x8D, 0x07, 0xC0, 0x8D, 0x0A, 0xC0,
            0xAD, 0x00, 0xC3, 0xAD, 0x00, 0xC8, 0x85, 0x43, 0x4C, 0x16, 0x10,
        ];
        load_program(&mut card, 0x1000, &prog3);
        run_program(&mut card, 0x1000);
        run(&mut card, 100);
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0x43], 0xAA);
        }

        // Part 4: clear INTCXROM too (part 3 left it on), then
        // LDA $CFFF (clear INTC8ROM — traps again) / LDA $C700
        // (select slot 7) / LDA $C800 / STA $44 → slot 7.
        let prog4 = [
            0xA9, 0x00, 0x8D, 0x2B, 0xC0, 0x8D, 0x08, 0xC0, 0x8D, 0x06, 0xC0, 0xAD, 0xFF, 0xCF,
            0xAD, 0x00, 0xC7, 0xAD, 0x00, 0xC8, 0x85, 0x44, 0x4C, 0x16, 0x10,
        ];
        load_program(&mut card, 0x1000, &prog4);
        run_program(&mut card, 0x1000);
        wait_trap(&mut card);
        card.host_write(0x19, 1, 0x00).unwrap();
        run(&mut card, 100);
        {
            let s = card.cpu.bus.state.borrow();
            assert_eq!(s.ram[MAIN_RAM + 0x44], 0x77);
        }
    }

    /// The ROM's internal-firmware thunk at `$FBB4` (`BIT $C015 /
    /// PHP / STA $C007 / JMP $C100`): with SLOTCXROM selected (the
    /// reset default) a direct `JSR $C100` runs the slot-1 ROM, and
    /// the thunk's `STA $C007` then makes the same address run the
    /// internal ROM.  The self-test can't catch the polarity — only
    /// the thunk can.
    #[test]
    fn fbb4_thunk_selects_slot_then_internal_rom() {
        let mut card = PdsIIeCard::new();
        // The thunk at CPU $FBB4 (main fw $3BB4).
        card.load_firmware(
            0x3BB4,
            &[0x2C, 0x15, 0xC0, 0x48, 0x8D, 0x07, 0xC0, 0x4C, 0x00, 0xC1],
        )
        .unwrap();
        // The internal ROM's $C100 dispatcher: PLA (the thunk's
        // PHP'd flags) / LDA #$55 / RTS.
        card.load_firmware(0x0100, &[0x68, 0xA9, 0x55, 0x60])
            .unwrap();
        // The slot-1 ROM: LDA #$33 / RTS.
        card.load_firmware(0xC100, &[0xA9, 0x33, 0x60]).unwrap();

        // STA $C006 (slot ROMs — the reset default) / JSR $C100 /
        // STA $C07D / JSR $FBB4 (thunk → internal) / STA $C07D /
        // LDA $C015 / STA $C07D / JMP $2017.
        load_program(
            &mut card,
            0x2000,
            &[
                0xA9, 0x00, 0x8D, 0x06, 0xC0, // slot ROMs
                0x20, 0x00, 0xC1, // JSR $C100 → slot ROM → A = $33
                0x8D, 0x7D, 0xC0, // mailbox $33
                0x20, 0xB4, 0xFB, // thunk → internal → A = $55
                0x8D, 0x7D, 0xC0, // mailbox $55
                0xAD, 0x15, 0xC0, // $C015: bit 7 = internal now
                0x8D, 0x7D, 0xC0, // mailbox $80
                0x4C, 0x17, 0x20,
            ],
        );
        run_program(&mut card, 0x2000);

        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x33); // slot ROM ran
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x55); // internal ran
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x80); // $C015 bit 7
        card.host_write(0x0C, 1, 0x00).unwrap();
    }

    /// The `$02` call's receiver reads its `nn` operand from the call
    /// site, which lives in the main ROM — so the interrupt-time
    /// alternate-bank routing must NOT cover the call site.  A minimal
    /// receiver reproduces the D1 receiver's stack arithmetic: the
    /// operand ends up in the `$C07D` trap's data.
    #[test]
    fn service_call_operand_comes_from_the_main_bank() {
        let mut card = PdsIIeCard::new();

        // Vectors in D1 (NMI $D000) and main (RESET unused).
        card.host_write(0x0D, 1, 0x08).unwrap();
        hw16(&mut card, 0x1C, 0x7FFA);
        for b in [0x00, 0xD0, 0x00, 0x00, 0x00, 0x00] {
            card.host_write(0x10, 1, b as u32).unwrap();
        }
        // The minimal receiver at D1 $D000: TSX / INX / INX /
        // LDA $0100,X / SEC / SBC #1 / STA $00 / LDA $0101,X /
        // SBC #0 / STA $01 / LDA ($00) / STA $C07D / RTI.
        hw16(&mut card, 0x1C, 0x5000);
        for b in [
            0xBA, 0xE8, 0xE8, 0xBD, 0x00, 0x01, 0x38, 0xE9, 0x01, 0x85, 0x00, 0xBD, 0x01, 0x01,
            0xE9, 0x00, 0x85, 0x01, 0xB2, 0x00, 0x8D, 0x7D, 0xC0, 0x40,
        ] {
            card.host_write(0x10, 1, b as u32).unwrap();
        }
        // The call site in the main ROM at CPU $F000 (fw $3000; the
        // real call sites are at $FAB4/$FAC0 — `$C100` would be
        // slot-ROM territory since SLOTCXROM is the reset default):
        // `02 7F` (the $02 call with nn = $7F) then RTS.
        hw16(&mut card, 0x1C, 0x3000);
        for b in [0x02, 0x7F, 0x60] {
            card.host_write(0x10, 1, b as u32).unwrap();
        }
        card.host_write(0x0D, 1, 0x00).unwrap();

        // Boot with bit 3 set and jump straight to the call site.
        hw16(&mut card, 0x20, 0x8800);
        card.cpu.reset_pc(0xF000);
        wait_trap(&mut card);
        // The operand must be $7F from the main bank (D1's $F001 is
        // empty, so a wrong routing would give 0).
        assert_eq!(card.trap_code(), 0x20);
        assert_eq!(card.trap_addr(), 0xC07D);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x7F);
    }

    /// The IIe Startup self-test (`Conf` 1000/1001), replayed in Rust.
    ///
    /// Apple's own acceptance test of the Gemini runs a 65C02 shim step
    /// by step and checks the exact trap code, address or data for each
    /// access.  The shim itself is Apple's code, so it is deliberately
    /// **not** shipped or loaded here; instead this test reproduces its
    /// *behaviour* with hand-assembled micro-programs that make the
    /// same accesses, and the host side checks the same trap code,
    /// address and data per step.
    #[test]
    fn replays_the_iie_selftest() {
        let mut card = PdsIIeCard::new();

        // Step 1: $0D=0, $20=$0802, read $04, ack; RAM-test both
        // Slinky halves (the upper half first, then the lower).
        card.host_write(0x0D, 1, 0x00).unwrap();
        hw16(&mut card, 0x20, 0x0802);
        card.host_read(0x04, 2).unwrap();
        card.host_write(0x0C, 1, 0x00).unwrap();
        for ctrl in [0x08, 0x00] {
            card.host_write(0x0D, 1, ctrl).unwrap();
            hw16(&mut card, 0x1C, 0x0000);
            for _ in 0..0x8000 {
                card.host_write(0x10, 2, 0xFFFF).unwrap();
            }
            hw16(&mut card, 0x1C, 0x0000);
            for _ in 0..0x8000 {
                assert_eq!(card.host_read(0x10, 1).unwrap(), 0xFF);
            }
            hw16(&mut card, 0x1C, 0x0000);
            for _ in 0..0x8000 {
                assert_eq!(card.host_read(0x11, 1).unwrap(), 0xFF);
            }
            hw16(&mut card, 0x1C, 0x0000);
            for _ in 0..0x8000 {
                card.host_write(0x10, 2, 0x0000).unwrap();
            }
            hw16(&mut card, 0x1C, 0x0000);
            for _ in 0..0x8000 {
                assert_eq!(card.host_read(0x10, 1).unwrap(), 0x00);
            }
            hw16(&mut card, 0x1C, 0x0000);
            for _ in 0..0x8000 {
                assert_eq!(card.host_read(0x11, 1).unwrap(), 0x00);
            }
        }

        // Step 2: the vectors (NMI $D000, RESET $2000, IRQ $2003) in
        // BOTH the main ROM (slinky $3FFA — the RESET fetch reads the
        // main bank) and D1 (slinky $7FFA — the NMI/IRQ fetches read
        // the alternate bank), and the NMI handler at D1 $5000.
        card.host_write(0x0D, 1, 0x08).unwrap();
        for addr in [0x3FFA, 0x7FFA] {
            hw16(&mut card, 0x1C, addr);
            for b in [0x00, 0xD0, 0x00, 0x20, 0x03, 0x20] {
                card.host_write(0x10, 1, b as u32).unwrap();
            }
        }
        hw16(&mut card, 0x1C, 0x5000);
        for b in [0xA9, 0x02, 0x8D, 0x7D, 0xC0, 0x40] {
            card.host_write(0x10, 1, b as u32).unwrap();
        }
        card.host_write(0x0D, 1, 0x00).unwrap();

        // Step 3: $21=$FC, $20=$80 — the reset vector fetch traps with
        // the address $FFFC; $20=$88 + ack boots the CPU at $2000.
        card.host_write(0x21, 1, 0xFC).unwrap();
        card.host_write(0x20, 1, 0x80).unwrap();
        assert!(card.suspended());
        assert_eq!(card.trap_addr(), 0xFFFC);
        card.host_write(0x20, 1, 0x88).unwrap();
        card.host_write(0x0C, 1, 0x00).unwrap();
        assert!(!card.suspended());
        assert_eq!(card.cpu.regs.pc, 0x2000);

        // Step 4: with $0D=3 the mailbox traps carry the code bits 7:6 —
        // LDA $C07D traps $E1 (answered with $19) and STA $C07D traps
        // $E0 with the answer as its data.
        card.host_write(0x0D, 1, 0x03).unwrap();
        load_program(
            &mut card,
            0x2000,
            &[0xAD, 0x7D, 0xC0, 0x8D, 0x7D, 0xC0, 0x4C, 0x06, 0x20],
        );
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card);
        assert_eq!(card.trap_code(), 0xE1);
        assert_eq!(card.trap_addr(), 0xC07D);
        card.host_write(0x19, 1, 0x5A).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.trap_code(), 0xE0);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x5A);
        card.host_write(0x0C, 1, 0x00).unwrap();

        // Step 5: the eight display stores — aux first (codes $0A/$2A/
        // $0C/$2C), then main ($02/$22/$04/$24); both banks hold $41.
        card.host_write(0x0D, 1, 0x00).unwrap();
        load_program(
            &mut card,
            0x2000,
            &[
                0xA9, 0x41, 0x8D, 0x05, 0xC0, 0x8D, 0x00, 0x04, 0x8D, 0x00, 0x08, 0x8D, 0x00, 0x20,
                0x8D, 0x00, 0x40, 0x8D, 0x04, 0xC0, 0x8D, 0x00, 0x04, 0x8D, 0x00, 0x08, 0x8D, 0x00,
                0x20, 0x8D, 0x00, 0x40, 0x4C, 0x20, 0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        for code in [0x0A, 0x2A, 0x0C, 0x2C, 0x02, 0x22, 0x04, 0x24] {
            wait_trap(&mut card);
            assert_eq!(card.trap_code(), code);
            card.host_write(0x0C, 1, 0x00).unwrap();
        }
        // One word cycle moves both lanes at the same address.
        hw16(&mut card, 0x1C, 0x0400);
        assert_eq!(card.host_read(0x10, 2).unwrap(), 0x4141);

        // Step 6: motor on — STA/LDA $C0E9 trap $38/$38; the read's
        // answer comes back through $19 (here via the mailbox).
        load_program(
            &mut card,
            0x2000,
            &[
                0x8D, 0xE9, 0xC0, 0xAD, 0xE9, 0xC0, 0x8D, 0x7D, 0xC0, 0x4C, 0x09, 0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card);
        assert_eq!(card.trap_code(), 0x38);
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.trap_code(), 0x38);
        card.host_write(0x19, 1, 0x42).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x42);
        card.host_write(0x0C, 1, 0x00).unwrap();

        // Step 7: $20 bit 4 set (the driver's motor bit, a byte write
        // to $20 — $21 stays $FC): the same eight display stores now
        // trap with the odd codes.  (Step 6's motor program
        // overwrote $2000, so reload the display program.)
        load_program(
            &mut card,
            0x2000,
            &[
                0xA9, 0x41, 0x8D, 0x05, 0xC0, 0x8D, 0x00, 0x04, 0x8D, 0x00, 0x08, 0x8D, 0x00, 0x20,
                0x8D, 0x00, 0x40, 0x8D, 0x04, 0xC0, 0x8D, 0x00, 0x04, 0x8D, 0x00, 0x08, 0x8D, 0x00,
                0x20, 0x8D, 0x00, 0x40, 0x4C, 0x20, 0x20,
            ],
        );
        card.host_write(0x20, 1, 0x98).unwrap();
        card.cpu.reset_pc(0x2000);
        // Like the self-test: each code is released by the read of its
        // address, and the ack does not release it.
        for code in [0x0B, 0x2B, 0x0D, 0x2D, 0x03, 0x23, 0x05, 0x25] {
            card.host_write(0x0C, 1, 0x00).unwrap();
            wait_trap(&mut card);
            assert_eq!(card.trap_code(), code);
            card.host_write(0x0C, 1, 0x00).unwrap();
            assert_eq!(card.trap_code(), code, "the ack must not release it");
            card.host_read(0x04, 2).unwrap();
            assert_eq!(card.trap_code(), 0);
        }

        // Step 8: motor off — STA/LDA $C0E8 trap $28/$28, then the host
        // clears $20 bit 4 (byte write again).
        card.host_write(0x20, 1, 0x88).unwrap();
        load_program(
            &mut card,
            0x2000,
            &[
                0x8D, 0xE8, 0xC0, 0xAD, 0xE8, 0xC0, 0x8D, 0x7D, 0xC0, 0x4C, 0x09, 0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card);
        assert_eq!(card.trap_code(), 0x28);
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.trap_code(), 0x28);
        card.host_write(0x19, 1, 0x24).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x24);
        card.host_write(0x0C, 1, 0x00).unwrap();

        // Step 9: the self-test clears $21 (= 0) before the slot test,
        // then slot I/O — STA/LDA $C0A0 trap $08/$09 with the
        // compacted address $0020; the read's answer via $19.
        card.host_write(0x21, 1, 0x00).unwrap();
        load_program(
            &mut card,
            0x2000,
            &[
                0x8D, 0xA0, 0xC0, 0xAD, 0xA0, 0xC0, 0x8D, 0x7D, 0xC0, 0x4C, 0x09, 0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card);
        assert_eq!(card.trap_code(), 0x08);
        assert_eq!(card.trap_addr(), 0x0020);
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.trap_code(), 0x09);
        card.host_write(0x19, 1, 0x77).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x77);
        card.host_write(0x0C, 1, 0x00).unwrap();

        // Step 10: keyboard — $18 sets the key and the strobe ($09 bit
        // 7); $C010 returns AKD ($0D bit 7) and clears the strobe.
        load_program(
            &mut card,
            0x2000,
            &[
                0xAD, 0x00, 0xC0, 0x8D, 0x7D, 0xC0, 0xAD, 0x10, 0xC0, 0x8D, 0x7D, 0xC0, 0xAD, 0x00,
                0xC0, 0x8D, 0x7D, 0xC0, 0xAD, 0x10, 0xC0, 0x8D, 0x7D, 0xC0, 0x4C, 0x18, 0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        card.host_write(0x18, 1, 0xA0).unwrap();
        assert_eq!(card.host_read(0x09, 1).unwrap() & 0x80, 0x80);
        card.host_write(0x0D, 1, 0x80).unwrap();
        wait_trap(&mut card); // LDA $C000 → key | strobe
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0xA0);
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card); // LDA $C010 → AKD, strobe cleared
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x80);
        assert_eq!(card.host_read(0x09, 1).unwrap() & 0x80, 0x00);
        card.host_write(0x0C, 1, 0x00).unwrap();
        card.host_write(0x0D, 1, 0x00).unwrap();
        wait_trap(&mut card); // LDA $C000 → key, strobe gone
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x20);
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card); // LDA $C010 → 0
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x00);
        card.host_write(0x0C, 1, 0x00).unwrap();

        // Step 11: VBL — $C019 reflects $0D bit 6.
        load_program(
            &mut card,
            0x2000,
            &[
                0xAD, 0x19, 0xC0, 0x8D, 0x7D, 0xC0, 0xAD, 0x19, 0xC0, 0x8D, 0x7D, 0xC0, 0x4C, 0x0A,
                0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        card.host_write(0x0D, 1, 0x40).unwrap();
        wait_trap(&mut card);
        // The low bits carry the keyboard latch (still 0x20 from the
        // keyboard step) — only bit 7 is the VBL.
        assert_eq!(card.host_read(0x0C, 1).unwrap() & 0x80, 0x80);
        card.host_write(0x0D, 1, 0x00).unwrap();
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap() & 0x80, 0x00);
        card.host_write(0x0C, 1, 0x00).unwrap();

        // Step 12: the software NMI — the $02 two-byte NOP dispatches
        // the NMI handler at $D000 (its mailbox carries 2) and RTI lands
        // on the following CLI.  Then $20 bit 5 raises the IRQ, whose
        // handler at $2003 also writes the mailbox.
        load_program(
            &mut card,
            0x2000,
            &[
                0x4C, 0x09, 0x20, // $2000: JMP $2009
                0xA9, 0x02, 0x8D, 0x7D, 0xC0, 0x40, // $2003: IRQ handler
                0x02, 0x00, // $2009: the software NMI
                0x58, 0x4C, 0x0C, 0x20, // $200B: CLI; JMP $200C
            ],
        );
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card); // the NMI handler's mailbox
        assert_eq!(card.trap_code(), 0x20);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x02);
        card.host_write(0x0C, 1, 0x00).unwrap();
        card.host_write(0x20, 1, 0x88 | 0x20).unwrap(); // IRQ on
        wait_trap(&mut card); // the IRQ handler's mailbox
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x02);
        card.host_write(0x20, 1, 0x88).unwrap(); // IRQ off
        card.host_write(0x0C, 1, 0x00).unwrap();
        run(&mut card, 2);
        assert_eq!(card.cpu.regs.pc, 0x200C); // back in the wait loop

        // Step 13: $0D bit 2 disables ALTZP (the self-test only proves
        // ALTZP; extending the bit to RAMRD/RAMWRT/80STORE is a guess).
        // The program stores $AA to $00 with ALTZP on and reports $00
        // back; with bit 2 set the store lands in main, otherwise in
        // aux.
        card.host_write(0x0D, 1, 0x00).unwrap();
        hw16(&mut card, 0x1C, 0x0000);
        card.host_write(0x10, 1, 0x11).unwrap(); // main[0]
        hw16(&mut card, 0x1C, 0x0000);
        card.host_write(0x11, 1, 0x22).unwrap(); // aux[0]
        load_program(
            &mut card,
            0x2000,
            &[
                0xA9, 0x01, 0x8D, 0x09, 0xC0, 0xA9, 0xAA, 0x85, 0x00, 0xA9, 0x00, 0x8D, 0x08, 0xC0,
                0xA5, 0x00, 0x8D, 0x7D, 0xC0, 0x4C, 0x13, 0x20,
            ],
        );
        card.host_write(0x0D, 1, 0x04).unwrap();
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0xAA); // main
        card.host_write(0x0C, 1, 0x00).unwrap();
        hw16(&mut card, 0x1C, 0x0000);
        card.host_write(0x10, 1, 0x11).unwrap(); // main[0] again
        card.host_write(0x0D, 1, 0x00).unwrap();
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x11); // main
        card.host_write(0x0C, 1, 0x00).unwrap();
        hw16(&mut card, 0x1C, 0x0000);
        assert_eq!(card.host_read(0x11, 1).unwrap(), 0xAA); // aux[0]

        // Step 14: $0D bit 4 — with the LC write switches off, a store
        // to $E000 lands in the live ROM image and reads back.
        card.host_write(0x0D, 1, 0x00).unwrap();
        load_program(
            &mut card,
            0x2000,
            &[
                0x2C, 0x82, 0xC0, 0xA9, 0x55, 0x8D, 0x00, 0xE0, 0xAD, 0x00, 0xE0, 0x8D, 0x7D, 0xC0,
                0x4C, 0x0E, 0x20,
            ],
        );
        card.host_write(0x0D, 1, 0x10).unwrap();
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x55);
        card.host_write(0x0C, 1, 0x00).unwrap();
        // The main ROM image itself changed: slinky $2000 = image
        // $E000.
        card.host_write(0x0D, 1, 0x08).unwrap();
        hw16(&mut card, 0x1C, 0x2000);
        assert_eq!(card.host_read(0x10, 1).unwrap(), 0x55);
        card.host_write(0x0D, 1, 0x00).unwrap();

        // Step 15: the sixteen switch writes; $08 reads $FF after the
        // eight "set" writes and $00 after the eight "clear"s.  (The
        // bit order is unknown; any consistent packing passes.)
        load_program(
            &mut card,
            0x2000,
            &[
                0xA9, 0x01, 0x8D, 0x0F, 0xC0, 0xA9, 0x01, 0x8D, 0x51, 0xC0, 0xA9, 0x01, 0x8D, 0x57,
                0xC0, 0xA9, 0x01, 0x8D, 0x5F, 0xC0, 0xA9, 0x01, 0x8D, 0x0D, 0xC0, 0xA9, 0x01, 0x8D,
                0x53, 0xC0, 0xA9, 0x01, 0x8D, 0x55, 0xC0, 0xA9, 0x01, 0x8D, 0x01, 0xC0, 0x4C, 0x22,
                0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        for _ in 0..8 {
            wait_trap(&mut card);
            assert_eq!(card.trap_code(), 0x20);
            card.host_write(0x0C, 1, 0x00).unwrap();
        }
        assert_eq!(card.host_read(0x08, 1).unwrap(), 0xFF);
        // The real self-test re-boots the 65C02 between steps, which
        // resets the soft switches: the set program left
        // 80STORE+PAGE2+HIRES on, and with the correct bank rule the
        // CPU would fetch this shim from aux RAM.  $00 stops the card;
        // the $88 run edge with bit 3 set soft-resets the switches and
        // boots at the step-2 vector ($2000).
        card.host_write(0x20, 1, 0x00).unwrap();
        card.host_write(0x20, 1, 0x88).unwrap();
        load_program(
            &mut card,
            0x2000,
            &[
                0xA9, 0x00, 0x8D, 0x0E, 0xC0, 0xA9, 0x00, 0x8D, 0x50, 0xC0, 0xA9, 0x00, 0x8D, 0x56,
                0xC0, 0xA9, 0x00, 0x8D, 0x5E, 0xC0, 0xA9, 0x00, 0x8D, 0x0C, 0xC0, 0xA9, 0x00, 0x8D,
                0x52, 0xC0, 0xA9, 0x00, 0x8D, 0x54, 0xC0, 0xA9, 0x00, 0x8D, 0x00, 0xC0, 0x4C, 0x22,
                0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        for _ in 0..8 {
            wait_trap(&mut card);
            assert_eq!(card.trap_code(), 0x20);
            card.host_write(0x0C, 1, 0x00).unwrap();
        }
        assert_eq!(card.host_read(0x08, 1).unwrap(), 0x00);

        // LC RAM (unverified layout, marked [INF] in the model): two
        // $C08B accesses enable bank-1 writes ($E000 → phys $D000,
        // $D000 → phys $C000); $C083 selects bank 2 ($D000 → $F000).
        load_program(
            &mut card,
            0x2000,
            &[
                0x8D, 0x8B, 0xC0, 0x8D, 0x8B, 0xC0, 0xA9, 0x33, 0x8D, 0x00, 0xE0, 0xA9, 0x44, 0x8D,
                0x00, 0xD0, 0x4C, 0x10, 0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        run(&mut card, 20);
        assert_eq!(card.cpu.regs.pc, 0x2010);
        hw16(&mut card, 0x1C, 0xC000);
        assert_eq!(card.host_read(0x10, 1).unwrap(), 0x44);
        hw16(&mut card, 0x1C, 0xD000);
        assert_eq!(card.host_read(0x10, 1).unwrap(), 0x33);
        load_program(
            &mut card,
            0x2000,
            &[
                0x8D, 0x83, 0xC0, 0xA9, 0x55, 0x8D, 0x00, 0xD0, 0xAD, 0x00, 0xD0, 0x8D, 0x7D, 0xC0,
                0x4C, 0x0E, 0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x55);
        card.host_write(0x0C, 1, 0x00).unwrap();
        hw16(&mut card, 0x1C, 0xF000);
        assert_eq!(card.host_read(0x10, 1).unwrap(), 0x55);

        // A $C028 write with bit 7 set selects a non-RAM ROM view
        // (reads $FF — shim $2143 needs $FFFA to change); a $C028
        // write with bit 7 clear restores the main bank.
        load_program(
            &mut card,
            0x2000,
            &[
                0xA9, 0x80, 0x8D, 0x28, 0xC0, 0xAD, 0xFA, 0xFF, 0x8D, 0x7D, 0xC0, 0xA9, 0x00, 0x8D,
                0x28, 0xC0, 0xAD, 0xFA, 0xFF, 0x8D, 0x7D, 0xC0, 0x4C, 0x16, 0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0xFF);
        card.host_write(0x0C, 1, 0x00).unwrap();
        wait_trap(&mut card);
        assert_eq!(card.host_read(0x0C, 1).unwrap(), 0x00); // NMI lo byte
        card.host_write(0x0C, 1, 0x00).unwrap();

        // $C02B bit 4 with ALTZP selects a second zero page in the
        // upper-aux half.
        load_program(
            &mut card,
            0x2000,
            &[
                0xA9, 0x10, 0x8D, 0x2B, 0xC0, 0xA9, 0x01, 0x8D, 0x09, 0xC0, 0xA9, 0x77, 0x85, 0x00,
                0x4C, 0x0E, 0x20,
            ],
        );
        card.cpu.reset_pc(0x2000);
        run(&mut card, 12);
        assert_eq!(card.cpu.regs.pc, 0x200E);
        card.host_write(0x0D, 1, 0x08).unwrap();
        hw16(&mut card, 0x1C, 0xFF00);
        assert_eq!(card.host_read(0x11, 1).unwrap(), 0x77);
        card.host_write(0x0D, 1, 0x00).unwrap();

        // Step 17: $20 = $0802 (stop).
        hw16(&mut card, 0x20, 0x0802);
    }
}
