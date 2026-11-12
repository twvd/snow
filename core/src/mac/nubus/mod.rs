pub mod mdc12;
pub mod se30video;
pub mod toby;

use crate::bus::Address;
use crate::bus::access::{
    BusAccess, BusResponse, CpuBus, bus_access_long, bus_access_word, write_lanes,
};
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::renderer::Renderer;
use crate::tickable::{Tickable, Ticks};
use anyhow::Result;
use mdc12::Mdc12;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use toby::Toby;

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub enum NubusCard<TRenderer: Renderer> {
    MDC12(Mdc12<TRenderer>),
    Toby(Toby<TRenderer>),
}

/// What a NuBus slot sees of a card
pub trait NubusSlot<TRenderer>:
    for<'a> Tickable<&'a dyn EmuContext> + Debuggable + Display
{
    /// One NuBus read: `size` bytes (1, 2 or 4) at `addr`, aligned to the
    /// size, right-justified as the 68k sees them. `addr` is the NuBus address
    /// with the digit of the slot cleared (see [`card_address`]): $F0xx_xxxx
    /// in the card's standard slot space and $0xxx_xxxx in its super slot
    /// space. None: the card doesn't answer at that address.
    fn read_sized(&mut self, addr: Address, size: u32) -> Option<u32>;

    /// One NuBus write, as `read_sized`
    fn write_sized(&mut self, addr: Address, size: u32, val: u32) -> Option<()>;

    fn get_irq(&self) -> bool;
    fn reset(&mut self);
    fn blank(&mut self) -> Result<()>;
    fn render(&mut self) -> Result<()>;
    /// Reinstalls the renderer, which is not serialized
    fn set_renderer(&mut self, renderer: TRenderer);
}

/// Evaluates `$e` with `$c` bound to the card in a NubusCard
macro_rules! with_card {
    ($card:expr, $c:ident => $e:expr) => {
        match $card {
            NubusCard::MDC12($c) => $e,
            NubusCard::Toby($c) => $e,
        }
    };
}

impl<TRenderer> NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    pub fn slot(&mut self) -> &mut dyn NubusSlot<TRenderer> {
        with_card!(self, c => c)
    }

    pub fn slot_ref(&self) -> &dyn NubusSlot<TRenderer> {
        with_card!(self, c => c)
    }
}

// Statically dispatched, for the paths run on every bus tick and access

impl<TRenderer> NubusSlot<TRenderer> for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_irq(&self) -> bool {
        with_card!(self, c => c.get_irq())
    }

    fn read_sized(&mut self, addr: Address, size: u32) -> Option<u32> {
        with_card!(self, c => c.read_sized(addr, size))
    }

    fn write_sized(&mut self, addr: Address, size: u32, val: u32) -> Option<()> {
        with_card!(self, c => c.write_sized(addr, size, val))
    }

    fn reset(&mut self) {
        with_card!(self, c => c.reset());
    }

    fn blank(&mut self) -> Result<()> {
        with_card!(self, c => c.blank())
    }

    fn render(&mut self) -> Result<()> {
        with_card!(self, c => c.render())
    }

    fn set_renderer(&mut self, renderer: TRenderer) {
        with_card!(self, c => c.set_renderer(renderer));
    }
}

impl<TRenderer> Tickable<&dyn EmuContext> for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        with_card!(self, c => c.tick(ticks, ctx))
    }
}

/// The address a card is given of a NuBus address in its slot: the digit of
/// the slot is cleared, so that the address map of a card doesn't depend on
/// the slot it's in.
///
/// Each slot has a standard slot space, $Fsxx_xxxx, and a super slot space,
/// $sxxx_xxxx. No NuBus signal tells the two apart: a card compares the
/// address with the ID of its slot, either bits 27-24 when bits 31-28 are all
/// ones, or bits 31-28 (Designing Cards and Drivers for the Macintosh Family,
/// 3rd ed., ch. 3). Without the slot, the card's standard slot space is
/// $F0xx_xxxx and its super slot space $0xxx_xxxx.
pub const fn card_address(addr: Address) -> Address {
    if addr >> 28 == 0xF {
        addr & 0xF0FF_FFFF
    } else {
        addr & 0x0FFF_FFFF
    }
}

