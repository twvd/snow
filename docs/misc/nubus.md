# NuBus

NuBus is the expansion bus of the Macintosh II family and the Quadras. This document describes
how Snow models it: how a card sees the CPU's accesses, the two address spaces of a slot, and how a
machine holds its cards. How a card is chosen, made and saved is in the
[device model](device-model.md), where a card is a device on the `NuBus` bus.

Terms used here:

- **Slot:** a NuBus connector, numbered `$9` to `$E`. The number is also the slot's place in the
  address map.
- **Card:** a device in a slot, such as the Macintosh Display Card 8-24.
- **Transaction:** one NuBus read or write, of a byte, a halfword or a word, at an address aligned
  to its size.
- **Cycle:** one access of the CPU on its own bus. A cycle can be of any size and alignment the CPU
  allows, so it may not be a single transaction.

## Cards take whole transactions

A card answers transactions through `NubusSlot`, the slot trait of the NuBus. It is a subtrait of
`DeviceOps`, which every device implements
([device model](device-model.md#device-operations-and-slot-traits)):

```rust
pub(crate) trait NubusSlot: DeviceOps + Debuggable + Send {
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
    /// For a card with video: which of its displays the guest knows by this id of a slot
    /// resource, as an index from 0. None by default: the card shows nothing.
    fn display_from_Mac_dCtlSlotId(&self, dctl_slot_id: u8) -> Option<usize> { None }
}
```

The trait is `Send` because the machine, with its cards, moves to the emulator's thread.
`display_from_Mac_dCtlSlotId()` is for machines with several displays
([displays.md](displays.md#several-displays)).

When the CPU accesses a NuBus card, the machine's bus uses a helper common to all machines,
`nubus::card_access()`, to turn the CPU's cycle into NuBus transactions. This helper, in turn,
calls the card's `read_sized()` or `write_sized()`, once for each transaction.

It makes the transactions as the NuChip of the Mac II, IIx and IIcx does (Designing Cards and
Drivers for the Macintosh II and SE, ch. 3, "Non-aligned MC68020 accesses"):

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

No NuBus signal tells them apart. On real hardware every card sees every address on the bus, with
the slot number in it. A card knows the number of its own slot from four pins of its connector
(`/ID3-/ID0`), and compares the address with it: either bits 27-24 when bits 31-28 are all ones,
or bits 31-28 (Designing Cards and Drivers, 3rd ed., ch. 2, "Card slot identification signals").

**In Snow, the `Nubus` helper does that comparison for the cards**, which differs from real
hardware. It calls only the card of the slot an address belongs to, so it doesn't have to ask
every card at each access. It gives the card the address with the digit of the slot cleared
(`nubus::card_address()`): `$F0xx_xxxx` in its standard slot space and `$0xxx_xxxx` in its super
slot space. So a card still tells the two spaces apart, and its address map doesn't depend on the
slot it is in. A card can't answer outside the two spaces of its own slot.

For example, the control register of an 8-24 is at `$F920_0000` when the card is in slot `$9`, and
at `$FA20_0000` in slot `$A`; in both cases the card sees `$F020_0000`, which `standard_offset()`
makes offset `$20_0000`.

A card is told the number of its slot by `connected()`, as a real card reads it from its
`/ID3-/ID0` pins. It needs it for what it does by itself and not in answer to an access, since the
addresses it is given have the digit of the slot cleared.

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

`nubus::Nubus` is where a machine has the cards of slots `$9` to `$E`, for every machine with NuBus
slots. A machine makes it with `Nubus::new(tick_rate, slots)`: the rate of the ticks it will give
the cards, which each card is told in `connected()`, and the ids of the slots that machine has.
It:

- routes a cycle in either address space of a slot to the card there;
- ticks the cards;
- gives their interrupts, one bit per slot: bit n for slot `$9 + n`, as VIA 2 port A has them on
  every machine;
- gives the emulator's device code the card in a slot, as the bus's `BusOps`
  ([device model](device-model.md#how-the-emulator-reaches-a-device-busops-and-deviceops)).

A card is made by the create function its `ModelInfo` names, which returns `MadeDevice::Nubus`.
`BusOps::attach()` then puts it in its slot. This is done while the machine is being made, and
never once it is made. The cards are trait objects, `Box<dyn NubusSlot>`, as SCSI targets are:
`Nubus` talks to a card through `NubusSlot` and doesn't know which card it is. A card with video
provides the Display function and creates its own renderer ([displays.md](displays.md)).

| Mac model | Slots | Slot interrupts | Source |
| --- | --- | --- | --- |
| Mac II, IIx | `$9` to `$E` | VIA 2 PA0-PA5 | Guide to the Macintosh Family Hardware, 2nd ed., table 4-9 |
| IIcx | `$9` to `$B` | VIA 2 PA0-PA2 | Same table |
| Quadra 700 | `$D`, `$E` (the PDS shares `$E`) | VIA 2 PA4, PA5 | Designing Cards and Drivers, 3rd ed., table 16-6 |
| Quadra 800 | `$C` to `$E` (the PDS shares `$E`) | VIA 2 PA3-PA5 | As MAME has them; IOSB holds the VIAs |

The slot interrupts, and on the Quadras the built-in video's, combine into VIA 2 CA1. The wait
states of a transaction on the Quadras are estimates, for the 10 MHz NuBus and the crossing to the
CPU's clock. The NuBus controllers of the Quadras, the YANCC and Kiwi, take a write in a buffer and
let the CPU go on, which isn't modelled.

The PDS slots of the Quadras aren't modelled yet; those of the LC family and of the SE/30 are in
[pds.md](pds.md). A PDS card that uses slot `$E` tells the NuBus controller with `/PDS.SLOT.E.EN`,
which then leaves `$Exxx_xxxx` and `$FExx_xxxx` to it.

**The SE/30's video isn't a card.** It answers in the space of slot `$E`, `$FExx_xxxx`, but it is on
the logic board, with no NuBus between it and the CPU. It is the SE/30's built-in video, at
`Built-in` / `video` in the device model, and has plain `read()` and `write()` methods taking an
offset in that space.

## Interrupts

Each NuBus slot has an interrupt line of its own. A card says whether it is asking for an interrupt
with `NubusSlot::get_irq()`. It is a level, not an event: the card keeps answering true until the
guest has dealt with the cause, by reading or writing a register of the card.

`Nubus::irqs()` gathers the lines of all the cards, one bit per slot. The machine's bus reads it
each time it has ticked the cards, and does with it what the hardware does:

- It puts the bits on VIA 2 port A, inverted. The guest reads them there to find which slot is
  asking.
- It asserts VIA 2's CA1 line for as long as any bit is set, which interrupts the CPU.

The table in [The cards of a machine](#the-cards-of-a-machine) gives the bit of each slot on each
Mac model. A built-in video that the guest sees as a card joins in the same way: the SE/30's as
slot `$E`, and the Quadras' through CA1.

## Bus masters

A NuBus card can be a bus master: it then reads and writes the machine's memory by itself, without
the CPU, which is often called DMA. It does so in its `tick()`, through the `dma` argument. How
that works in Snow, and what Snow doesn't model of it, is in [dma.md](dma.md): it is the same for
a NuBus card as for any other device.

## Adding a card

A new card is one file: in `core/src/mac/nubus/`, or with the other devices of its family when it
has one, as the Ethernet cards are with the SONIC in `core/src/mac/ethernet/`. The card:

1. implements `DeviceOps`, `NubusSlot` and `Debuggable`. If it has video, it also
   implements `DisplayFunction`, and creates its own renderer;
2. has a `ModelInfo` with `BusKind::Nubus` as its `bus`, `fixed: true`, its ROM if it has one,
   and a create function returning `MadeDevice::Nubus`. `ModelInfo::base()` makes it fit every
   Mac model with NuBus slots;
3. gets its device model and the arm for its `ModelInfo` in `DeviceModel::info()`, as in the
   [device model](device-model.md#adding-a-device-model).
