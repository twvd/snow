//! What a machine gives a device that reads and writes its memory by itself, without the CPU

use crate::bus::Address;
use crate::tickable::Ticks;

/// What a DMA operation did
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// A machine's RAM, as `read_ram()` and `write_ram()` reach it. Implemented by each machine on
/// the struct through which it gives `DmaOps` to its devices.
pub trait DmaRam {
    /// The RAM
    fn ram(&mut self) -> &mut [u8];
    /// The offset in `ram()` of the byte at physical address `addr`, if that byte is in the RAM
    fn ram_offset(&self, addr: Address) -> Option<usize>;
    /// The width in bytes of the RAM's data bus (2 or 4), and the wait states of a single access
    /// to it: those the machine gives the CPU's accesses, without overclocking
    fn ram_timing(&self, write: bool) -> (u32, Ticks);
}

/// Ticks of a bus cycle with no wait state
const DMA_CYCLE: Ticks = 1;

/// Ticks after the last long word of a burst
const DMA_BURST_END: Ticks = 1;

/// The bytes of a burst: an aligned line of four long words
const LINE: u64 = 16;

/// The long words that a block of `len` bytes at `addr` touches
fn longwords(addr: Address, len: usize) -> Ticks {
    let start = u64::from(addr) & !3;
    let end = (u64::from(addr) + len as u64 + 3) & !3;
    (end - start) / 4
}

/// The latency of a DMA operation of `len` bytes at `addr` in a machine's RAM.
///
/// At most 32 bits move per bus cycle, and a single access costs a cycle and the wait states of
/// the RAM. On a 32-bit RAM, each aligned line of 16 bytes in the block is a burst: its first long
/// word costs a single access, each of the next three a cycle, and a cycle follows it. The rest
/// of the block costs a single access per long word. On a 16-bit RAM, each long word costs two
/// single accesses. These are approximations, to be tuned.
pub fn ram_latency(dma: &impl DmaRam, addr: Address, len: usize, write: bool) -> Ticks {
    let (width, waits) = dma.ram_timing(write);
    let single = DMA_CYCLE + waits;
    if width < 4 {
        return longwords(addr, len) * Ticks::from(4 / width) * single;
    }

    let end = u64::from(addr) + len as u64;
    let mut at = u64::from(addr);
    let mut ticks = 0;
    while at < end {
        if at % LINE == 0 && end - at >= LINE {
            ticks += single + 3 * DMA_CYCLE + DMA_BURST_END;
            at += LINE;
        } else {
            ticks += single;
            at = (at & !3) + 4;
        }
    }
    ticks
}

/// The body of `DmaOps::read()` on a machine's RAM
///
/// A block is copied up to the first address that isn't in the RAM: what was copied until then
/// stays, and the operation isn't `ok`. The latency is that of what was copied.
pub fn read_ram(dma: &mut impl DmaRam, addr: Address, data: &mut [u8]) -> DmaOutcome {
    let mut copied = 0;
    for (a, byte) in (addr..).zip(data.iter_mut()) {
        let Some(idx) = dma.ram_offset(a) else {
            break;
        };
        *byte = dma.ram()[idx];
        copied += 1;
    }
    DmaOutcome {
        latency: ram_latency(dma, addr, copied, false),
        ok: copied == data.len(),
    }
}

/// The body of `DmaOps::write()` on a machine's RAM
///
/// A block is copied up to the first address that isn't in the RAM: what was copied until then
/// stays, and the operation isn't `ok`. The latency is that of what was copied.
pub fn write_ram(dma: &mut impl DmaRam, addr: Address, data: &[u8]) -> DmaOutcome {
    let mut copied = 0;
    for (a, &byte) in (addr..).zip(data) {
        let Some(idx) = dma.ram_offset(a) else {
            break;
        };
        dma.ram()[idx] = byte;
        copied += 1;
    }
    DmaOutcome {
        latency: ram_latency(dma, addr, copied, true),
        ok: copied == data.len(),
    }
}

/// The DMA operations of a NuBus card: the machine's own, where each long word is a NuBus
/// transaction that ends with a single access to the RAM, without bursts
pub struct NubusDma<'a, D> {
    /// The machine's DMA operations
    pub dma: &'a mut D,
    /// The wait states of a NuBus transaction, those the machine gives the CPU's
    pub nubus_waits: Ticks,
}