/// The offset of the address `addr` a card is given in its standard slot
/// space, or None if it is in its super slot space
pub const fn standard_offset(addr: Address) -> Option<Address> {
    if addr >> 28 == 0xF {
        Some(addr & 0xFF_FFFF)
    } else {
        None
    }
}

/// What a read of `size` bytes at `offset` takes of the long word a card
/// drives, right-justified.
///
/// A card that decodes only the address of a register answers every size
/// with the whole long.
pub const fn lanes(long: u32, offset: Address, size: u32) -> u32 {
    (long >> (8 * (4 - offset % 4 - size))) & (u32::MAX >> (32 - 8 * size))
}

/// The long word on the bus during a write of `size` bytes at `offset`.
///
/// The CPU repeats a byte or a halfword on the lanes it doesn't fill, so a
/// card that decodes only the address finds the data in its own lanes
/// whatever the size.
pub fn bus_long(val: u32, size: u32, offset: Address) -> u32 {
    write_lanes(val, size, offset)
}

/// Whether `size` bytes at `offset` make a NuBus transaction: a byte, a
/// halfword at an even offset or a word at a multiple of 4
const fn natural(offset: Address, size: u32) -> bool {
    size == 1 || (size == 2 && offset.is_multiple_of(2)) || (size == 4 && offset.is_multiple_of(4))
}

/// Whether a 68020 or 68030 write cycle at `addr`, with `size` bytes left of
/// the operand, is a NuBus transaction: the bytes of the cycle on a 32-bit
/// port, up to the end of the word, make one
fn takes_write_cycle(addr: Address, size: u32) -> bool {
    let lane = addr % 4;
    natural(lane, (4 - lane).min(size))
}

/// `len` bytes at `offset`, within one word: a NuBus transaction as it is, or
/// the bytes asked for out of a read of the whole word
fn read_cycle(
    offset: Address,
    len: u32,
    read: impl FnOnce(Address, u32) -> Option<u32>,
) -> Option<u32> {
    if natural(offset, len) {
        return read(offset, len);
    }
    let lane = offset % 4;
    let word = read(offset - lane, 4)?;
    Some((word >> (8 * (4 - lane - len))) & (u32::MAX >> (32 - 8 * len)))
}

// Reads and writes don't split the same way, because the NuChip answers them
// differently.
//
// Designing Cards and Drivers for the Macintosh II and SE (1987), ch. 3,
// "Non-aligned MC68020 accesses" (the same text is in the 2nd edition, 1990,
// for the whole Macintosh II family, and in the 3rd, 1992): "The Macintosh II
// to NuBus interface will respond to off-boundary MC68020 requests with DSACKs
// which tell the processor that the bus is only 16 bits wide. This will cause
// the MC68020 to make several cycles to fulfill the original request". Its two
// examples:
// - "a longword read of $Fs00 0001 generates a word read to $Fs00 0000 and a
//   byte read to $Fs00 0004";
// - "a longword write to $Fs00 0001 would be performed in three pieces: a
//   byte write to $Fs00 0001, a halfword write to $Fs00 0002, and a byte write
//   to $Fs00 0004".
//
// The text doesn't say whether the 16-bit answer is for writes only. The write
// example is what a 16-bit answer to each off-boundary cycle gives. The read
// example can only be two transactions if its first cycle is answered as
// 32-bit: the word read at 0 gives the CPU bytes 1 to 3, and its next cycle is
// the byte at 4. Answered as 16-bit, the CPU would take byte 1 alone and come
// back for a halfword at 2, which makes three. B. G. Taylor, "Developing for
// the Macintosh NuBus" (CERN-PRE 89-050, EuroBus 89), section 10.2, gives the
// same counts: "All non-aligned reads can be completed by a maximum of two
// NuBus transactions, since the additional bytes acquired by 32-bit word reads
// can be ignored by the processor. In the case of non-aligned writes, however,
// up to three NuBus transactions may be required".
//
// So a read cycle is answered as 32-bit, and a nonaligned one reads the whole
// word. A write cycle that isn't a NuBus transaction is answered as a 16-bit
// port, the CPU makes the pieces itself, and the card only ever gets NuBus
// transactions.

