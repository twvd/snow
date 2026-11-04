pub mod mdc12;
pub mod se30video;
pub mod toby;

use crate::bus::Address;
use crate::bus::access::{
    Bus68020, BusAccess, BusResponse, bus_access_long, bus_access_word, write_lanes,
};
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::renderer::Renderer;
use crate::tickable::{Tickable, Ticks};
use anyhow::Result;
use mdc12::Mdc12;
use se30video::SE30Video;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use toby::Toby;

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub enum NubusCard<TRenderer: Renderer> {
    MDC12(Mdc12<TRenderer>),
    SE30Video(SE30Video<TRenderer>),
    Toby(Toby<TRenderer>),
}

/// What a NuBus slot sees of a card
pub trait NubusSlot<TRenderer>:
    for<'a> Tickable<&'a dyn EmuContext> + Debuggable + Display
{
    /// One NuBus read: `size` bytes (1, 2 or 4) at `offset` in the slot,
    /// aligned to the size, right-justified as the 68k sees them. None: the
    /// card refuses it, which is a bus error.
    fn read_sized(&mut self, offset: Address, size: u32) -> Option<u32>;

    /// One NuBus write, as `read_sized`
    fn write_sized(&mut self, offset: Address, size: u32, val: u32) -> Option<()>;

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
            NubusCard::SE30Video($c) => $e,
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

    // Statically dispatched, for the paths run on every bus tick and access

    pub fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        with_card!(self, c => c.tick(ticks, ctx))
    }

    pub fn get_irq(&self) -> bool {
        with_card!(self, c => c.get_irq())
    }

    pub fn read_sized(&mut self, offset: Address, size: u32) -> Option<u32> {
        with_card!(self, c => c.read_sized(offset, size))
    }

    pub fn write_sized(&mut self, offset: Address, size: u32, val: u32) -> Option<()> {
        with_card!(self, c => c.write_sized(offset, size, val))
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

/// A 68020 or 68030 cycle in a card's standard slot space, as the NuChip of
/// the Mac II, IIx and IIcx makes it into NuBus transactions for the card
pub fn nuchip_access<TRenderer, C>(
    acc: BusAccess<Bus68020>,
    waits: Ticks,
    card: &mut C,
) -> BusResponse
where
    C: NubusSlot<TRenderer> + ?Sized,
{
    // The helpers want a sized device: the card's reference is one
    let mut card = card;
    let read = |card: &mut &mut C, addr: Address, len: u32| {
        read_cycle(addr & 0xFF_FFFF, len, |o, size| card.read_sized(o, size))
    };
    let write = |card: &mut &mut C, addr: Address, len: u32, val: u32| {
        let offset = addr & 0xFF_FFFF;
        debug_assert!(
            natural(offset, len),
            "{len} bytes at {offset:06X} is no NuBus write"
        );
        card.write_sized(offset, len, val)
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