impl<D: DmaOps + DmaRam> NubusDma<'_, D> {
    fn latency(&self, addr: Address, len: usize, write: bool) -> Ticks {
        let (_, waits) = self.dma.ram_timing(write);
        longwords(addr, len) * (self.nubus_waits + DMA_CYCLE + waits)
    }
}

impl<D: DmaOps + DmaRam> DmaOps for NubusDma<'_, D> {
    fn read(&mut self, addr: Address, data: &mut [u8]) -> DmaOutcome {
        let ok = self.dma.read(addr, data).ok;
        DmaOutcome {
            latency: self.latency(addr, data.len(), false),
            ok,
        }
    }

    fn write(&mut self, addr: Address, data: &[u8]) -> DmaOutcome {
        let ok = self.dma.write(addr, data).ok;
        DmaOutcome {
            latency: self.latency(addr, data.len(), true),
            ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 bytes of RAM at address 0
    struct Ram {
        bytes: Vec<u8>,
        width: u32,
        waits: Ticks,
    }

    impl Ram {
        fn new(width: u32, waits: Ticks) -> Self {
            Self {
                bytes: vec![0; 64],
                width,
                waits,
            }
        }
    }

    impl DmaRam for Ram {
        fn ram(&mut self) -> &mut [u8] {
            &mut self.bytes
        }

        fn ram_offset(&self, addr: Address) -> Option<usize> {
            (addr < 64).then_some(addr as usize)
        }

        fn ram_timing(&self, _write: bool) -> (u32, Ticks) {
            (self.width, self.waits)
        }
    }

    impl DmaOps for Ram {
        fn read(&mut self, addr: Address, data: &mut [u8]) -> DmaOutcome {
            read_ram(self, addr, data)
        }

        fn write(&mut self, addr: Address, data: &[u8]) -> DmaOutcome {
            write_ram(self, addr, data)
        }
    }

    #[test]
    fn a_long_word_on_its_own_is_a_single_access() {
        let ram = Ram::new(4, 2);
        assert_eq!(ram_latency(&ram, 0, 4, false), 3);
        // A byte, or a long word across two long words
        assert_eq!(ram_latency(&ram, 5, 1, false), 3);
        assert_eq!(ram_latency(&ram, 2, 4, false), 6);
    }

    #[test]
    fn an_aligned_line_is_a_burst() {
        let ram = Ram::new(4, 2);
        // 3 + 1 + 1 + 1, then one cycle
        assert_eq!(ram_latency(&ram, 0, 16, false), 7);
        assert_eq!(ram_latency(&ram, 0, 32, false), 14);
        // Two partial lines: a long word before the burst, a long word after
        assert_eq!(ram_latency(&ram, 12, 24, false), 3 + 7 + 3);
        // No line is whole
        assert_eq!(ram_latency(&ram, 4, 16, false), 4 * 3);
    }

    #[test]
    fn a_16_bit_ram_takes_two_accesses_per_long_word() {
        let ram = Ram::new(2, 1);
        assert_eq!(ram_latency(&ram, 0, 4, false), 4);
        assert_eq!(ram_latency(&ram, 0, 16, false), 16);
    }

    #[test]
    fn an_operation_that_stops_short_costs_what_it_copied() {
        let mut ram = Ram::new(4, 2);
        let mut block = [0; 16];
        assert_eq!(
            read_ram(&mut ram, 56, &mut block),
            DmaOutcome {
                latency: 2 * 3,
                ok: false
            }
        );
        assert_eq!(
            write_ram(&mut ram, 0, &block),
            DmaOutcome {
                latency: 7,
                ok: true
            }
        );
    }

    #[test]
    fn a_nubus_card_pays_a_transaction_per_long_word() {
        let mut ram = Ram::new(4, 2);
        let mut nubus = NubusDma {
            dma: &mut ram,
            nubus_waits: 4,
        };
        let mut block = [0; 16];
        assert_eq!(
            nubus.read(0, &mut block),
            DmaOutcome {
                latency: 4 * (4 + 3),
                ok: true
            }
        );
    }
}
