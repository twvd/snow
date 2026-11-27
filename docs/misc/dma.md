# Bus masters and DMA

A **bus master** is a device that reads and writes the machine's memory by itself, without the
CPU. An Ethernet controller does so to fetch its list of buffers and to store the frames it
receives. This is often called **DMA**, direct memory access.

This document describes how a device does DMA in Snow, and what Snow doesn't model of it. It
applies to every device that can master the bus, wherever it is: a [NuBus](nubus.md) card, a card
in a [processor-direct slot](pds.md), or a device on the logic board. The devices themselves are
described in the [device model](device-model.md).

**What Snow has today is a simplified model.** The section
[What isn't modelled](#what-isnt-modelled) lists its limits.

## The DMA operations

A device does DMA through two operations that the machine provides. One reads the guest's memory
at a given address, the other writes it. The device never accesses the machine's RAM arrays
directly, as their organisation is machine specific. This mimics a real device, which uses bus
addresses.

```rust
/// What a DMA operation did
pub struct DmaOutcome {
    /// Ticks of the clock of the device's bus, as its tick is given them, before the operation
    /// counts as complete
    pub latency: Ticks,
    /// False: it stopped at an address that isn't RAM
    pub ok: bool,
}

/// The DMA operations a machine gives a device that masters the bus. The data moves at once; the
/// latency is the time the transfer would take on the bus.
pub trait DmaOps {
    /// Copies `data.len()` bytes from the guest's RAM at `addr`
    fn read(&mut self, addr: Address, data: &mut [u8]) -> DmaOutcome;
    /// Copies `data` to the guest's RAM at `addr`
    fn write(&mut self, addr: Address, data: &[u8]) -> DmaOutcome;
}
```

The address is a CPU physical address: where the memory is in the machine's address map, as the
CPU sees it after its MMU. It isn't an address on the device's own bus (see
[What isn't modelled](#what-isnt-modelled)).

A device gets them as an argument of its tick. The slot trait of a bus whose devices can master it
has a `tick()` of its own for this, as the SCSI bus has one for its targets. For a NuBus card:

```rust
fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext, dma: &mut dyn DmaOps) -> Result<()>;
```

A card in a processor-direct slot gets them the same way, from its slot trait, `PdsSlot`
([pds.md](pds.md)). A device on the logic board has no slot trait: its machine calls its tick
directly, with the same argument.

- **They reach the machine's RAM, and nothing else**: not its ROM, and not another device (see
  [What isn't modelled](#what-isnt-modelled)).
- **A block stops at the first address that isn't RAM.** The bytes before that address are
  copied, as on real hardware, where a device does one transaction at a time; the rest isn't, and
  the operation isn't `ok`.
- **The data moves at once, and each operation returns its latency**: how long the transfer would
  take on the bus ([Latency](#latency)).
- **They are the same on every bus, but for the latency.** A NuBus card, a card in a
  processor-direct slot and a device on the logic board get the same operations from their
  machine. A NuBus card's also count the NuBus transactions.
- **A write through them shows in the debugger.** The machine counts it as a change of memory made
  by something other than the CPU, so the memory views and the memory notifiers see it.
- **They don't touch the CPU's caches.** A real 68030 doesn't see a device's writes either: the
  guest's driver flushes the caches itself.

## Only at a tick, never during a register access

A device can't master the bus from inside a read or a write of one of its registers, as a real
device can't: it has to wait for that cycle to end before it can own the bus.

So a write to a register that starts such an action only sets a flag in the device. The device's
next tick picks the flag up, and does the DMA then. A device that needs to be closer to the real
one can spread the work over several ticks, with a state machine.

**When an operation isn't `ok`**, part of the block wasn't RAM. What the device does then is its
own choice, as it is what the real device would do on a bus error: a network controller, for
example, sets an error bit in its status and raises its interrupt.

**A card with memory of its own** can implement `DmaOps` too, for a chip on the card that masters
the card's own bus: the Apple Ethernet NB Card's controller reads and writes the card's RAM, not the
Mac's. Such a card gives its chip its own `DmaOps`, and may ignore the `dma` argument of its tick.

## Latency

A DMA operation copies its data at once, and returns how many ticks it would take on the bus: its
**latency**. The ticks are those of the bus the device is on, the ones its tick is given.

The device starts no other DMA until that time has passed, and makes the end of the transfer
visible only then: the status or descriptor that says it is done, a register bit, an interrupt.
After the operations of one step that the guest can see, a received frame and its descriptors for
example, it adds up their latencies and counts them down at each tick. A device that also paces
itself by a clock of its own, as a network controller does by the speed of its wire, completes
when both times have passed.

The machine computes the latency from the timing it gives the CPU's accesses to the RAM: its
width, and its wait states for a single access. At most 32 bits move per bus cycle:

- A long word on its own costs a cycle and the wait states.
- An aligned line of 16 bytes is a burst: its first long word costs a cycle and the wait states,
  each of the next three a cycle, and one more cycle follows the burst.
- The long words of a block that aren't in a whole line cost a cycle and the wait states each.
- On a RAM with a 16-bit data bus, as the LC's and the LC II's, each long word costs two single
  accesses, and there is no burst.
- A NuBus card's DMA crosses NuBus: each long word is a NuBus transaction, with the wait states
  the machine gives the CPU's NuBus cycles, then a single access to the RAM. There is no burst.
- An operation that stops short costs what it copied.

These numbers are approximations, constants to be tuned. When the machine is overclocked, the
latency is still computed from the timing of the real hardware.

## What isn't modelled

**DMA takes time.** On real hardware, the device first asks for the bus, and waits until it is
granted. It then does one transaction at a time. Each takes a number of bus cycles, with the wait
states of the memory it accesses. The CPU is held off the bus meanwhile, while the other devices
keep running. Additionally, the way this mechanism works and the associated timings are very bus
specific.

In Snow, a device's reads and writes through `DmaOps` happen at once, inside its tick, and only
their end waits for their [latency](#latency). So:

- The data is in the RAM at the start of the latency, not spread over it. Only the end of the
  transfer is late, which is what a driver can observe.
- Each device sees a free bus: two devices that master the bus can use the same bus cycles.
- The CPU isn't held off the bus.
- The block transfers of NuBus on the Quadras aren't modelled: a NuBus card's DMA is one
  transaction per long word.

**DMA can target other devices.** On real hardware, DMA from a device can in general target
another device, and not only the machine's RAM. The rules governing this can be bus specific.
For example, a NuBus card in a Mac II can reach the I/O devices of the logic board, but Apple
says this isn't for normal use
(Designing Cards and Drivers, 3rd ed., ch. 1, "NuBus-to-processor bus state machines").

In Snow, a device can't do DMA into another device. The rules for this are complex, and the Rust
implementation would be even more so. None of the currently implemented devices needs it.

**A bus has its own addresses.** On real hardware, a device puts an address of its own bus on the
wires. It isn't always the address the CPU uses for the same thing: the controller of the bus
translates between the two. For example, a NuBus card in a Mac II reaches the RAM at the address
the CPU uses, but the ROM at `$F080_0000` to `$F0FF_FFFF`, where the CPU has it at `$4xxx_xxxx`
(same section of Designing Cards and Drivers).

In Snow, nothing is translated: a device gives `DmaOps` a CPU physical address. Since the
operations reach the machine's RAM only, the difference shows only on a bus that has the RAM
at another address than the CPU does. A device on such a bus has to translate the address itself.

**A fuller model** would turn the calls around. A device would return from its tick with a request
for the bus. The bus would grant it later, and then call back into the device for each
transaction: the device would say what kind of transaction it is, at which address, and give the
value or where to put it. The bus would carry the transaction out, with its real duration, and
tell the device when it is done. The operations would belong to the bus, and could differ from one
type of bus to the next. And since the bus would carry out each transaction itself, it could reach
whatever the address leads to, another device included.

That is a larger change, outside the scope of this design.

## Not too fast

A device that waits for the latency of its operations can't move more data than the bus would
carry. That is all the latency gives: anything else that limits the real device, the speed of a
network wire or of a disk, is the device model's own to pace. A device has to take care not to do
its work so much faster than the real device could that the guest's driver breaks. Old device
drivers have interesting races when things happen more quickly than real hardware possibly can.
