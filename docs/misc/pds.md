# Processor-direct slots

A processor-direct slot, or PDS, is an expansion connector wired to the CPU's own bus. The
Macintosh LC family has one, and the SE/30 has one of another type. This document describes how
Snow models them: what a machine asks of the card in its slot, the addresses a card answers at, and
how a machine holds its card. How a card is chosen, made and saved is in the
[device model](device-model.md), where a card is a device on the `PDS_LC` or the `PDS_SE30` bus.

Terms used here:

- **Slot:** the processor-direct connector of a Mac. A Mac has one at most.
- **Card:** a device in the slot, such as the Apple IIe Card.
- **Cycle:** one access of the CPU on its bus.

## One slot trait for every type of slot

A card answers the machine through `PdsSlot`, the slot trait of the processor-direct slots. It is
a subtrait of `DeviceOps`, which every device implements
([device model](device-model.md#device-operations-and-slot-traits)):

```rust
pub(crate) trait PdsSlot: DeviceOps + Debuggable + Send {
    /// One cycle of the CPU: `size` bytes (1, 2 or 4) at `addr`, the address as it is on the
    /// CPU's bus. `map32`: the cycle uses the 32-bit memory map. None: the card doesn't answer
    /// there. Called by the machine for a cycle in the address ranges it gives the slot.
    fn read(&mut self, addr: Address, size: u32, map32: bool) -> Option<u32>;
    fn write(&mut self, addr: Address, size: u32, map32: bool, val: u32) -> Option<()>;
    /// Called by the machine to specify the tick rate, once, when the bus attaches the card and
    /// before any other method. `tick_rate` is the number of ticks in one second, for the ticks
    /// that `tick()` counts. Nothing by default.
    fn connected(&mut self, tick_rate: Ticks) {}
    /// The levels of the card's interrupt lines, one bit per line of the slot. Called by the
    /// machine at each of its ticks.
    fn irqs(&self) -> u8;
    /// `hard`: the machine was powered on, and not only reset
    fn reset(&mut self, hard: bool);
    /// Time passing. A card that masters the bus does its DMA here, through `dma`.
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext, dma: &mut dyn DmaOps) -> Result<()>;
    /// The average level of the card's SNDOUT line since the last call, from 0 (low) to 1 (high).
    /// 0 by default: the card doesn't drive the line.
    fn sound_pin_value(&mut self) -> f32 { 0.0 }
}
```

- **`reset()` says whether the machine was powered on.** The Apple IIe Card is a computer of its
  own: only a power on resets it, and a reset of the Mac leaves it running.
- **`sound_pin_value()` is the SNDOUT signal of the slot.** It is an "input to the audio
  amplifier on the main logic board", which "allows a PDS card to drive the speaker without
  involving the CPU" (Macintosh LC III Developer Note, signals of the expansion connector). The
  Apple IIe Card puts the Apple IIe's speaker on it, and the machine mixes it into its sound
  output.
- **`irqs()` gives levels**, as a NuBus card's interrupt is ([nubus.md](nubus.md#interrupts)):
  a bit stays set until the guest has dealt with its cause. The slot of an LC has one interrupt
  line, `/SLOTIRQ`, in bit 0. The slot of an SE/30 has three, one per pseudo-slot, `$9`, `$A`
  and `$B`, in bits 0 to 2.
- **`connected()` tells the card the rate of its ticks.** A create function has only the device
  config, which says nothing of the machine, so the bus tells the card when it attaches it
  ([device model](device-model.md#device-operations-and-slot-traits)). A machine has one slot, so
  there is no slot number to give.
- **The machine sizes a cycle to the card's port.** A card on a 16-bit port, such as the Apple
  IIe Card, gets cycles of 1 or 2 bytes only: the machine splits a 32-bit cycle, as the CPU's
  dynamic bus sizing does. The card doesn't say its port size; the machine knows it.

**The types of slot share this one trait.** On real hardware they are incompatible: a card for an
LC doesn't fit an SE/30. In Snow, what a machine asks of a card is the same for the types modelled
so far, so they have one slot trait. What keeps a card out of the wrong machine is the device
model: each type of slot is a type of bus of its own, `BusKind::LcPds` or `BusKind::Se30Pds`, and a
device model fits one of them. This may change: a device model that needs something only one type
of slot has would lead to a slot trait for that type.

## The addresses of a card

On real hardware, a card in a processor-direct slot sees every address the CPU puts on its bus,
and decodes them itself. Apple gives a convention for it, called pseudo-slot design: the card
answers where a NuBus card in slot `$E` would be, so the Slot Manager finds it there and a NuBus
driver works with it (Macintosh LC Developer Note and Macintosh LC III Developer Note, "Addressing
guidelines"):

| Memory map | Where the card answers |
| --- | --- |
| 24-bit | `$E0 0000` to `$EF FFFF` |
| 32-bit | `$FE00 0000` to `$FEFF FFFF`, the standard slot space of slot `$E`, and `$E000 0000` to `$EFFF FFFF`, its super slot space |

On an LC the card is told which map a cycle uses by the `/FC3` line of the connector ("Low
selects 24-bit map; high selects 32-bit map"), and selects itself from A31 in the 32-bit map and
from A23 to A20 in the 24-bit map (Macintosh LC Developer Note, Figure 1-5).

**In Snow, the machine decides which cycles reach the slot, and the card decodes them.** The
machine gives the card every cycle in the address ranges of its slot, with the address as it is
on the CPU's bus, and `map32` for the memory map. The card answers, or returns None.

A card gets the full address, where a NuBus card gets its address with the digit of its slot
cleared ([nubus.md](nubus.md#standard-and-super-slot-spaces)). A NuBus card can be in any slot,
and clearing the digit spares it from decoding the slot it is in. A card in a processor-direct
slot is always at the same place on its machine, so it decodes its addresses as the real card
does.

| Mac model | Ranges the machine gives the slot | `map32` |
| --- | --- | --- |
| LC, LC II | The card's ranges of the table above, and `$FC 0000` to `$FF FFFF` in the 24-bit map | What `/FC3` says |
| LC III, LC 475 | The card's ranges of the table above | Always: their chips decode 32-bit addresses only |
| SE/30 | The standard and super slot spaces of `$9`, `$A` and `$B` | Always: the 68030 puts 32-bit physical addresses on its bus |

The LC's range at `$FC 0000` is outside the convention: the Apple IIe Card answers there too, and
its software uses it (its Startup INIT's base, `$50FC 0000` in the 32-bit map, lands there). The
LC Developer Note lists `$F0 0000` to `$FF FFFF` as I/O space and doesn't mention it. A card that
doesn't decode it returns None there.

The SE/30's slot has three pseudo-slots, `$9`, `$A` and `$B`, each with its own interrupt line
(Designing Cards and Drivers for the Macintosh Family, 3rd ed., ch. 15). Snow gives the card all
three, and the card answers in those it uses.

Another way would be for a card to give the machine the list of the address ranges it answers at,
so that the machine needn't know them. Snow doesn't do this: the machine would have to check that
list at each access, which is slower, and no card needs it so far.

## The card of a machine

`pds::Pds` is where a machine has the card of its slot, for every machine with a processor-direct
slot. It:

- gives a cycle to the card, if there is one;
- ticks the card;
- gives the card's interrupts and its sound level;
- gives the emulator's device code the card, as the bus's `BusOps`
  ([device model](device-model.md#how-the-emulator-reaches-a-device-busops-and-deviceops)).

A card is made by the create function its `ModelInfo` names, which returns `MadeDevice::Pds`.
`BusOps::attach()` then puts it in the slot. A card is a trait object, `Box<dyn PdsSlot>`, as a
NuBus card is: `Pds` talks to it through `PdsSlot` and doesn't know which card it is.

The machine keeps what is its own: which cycles reach the slot, the wait states and the port size
of a cycle, where the card's interrupts go, and mixing the card's sound into its sound output.

| Mac model | Where the card's interrupts go |
| --- | --- |
| LC, LC II, LC III | Bit 0 to the slot interrupt of the pseudo-VIA 2 (a level-2 interrupt when the slot interrupt is enabled; LC Developer Note, `/SLOTIRQ`) |
| LC 475 | Bit 0 to the slot interrupt of its VIA 2 |
| SE/30 | Bits 0 to 2 to VIA 2 PA0 to PA2, as slots `$9` to `$B` on a Macintosh II |

| Mac model | Bus | Cards |
| --- | --- | --- |
| LC, LC II, LC III, LC 475 | `PDS_LC` | The Apple IIe Card |
| SE/30 | `PDS_SE30` | None yet |

The PDS slots of the Quadras aren't modelled ([nubus.md](nubus.md#the-cards-of-a-machine)).

## What only one card has

The emulator's device code reaches a card as it does any device: through `DeviceOps::function()`.
What only the Apple IIe Card has, its game port and whether it is in front, is reached through its
function, `AppleIIeCardFunction` ([device model](device-model.md#functions-and-their-status)). No
machine has a method that returns the Apple IIe Card.

## Bus masters

A card in a processor-direct slot can be a bus master: it then reads and writes the machine's
memory by itself, without the CPU. It does so in its `tick()`, through the `dma` argument, and
never from inside `read()` or `write()`. How that works in Snow, and what Snow doesn't model of
it, is in [dma.md](dma.md).

## Adding a card

A new card is one file. The card:

1. implements `DeviceOps`, `PdsSlot` and `Debuggable`;
2. has a `ModelInfo` with the type of its slot in `bus`, `BusKind::LcPds` or `BusKind::Se30Pds`,
   `fixed: true`, its ROM if it has one, and a create function returning `MadeDevice::Pds`;
3. gets its device model and the arm for its `ModelInfo` in `DeviceModel::info()`, as in the
   [device model](device-model.md#adding-a-device-model).

Neither `Pds` nor the machines change for a new card that answers in the ranges its machine
already gives the slot.