/// A CPU cycle in a card's slot space, made into NuBus transactions for it
///
/// This is what the NuChip of the Mac II, IIx and IIcx does. A 68040 makes only
/// cycles that are NuBus transactions already, and they go through unchanged.
pub fn card_access<B, TRenderer, C>(acc: BusAccess<B>, waits: Ticks, card: &mut C) -> BusResponse
where
    B: CpuBus,
    C: NubusSlot<TRenderer> + ?Sized,
{
    // The helpers want a sized device: the card's reference is one
    let mut card = card;
    let read = |card: &mut &mut C, addr: Address, len: u32| {
        read_cycle(card_address(addr), len, |a, size| card.read_sized(a, size))
    };
    let write = |card: &mut &mut C, addr: Address, len: u32, val: u32| {
        let addr = card_address(addr);
        debug_assert!(
            natural(addr, len),
            "{len} bytes at {addr:08X} is no NuBus write"
        );
        card.write_sized(addr, len, val)
    };
    if acc.write && !takes_write_cycle(acc.addr, acc.size) {
        bus_access_word(acc, waits, &mut card, read, write)
    } else {
        bus_access_long(acc, waits, &mut card, read, write)
    }
}

impl<TRenderer> Debuggable for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        self.slot_ref().get_debug_properties()
    }
}

impl<TRenderer> Display for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.slot_ref().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transactions a read makes, and what it returns from a card
    /// answering 0x10 + offset in each byte
    fn read(offset: Address, len: u32) -> (Vec<(Address, u32)>, u32) {
        let mut seen = vec![];
        let val = read_cycle(offset, len, |o, size| {
            seen.push((o, size));
            Some((o..o + size).fold(0, |v, a| (v << 8) | (0x10 + a)))
        })
        .unwrap();
        (seen, val)
    }

    /// The CPU cycles of a write of `size` bytes at `addr`: the bytes of each on a
    /// 32-bit port, or a 16-bit one where the interface answers as such
    fn cpu_write_cycles(mut addr: Address, mut size: u32) -> Vec<(Address, u32)> {
        let mut cycles = vec![];
        while size > 0 {
            let port = if takes_write_cycle(addr, size) { 4 } else { 2 };
            let len = (port - addr % port).min(size);
            cycles.push((addr, len));
            addr += len;
            size -= len;
        }
        cycles
    }

    #[test]
    fn aligned_reads_are_one_transaction() {
        assert_eq!(read(0, 4), (vec![(0, 4)], 0x1011_1213));
        assert_eq!(read(2, 2), (vec![(2, 2)], 0x1213));
        assert_eq!(read(3, 1), (vec![(3, 1)], 0x13));
    }

    #[test]
    fn a_nonaligned_read_reads_the_word() {
        // The first cycle of a long read at 1, then a halfword read at 1
        assert_eq!(read(1, 3), (vec![(0, 4)], 0x11_1213));
        assert_eq!(read(1, 2), (vec![(0, 4)], 0x1112));
        // The second cycle of a long read at 3
        assert_eq!(read(0, 3), (vec![(0, 4)], 0x10_1112));
    }

    #[test]
    fn a_nonaligned_write_is_cycles_the_cpu_makes() {
        // The book's example: a long write at 1 is a byte, a halfword and a byte
        assert_eq!(cpu_write_cycles(1, 4), [(1, 1), (2, 2), (4, 1)]);
        assert_eq!(cpu_write_cycles(3, 4), [(3, 1), (4, 2), (6, 1)]);
        assert_eq!(cpu_write_cycles(1, 2), [(1, 1), (2, 1)]);
        // Aligned ones stay whole
        assert_eq!(cpu_write_cycles(0, 4), [(0, 4)]);
        assert_eq!(cpu_write_cycles(2, 4), [(2, 2), (4, 2)]);
        assert_eq!(cpu_write_cycles(6, 2), [(6, 2)]);
    }
}
