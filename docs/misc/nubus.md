# NuBus

NuBus is the expansion bus of the Macintosh II family and the Quadras. This document describes
how Snow models it: how a card sees the CPU's accesses and the two address spaces of a slot.

Terms used here:

- **Slot:** a NuBus connector, numbered `$9` to `$E`. The number is also the slot's place in the
  address map.
- **Card:** a device in a slot, such as the Macintosh Display Card 8-24.
- **Transaction:** one NuBus read or write, of a byte, a halfword or a word, at an address aligned
  to its size.
- **Cycle:** one access of the CPU on its own bus. A cycle can be of any size and alignment the CPU
  allows, so it may not be a single transaction.

## Cards take whole transactions

A card answers transactions through its slot trait, `NubusSlot`. It is a subtrait of `DeviceOps`,
which every device implements:

```rust
pub trait NubusSlot: Send + DeviceOps + Debuggable {
    /// One NuBus read: `size` bytes (1, 2 or 4) at `addr`, aligned to the size, right-justified
    /// as the 68k sees them. `addr` has the digit of the slot cleared: $F0xx_xxxx or $0xxx_xxxx.
    /// None: the card doesn't answer at that address.
    fn read_sized(&mut self, addr: Address, size: u32) -> Option<u32>;
    fn write_sized(&mut self, addr: Address, size: u32, val: u32) -> Option<()>;
    /// Called by the machine to specify the slot and the tick rate, once, when the bus attaches
    /// the card and before any other method. `slot` is the slot number, $9 to $E. `tick_rate`
    /// is the number of ticks in one second, for the ticks that `tick()` counts. Nothing by
    /// default.
    fn connected(&mut self, slot: u8, tick_rate: Ticks) {}
    fn get_irq(&self) -> bool;
    fn reset(&mut self);
    /// Time passing. A card that masters the bus does its DMA here, through `dma`.
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext, dma: &mut dyn DmaOps) -> Result<()>;
}
```

`nubus::card_access()` turns a CPU cycle into transactions, as the NuChip of the Mac II, IIx and
IIcx does (Designing Cards and Drivers for the Macintosh II and SE, ch. 3, "Non-aligned MC68020
accesses"):

- A read cycle is answered as 32-bit: a read that isn't aligned reads the whole word, and the CPU
  takes the bytes it wanted.
- A write cycle that isn't a transaction is answered as a 16-bit port, so the CPU splits it into
  pieces itself.

A 68040 only makes cycles that are transactions already, which go through unchanged. The wait
states stay in the machine's bus code, around the access.

## Standard and super slot spaces

Each slot has two address spaces (Designing Cards and Drivers for the Macintosh Family, 3rd ed.,
ch. 3 and 7), where `s` is the slot number:

| Space | Addresses | Size |
| --- | --- | --- |
| Standard slot space | `$Fsxx_xxxx` | 16 MB |
| Super slot space | `$sxxx_xxxx` | 256 MB |

No NuBus signal tells them apart: a card compares the address with the number of its slot, either
bits 27-24 when bits 31-28 are all ones, or bits 31-28.

The bus gives a card only the addresses of its own slot, in either space, with the digit of the
slot cleared (`nubus::card_address()`): `$F0xx_xxxx` in its standard slot space and `$0xxx_xxxx` in
its super slot space. So a card still tells the two spaces apart, as a real one does, and its
address map doesn't depend on the slot it is in. For example, the control register of an 8-24 is
at `$F920_0000` when the card is in slot `$9`, and at `$FA20_0000` in slot `$A`; in both cases the
card sees `$F020_0000`, which `standard_offset()` makes offset `$20_0000`.

`nubus::standard_offset()` gives the offset in the standard slot space, and None in the super slot
space. Every card in Snow refuses its super slot space today; a card that needs it only has to
answer there. A card's read typically starts like this:

```rust
fn read_sized(&mut self, addr: Address, size: u32) -> Option<u32> {
    let offset = standard_offset(addr)?;   // None: the super slot space, not answered
    // ... then decode `offset`, from $00_0000 to $FF_FFFF
}
```

When no card answers in super slot space, the Mac II family reads open bus, the Quadra 700 reads
0, and the Quadra 800 gets a bus error. A standard slot space that nothing answers is a bus error,
as the Slot Manager expects of an empty slot.

## The cards of a machine

`nubus::Nubus` holds the cards of slots `$9` to `$E`, for every machine with NuBus slots. It:

- routes a cycle in either address space of a slot to the card there;
- ticks the cards;
- gives their interrupts, one bit per slot: bit n for slot `$9 + n`, as VIA 2 port A has them on
  every machine;
- gives the emulator's device code the card in a slot, as the bus's `BusOps`.

A card is made by the create function its `ModelInfo` names, which returns `MadeDevice::Nubus`.
`BusOps::attach()` then puts it in its slot, and tells it the number of the slot and the rate of
its ticks with `connected()`. This is done while the machine is being made, and never once it has
run. The cards are trait objects, `Box<dyn NubusSlot>`, as SCSI targets are: `Nubus` talks to a
card through `NubusSlot` and doesn't know which card it is. The `typetag` crate lets serde save
and restore them. A card with video provides the Display function and creates its own renderer.

| Mac model | Slots | Slot interrupts | Source |
| --- | --- | --- | --- |
| Mac II, IIx | `$9` to `$E` | VIA 2 PA0-PA5 | Guide to the Macintosh Family Hardware, 2nd ed., table 4-9 |
| IIcx | `$9` to `$B` | VIA 2 PA0-PA2 | Same table |

The slot interrupts combine into VIA 2 CA1.

**The SE/30's video isn't a card.** It answers in the space of slot `$E`, `$FExx_xxxx`, but it is on
the logic board, with no NuBus between it and the CPU. It is a field of the SE/30's bus of its own,
and has plain `read()` and `write()` methods taking an offset in that space.

## Bus masters

A NuBus card can be a bus master: it then reads and writes the machine's memory by itself, without
the CPU, which is often called DMA. It does so in its `tick()`, through the `dma` argument: the
two operations of `DmaOps`, which the machine provides. They reach the machine's RAM at the
address the CPU uses, take no time, and don't touch the CPU's caches. A write through them shows
in the memory views of the debugger.

## Adding a card

A new card is one file in `core/src/mac/nubus/`. The card:

1. implements `DeviceOps`, `NubusSlot` and `Debuggable`, with `#[typetag::serde]` on its
   `impl NubusSlot` so that save states keep it. If it has video, it also implements
   `DisplayFunction`, and creates its own renderer;
2. has a `ModelInfo` with `fixed: true`, its ROM if it has one, and a create function returning
   `MadeDevice::Nubus`;
3. gets its device model and the arm for its `ModelInfo` in `DeviceModel::info()`.
