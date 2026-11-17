pub mod mdc12;
pub mod se30video;
pub mod toby;

use crate::bus::Address;
use crate::bus::access::{
    BusAccess, BusResponse, CpuBus, bus_access_long, bus_access_word, write_lanes,
};
use crate::debuggable::Debuggable;
use crate::devices::{BusOps, DeviceOps, MadeDevice};
use crate::emulator::EmuContext;
use crate::mac::dma::DmaOps;
use crate::tickable::Ticks;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// What a NuBus slot sees of a card
#[typetag::serde(tag = "type")]
pub(crate) trait NubusSlot: Send + DeviceOps + Debuggable {
    /// One NuBus read: `size` bytes (1, 2 or 4) at `addr`, aligned to the
    /// size, right-justified as the 68k sees them. `addr` is the NuBus address
    /// with the digit of the slot cleared (see [`card_address`]): $F0xx_xxxx
    /// in the card's standard slot space and $0xxx_xxxx in its super slot
    /// space. None: the card doesn't answer at that address.
    fn read_sized(&mut self, addr: Address, size: u32) -> Option<u32>;

    /// One NuBus write, as `read_sized`
    fn write_sized(&mut self, addr: Address, size: u32, val: u32) -> Option<()>;

    /// Called by the machine to specify the slot and the tick rate, once, when the bus attaches
    /// the card and before any other method. `slot` is the slot number, $9 to $E. `tick_rate`
    /// is the number of ticks in one second, for the ticks that `tick()` counts. Nothing by
    /// default.
    fn connected(&mut self, _slot: u8, _tick_rate: Ticks) {}

    fn get_irq(&self) -> bool;
    fn reset(&mut self);

    /// Called by the machine at each of its ticks, with the time that passed. A card that
    /// masters the bus does its DMA here, through `dma`.
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext, dma: &mut dyn DmaOps) -> Result<()>;
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
pub(crate) fn card_access<B: CpuBus>(
    acc: BusAccess<B>,
    waits: Ticks,
    card: &mut dyn NubusSlot,
) -> BusResponse {
    // The helpers want a sized device: the card's reference is one
    let mut card = card;
    let read = |card: &mut &mut dyn NubusSlot, addr: Address, len: u32| {
        read_cycle(card_address(addr), len, |a, size| card.read_sized(a, size))
    };
    let write = |card: &mut &mut dyn NubusSlot, addr: Address, len: u32, val: u32| {
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

/// The ids of the slots $9 to $E
const SLOT_IDS: [&str; 6] = ["9", "A", "B", "C", "D", "E"];

/// The number of a slot, $9 to $E
fn slot_number(slot: &str) -> Option<u8> {
    let index = SLOT_IDS.iter().position(|&id| id == slot)?;
    Some(0x9 + index as u8)
}

/// The cards in the NuBus slots of a machine, $9 to $E
#[derive(Serialize, Deserialize)]
pub struct Nubus {
    cards: [Option<Box<dyn NubusSlot>>; 6],

    /// The slots the machine has, of $9 to $E
    slots: [bool; 6],

    /// The number of ticks in one second, for the ticks that `tick()` counts
    tick_rate: Ticks,
}

impl Nubus {
    /// The NuBus of a machine with the slots of these ids, and no card
    pub fn new(tick_rate: Ticks, slots: &[&str]) -> Self {
        Self {
            cards: core::array::from_fn(|_| None),
            slots: SLOT_IDS.map(|id| slots.contains(&id)),
            tick_rate,
        }
    }

    /// The place in `cards` of a slot the machine has
    fn index(&self, slot: &str) -> Option<usize> {
        let index = usize::from(slot_number(slot)? - 0x9);
        self.slots[index].then_some(index)
    }

    pub(crate) fn cards_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn NubusSlot>> {
        self.cards.iter_mut().flatten()
    }

    /// A CPU cycle in the standard slot space ($Fsxx_xxxx) or the super slot
    /// space ($sxxx_xxxx) of slot s, for the card in that slot. None if the
    /// slot is empty.
    pub fn access<B: CpuBus>(&mut self, acc: BusAccess<B>, waits: Ticks) -> Option<BusResponse> {
        let slot = match acc.addr >> 28 {
            0xF => (acc.addr >> 24) & 0xF,
            s => s,
        };
        let card = self.cards.get_mut((slot as usize).checked_sub(0x9)?)?;
        Some(card_access(acc, waits, card.as_deref_mut()?))
    }

    pub fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext, dma: &mut dyn DmaOps) -> Result<()> {
        for card in self.cards_mut() {
            card.tick(ticks, ctx, dma)?;
        }
        Ok(())
    }

    /// The interrupt request of each slot, bit n for slot $9 + n, as VIA 2
    /// port A has them on every machine, but active high
    pub fn irqs(&self) -> u8 {
        self.cards
            .iter()
            .enumerate()
            .filter(|(_, card)| card.as_ref().is_some_and(|c| c.get_irq()))
            .fold(0, |irqs, (i, _)| irqs | (1 << i))
    }

    pub fn reset(&mut self) {
        for card in self.cards_mut() {
            card.reset();
        }
    }
}

impl BusOps for Nubus {
    fn device(&mut self, slot: &str) -> Option<&mut dyn DeviceOps> {
        let card = self.cards[self.index(slot)?].as_mut()?;
        // A card is a DeviceOps too
        Some(&mut **card)
    }

    fn attach(&mut self, slot: &str, device: MadeDevice) -> Result<()> {
        let index = self.index(slot).context("No such NuBus slot")?;
        let MadeDevice::Nubus(mut card) = device else {
            bail!("Not a NuBus card");
        };
        if self.cards[index].is_some() {
            bail!("The NuBus slot already has a card");
        }
        card.connected(0x9 + index as u8, self.tick_rate);
        self.cards[index] = Some(card);
        Ok(())
    }

    fn detach(&mut self, slot: &str) -> Result<()> {
        let index = self.index(slot).context("No such NuBus slot")?;
        if self.cards[index].take().is_none() {
            bail!("The NuBus slot has no card");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::mdc12::Mdc12;
    use super::toby::Toby;
    use super::*;
    use crate::mac::MacMonitor;

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

    #[test]
    fn cards_are_saved_and_loaded() {
        let mut nubus = Nubus::new(1, &["9", "A", "B"]);
        let card = Box::new(Mdc12::new(&[], Some(MacMonitor::RGB12)));
        nubus.attach("9", MadeDevice::Nubus(card)).unwrap();
        let card = Box::new(Toby::new(&[]));
        nubus.attach("B", MadeDevice::Nubus(card)).unwrap();
        // A slot the machine doesn't have, and one that is taken
        assert!(
            nubus
                .attach("C", MadeDevice::Nubus(Box::new(Toby::new(&[]))))
                .is_err()
        );
        assert!(
            nubus
                .attach("B", MadeDevice::Nubus(Box::new(Toby::new(&[]))))
                .is_err()
        );

        let saved = serde_json::to_string(&nubus).unwrap();
        let nubus: Nubus = serde_json::from_str(&saved).unwrap();
        let cards = nubus
            .cards
            .iter()
            .map(|card| card.as_ref().map(|c| c.info().name));
        assert!(cards.eq([
            Some("Macintosh Display Card 8-24"),
            None,
            Some("Macintosh II Video Card (Toby)"),
            None,
            None,
            None,
        ]));
    }
}
