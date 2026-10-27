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

pub mod cpu;

use std::cell::RefCell;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::bus::C16M_FREQUENCY;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::tickable::{Tickable, Ticks};

use cpu::cpu::Cpu;
use cpu::{Address, Bus6502, ReadKind, Variant};

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
}

/// Everything the 65C02's bus needs to answer an access: the card RAM,
/// the soft switches, the Mac host registers, the Gemini's interrupt
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
}

impl BusState {
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
            // `$CFFF` writes just deselect the C8 space (see
            // `c8_select`, applied before the trap in `write`).
            _ => {}
        }
    }

    /// A 65C02 bus read.
    fn read(&mut self, addr: u16) -> u8 {
        self.c8_select(addr);
        match addr {
            0x0000..=0xBFFF => self.ram_read(addr),
            // The soft-switch window; the rest of the ROM image
            // ($C100+) reads through rom_read.
            0xC000..=0xC0FF => self.io_read(addr),
            0xC100..=0xFFFF => self.rom_read(addr),
        }
    }

    /// A 65C02 bus write.
    fn write(&mut self, addr: u16, val: u8) {
        self.c8_select(addr);
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
                }
            }
            // Writes to the ROM image area are ignored (read-only to
            // the 65C02; the host loads it through the Slinky).
            _ => {}
        }
    }

    /// Soft reset (a released 65C02 reset or a Mac-side reset).
    fn soft_reset(&mut self) {
        self.sw = SoftSwitches::default();
        self.kbd = 0;
        self.kbd_strobe = false;
        self.paddle = [0; 4];
        self.btn = [false; 3];
        self.irq = false;
        self.nmi = false;
        self.int_bank = false;
    }
}

/// The 65C02's view of the card: the IIe memory model and the soft
/// switches.
///
/// Owned by the [`Cpu`]; the mutable state lives behind a `RefCell`
/// because [`Bus6502::read`] takes `&self` but has side effects (the
/// keyboard strobe, the switches).
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
}

/// The Apple IIe Card for the Macintosh LC family: the 65C02 and its bus,
/// and the C16M clock divider.
#[derive(Serialize, Deserialize)]
pub struct PdsIIeCard {
    /// The 65C02, which owns the card's bus.
    pub(crate) cpu: Cpu<PdsIIeBus>,

    /// C16M ticks received, scaled by the CPU clock in Hz, less the 65C02
    /// cycles run, scaled by C16M in Hz. It goes below zero when an
    /// instruction takes more cycles than the ticks received.
    clock_acc: i64,
}

/// The language-card physical layout (the A/B test switch): false =
/// this model's original guess (bank 1 at `$C000`, bank 2 at `$F000`),
/// true = the MAME / IIgs Mega II layout (spec §14: bank 2 at `$C000`,
/// bank 1 at `$D000`, `$E000` at `$E000`).
const LC_BANK2_AT_C000: bool = false;

impl PdsIIeCard {
    /// Creates a card with its 65C02 stopped.  The host loads the
    /// firmware and sets control register 2 bit 7 to run it.
    pub fn new() -> Self {
        let cpu = Cpu::new(PdsIIeBus::new(), Variant::M65C02);
        Self { cpu, clock_acc: 0 }
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
        }
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
            // One CPU cycle's worth of host time was charged on the
            // loop entry; charge the rest of the instruction's cycles
            // (step() returns the count).
            let cyc = self.cpu.step()?;
            self.clock_acc -= cyc.saturating_sub(1) as i64 * C16M_HZ;
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
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loads a 65C02 program into main RAM.
    fn load_program(card: &mut PdsIIeCard, addr: u16, bytes: &[u8]) {
        let mut s = card.cpu.bus.state.borrow_mut();
        let a = MAIN_RAM + addr as usize;
        s.ram[a..a + bytes.len()].copy_from_slice(bytes);
    }

    /// Starts a card running a program at `addr`.
    fn run_program(card: &mut PdsIIeCard, addr: u16) {
        card.cpu.bus.state.borrow_mut().host.ctrl2 = 0x88;
        card.cpu.reset();
        card.cpu.reset_pc(addr);
    }

    /// Runs the 65C02 for `steps` CPU instructions worth of time.
    fn run(card: &mut PdsIIeCard, steps: usize) {
        for _ in 0..steps * 16 {
            card.tick(16, ()).unwrap();
        }
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
}
