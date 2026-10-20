//! Bus accesses
//!
//! Models a 68k bus cycle: the CPU presents an address and the number of
//! bytes left in the operand (SIZ0/SIZ1), the addressed port terminates the
//! cycle with its width (DSACK0/DSACK1) and takes as many bytes as fit in it.
//! The 68020 and up run further cycles for the rest of the operand, the 68000
//! has a fixed 16-bit bus.
//!
//! Data is passed right-aligned rather than on the byte lanes it occupies on
//! the real data bus.

use std::marker::PhantomData;

use crate::bus::Address;
use crate::tickable::Ticks;

/// Data bus of the CPU a bus is attached to
pub trait CpuBus: Copy {
    /// Data bus width in bytes
    const WIDTH: u32;
    /// Dynamic bus sizing
    const DYN_SIZING: bool;
    /// CPU ticks of a bus cycle without wait states
    const CYCLE: Ticks;
}

/// 68000: fixed 16-bit data bus
#[derive(Clone, Copy, Debug)]
pub struct Bus68000;

impl CpuBus for Bus68000 {
    const WIDTH: u32 = 2;
    const DYN_SIZING: bool = false;
    const CYCLE: Ticks = 4;
}

/// 68020 and 68030: 32-bit data bus with dynamic bus sizing
#[derive(Clone, Copy, Debug)]
pub struct Bus68020;

impl CpuBus for Bus68020 {
    const WIDTH: u32 = 4;
    const DYN_SIZING: bool = true;
    // The asynchronous bus cycle of the 68020 and 68030
    const CYCLE: Ticks = 3;
}

/// One bus cycle as presented by the CPU, on the data bus `C` of the CPU
#[derive(Clone, Copy, Debug)]
pub struct BusAccess<C> {
    /// Address of the first byte
    pub addr: Address,
    /// Bytes left in the operand (1-4)
    pub size: u32,
    /// Write data, right-aligned to `size`
    pub data: u32,
    pub write: bool,
    // Function code, currently unused
    // pub fc: u8,
    /// CPU cycles elapsed since the bus was last ticked
    pub pending: Ticks,
    /// Label with CPU type for compile time checking
    _cpu: PhantomData<C>,
}

impl<C: CpuBus> BusAccess<C> {
    pub const fn read(
        addr: Address,
        size: u32,
        // fc: u8,
        pending: Ticks,
    ) -> Self {
        Self {
            addr,
            size,
            data: 0,
            write: false,
            // fc,
            pending,
            _cpu: PhantomData,
        }
    }

    pub const fn write(
        addr: Address,
        size: u32,
        data: u32,
        // fc: u8,
        pending: Ticks,
    ) -> Self {
        Self {
            addr,
            size,
            data,
            write: true,
            // fc,
            pending,
            _cpu: PhantomData,
        }
    }
}

/// Termination of a bus cycle
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum BusResponse {
    /// DSACK
    Ack {
        /// Port width in bytes
        width: u32,
        /// Read data, right-aligned to the bytes the port took
        data: u32,
        /// Wait states of the cycle, in CPU cycles
        waits: Ticks,
    },
    /// The port can't take the cycle yet: the CPU waits this many cycles,
    /// with the bus ticked through them, then presents the cycle again
    Stall(Ticks),
    /// BERR
    BusError,
}

/// Mask for the low `bytes` bytes
#[inline(always)]
const fn mask(bytes: u32) -> u32 {
    if bytes >= 4 {
        u32::MAX
    } else {
        (1 << (bytes * 8)) - 1
    }
}

/// What the CPU drives on the lanes of its data bus during a write, as
/// operand bytes, so that the bytes are right whatever the width of the port
/// turns out to be: an 8-bit port is on D31-D24, a 16-bit one on D31-D16.
/// With OP0-OP3 the bytes of a long word operand, the last ones for a
/// shorter transfer, the 68020 drives (MC68020 User's Manual, 1984, table
/// 5-3):
///
/// | Size   | A1 A0 | D31-D24 | D23-D16 | D15-D8 | D7-D0 |
/// |--------|-------|---------|---------|--------|-------|
/// | Byte   | x x   | OP3     | OP3     | OP3    | OP3   |
/// | Word   | x 0   | OP2     | OP3     | OP2    | OP3   |
/// | Word   | x 1   | OP2     | OP2     | OP3    | OP2   |
/// | 3 byte | 0 0   | OP1     | OP2     | OP3    | OP1   |
/// | 3 byte | 0 1   | OP1     | OP1     | OP2    | OP3   |
/// | 3 byte | 1 0   | OP1     | OP2     | OP1    | OP2   |
/// | 3 byte | 1 1   | OP1     | OP1     | OP1    | OP1   |
/// | Long   | 0 0   | OP0     | OP1     | OP2    | OP3   |
/// | Long   | 0 1   | OP0     | OP0     | OP1    | OP2   |
/// | Long   | 1 0   | OP0     | OP1     | OP0    | OP1   |
/// | Long   | 1 1   | OP0     | OP0     | OP1    | OP0   |
///
/// The 68000 drives a byte on both halves of its 16-bit data bus and a word
/// as it is, which are the first two lanes of the byte and word rows.
///
/// Indexed by the size minus one and A1 A0, each lane gives the operand byte
/// counted from the first of the transfer.
const WRITE_LANES: [[[u32; 4]; 4]; 4] = [
    [[0, 0, 0, 0]; 4],
    [[0, 1, 0, 1], [0, 0, 1, 0], [0, 1, 0, 1], [0, 0, 1, 0]],
    [[0, 1, 2, 0], [0, 0, 1, 2], [0, 1, 0, 1], [0, 0, 0, 0]],
    [[0, 1, 2, 3], [0, 0, 1, 2], [0, 1, 0, 1], [0, 0, 1, 0]],
];

/// The lanes of the data bus during a write of the `size` bytes left of an
/// operand, right-aligned in `data`, at `addr`: D31-D24 in the top byte
#[inline(always)]
fn write_lanes(data: u32, size: u32, addr: Address) -> u32 {
    WRITE_LANES[size as usize - 1][(addr % 4) as usize]
        .iter()
        .fold(0, |lanes, &op| {
            (lanes << 8) | ((data >> (8 * (size - 1 - op))) & 0xFF)
        })
}

// ------------------------------------------------------------------------
// --------------------- Helpers for use by bus models --------------------
// ------------------------------------------------------------------------

/// bus_access() is the "base" helper. It's rarely used directly (if at all),
/// common-case wrappers are provided below.
///
/// It runs a bus cycle against a port of `BUS_W` bytes wide, in which a device
/// of `DEV_W` bytes wide sits at address offset `OFFSET` within the port
/// (ie device first lane within the port).
///
/// The helper takes two callbacks: The device read() and write().
///
/// Those callbacks get the address of the first device byte accessed, the
/// number of bytes, and take or return the data right-aligned.
///
/// A device whose bytes are outside of the transfer is still accessed, as its
/// select line only comes from the address decode: reads are discarded, writes
/// get the data the CPU replicates on its lanes. Lanes not driven by the device
/// read as all ones.
#[inline(always)]
pub fn bus_access<const BUS_W: u32, const DEV_W: u32, const OFFSET: u32, C: CpuBus, D>(
    acc: BusAccess<C>,
    waits: Ticks,
    dev: &mut D,
    read: impl FnOnce(&mut D, Address, u32) -> Option<u32>,
    write: impl FnOnce(&mut D, Address, u32, u32) -> Option<()>,
) -> BusResponse {
    // Compile-time checking that things are sane, the port fits in the
    // data bus, the device fits the port, and the sizes are supported.
    const {
        assert!(BUS_W == 1 || BUS_W == 2 || BUS_W == 4);
        assert!(DEV_W >= 1 && OFFSET + DEV_W <= BUS_W);
        assert!(if C::DYN_SIZING {
            BUS_W <= C::WIDTH
        } else {
            BUS_W == C::WIDTH
        });
    }

    // A bus without dynamic sizing (68000) never splits an operand, so a
    // device as wide as the bus gets the whole of it as it is. Resolved at
    // compile time.
    if !C::DYN_SIZING && DEV_W == BUS_W {
        if acc.write {
            if write(dev, acc.addr, acc.size, acc.data).is_none() {
                return BusResponse::BusError;
            }
            return BusResponse::Ack {
                width: BUS_W,
                data: 0,
                waits,
            };
        }
        let Some(val) = read(dev, acc.addr, acc.size) else {
            return BusResponse::BusError;
        };
        return BusResponse::Ack {
            width: BUS_W,
            data: val & mask(acc.size),
            waits,
        };
    }

    // Figure out the lanes...
    //
    // start is the first lane of the transfer (0 or 1 on a 16-bit bus for ex):
    let start = acc.addr % BUS_W;
    // len is the number of lanes of the transfer (the min() of the bus lanes left
    // after start, and the bytes left in the operand).
    let len = (BUS_W - start).min(acc.size);
    // Base is the address of lane 0 of the bus for that access
    let base = acc.addr - start;
    // First lane of the transfer that hits the device
    let lo = start.max(OFFSET);
    // One past the last lane of the transfer that hits the device
    // (lo..hi is empty if the transfer misses)
    let hi = (start + len).min(OFFSET + DEV_W);

    // Fast-path for the common case (resolved partially at compile time): A
    // device filling its port completely or a transfer that hits the device
    // port skips replication.
    // This diverges from real HW in that real HW always replicates lanes on
    // writes that are smaller than the bus, this instead assumes that for a
    // transfer that "hits" the device, the device models will honor the byte
    // enables we pass, so we can skip the replication.
    // This avoids hitting write_lanes() in the most common cases.
    //
    // Example of models that will actually hit the replication path when
    // converted is the SCC on Compact.
    let in_transfer = DEV_W == BUS_W || lo < hi;

    if acc.write {
        let (lo, hi, val) = if in_transfer {
            // Fast path, just right align the data
            let data = (acc.data >> (8 * (acc.size - len))) & mask(len);
            (lo, hi, (data >> (8 * (start + len - hi))) & mask(hi - lo))
        } else {
            // Slow path, the device relies on lane replication
            let lanes = write_lanes(acc.data, acc.size, acc.addr) >> (8 * (4 - BUS_W));
            (
                OFFSET,
                OFFSET + DEV_W,
                (lanes >> (8 * (BUS_W - OFFSET - DEV_W))) & mask(DEV_W),
            )
        };

        // Hey device, take it !
        if write(dev, base + lo, hi - lo, val).is_none() {
            return BusResponse::BusError;
        }
        BusResponse::Ack {
            width: BUS_W,
            data: 0,
            waits,
        }
    } else {
        // We hit the device even if it's not "in_transfer" because while
        // we might ignore the returned data, the read can have side effects
        // device-side.
        let (lo, hi) = if in_transfer {
            (lo, hi)
        } else {
            (OFFSET, OFFSET + DEV_W)
        };

        // Hey device, give it !
        let Some(val) = read(dev, base + lo, hi - lo) else {
            return BusResponse::BusError;
        };
        // Device bytes on their lanes, the other lanes of the port floating
        let shift = 8 * (BUS_W - hi);
        let lanes = (mask(BUS_W) & !(mask(hi - lo) << shift)) | ((val & mask(hi - lo)) << shift);
        BusResponse::Ack {
            width: BUS_W,
            // Bytes of the transfer, right-aligned
            data: (lanes >> (8 * (BUS_W - start - len))) & mask(len),
            waits,
        }
    }
}

// ------- Here are the "simplified" helpers ------

/// 32-bit device on a 32-bit port
///
/// Not available on a 68000 bus:
/// ```compile_fail
/// use snow_core::bus::access::*;
/// let mut v = 0u32;
/// let _ = bus_access_long(
///     BusAccess::<Bus68000>::read(0, 2, 0),
///     0,
///     &mut v,
///     |_, _, _| Some(0),
///     |_, _, _, _| Some(()),
/// );
/// ```
#[inline(always)]
pub fn bus_access_long<C: CpuBus, D>(
    acc: BusAccess<C>,
    waits: Ticks,
    dev: &mut D,
    read: impl FnOnce(&mut D, Address, u32) -> Option<u32>,
    write: impl FnOnce(&mut D, Address, u32, u32) -> Option<()>,
) -> BusResponse {
    bus_access::<4, 4, 0, C, D>(acc, waits, dev, read, write)
}

/// 16-bit device on a 16-bit port
#[inline(always)]
pub fn bus_access_word<C: CpuBus, D>(
    acc: BusAccess<C>,
    waits: Ticks,
    dev: &mut D,
    read: impl FnOnce(&mut D, Address, u32) -> Option<u32>,
    write: impl FnOnce(&mut D, Address, u32, u32) -> Option<()>,
) -> BusResponse {
    bus_access::<2, 2, 0, C, D>(acc, waits, dev, read, write)
}

/// 8-bit device on an 8-bit port
#[inline(always)]
pub fn bus_access_byte<C: CpuBus, D>(
    acc: BusAccess<C>,
    waits: Ticks,
    dev: &mut D,
    read: impl FnOnce(&mut D, Address, u32) -> Option<u32>,
    write: impl FnOnce(&mut D, Address, u32, u32) -> Option<()>,
) -> BusResponse {
    bus_access::<1, 1, 0, C, D>(acc, waits, dev, read, write)
}

/// 16-bit device at address offset `OFFSET` of a `BUS_W` port
#[inline(always)]
pub fn bus_access_word_lane<const BUS_W: u32, const OFFSET: u32, C: CpuBus, D>(
    acc: BusAccess<C>,
    waits: Ticks,
    dev: &mut D,
    read: impl FnOnce(&mut D, Address, u32) -> Option<u32>,
    write: impl FnOnce(&mut D, Address, u32, u32) -> Option<()>,
) -> BusResponse {
    bus_access::<BUS_W, 2, OFFSET, C, D>(acc, waits, dev, read, write)
}

/// 8-bit device at address offset `OFFSET` of a `BUS_W` port
#[inline(always)]
pub fn bus_access_byte_lane<const BUS_W: u32, const OFFSET: u32, C: CpuBus, D>(
    acc: BusAccess<C>,
    waits: Ticks,
    dev: &mut D,
    read: impl FnOnce(&mut D, Address, u32) -> Option<u32>,
    write: impl FnOnce(&mut D, Address, u32, u32) -> Option<()>,
) -> BusResponse {
    bus_access::<BUS_W, 1, OFFSET, C, D>(acc, waits, dev, read, write)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-addressed memory, logs the device accesses
    struct Mem {
        bytes: Vec<u8>,
        log: Vec<(Address, u32)>,
    }

    impl Mem {
        fn new() -> Self {
            Self {
                bytes: (0..16).map(|i| 0x10 + i).collect(),
                log: vec![],
            }
        }

        fn read(&mut self, addr: Address, len: u32) -> Option<u32> {
            self.log.push((addr, len));
            Some((0..len).fold(0, |v, i| {
                (v << 8) | u32::from(self.bytes[(addr + i) as usize])
            }))
        }

        fn write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
            self.log.push((addr, len));
            for i in 0..len {
                self.bytes[(addr + i) as usize] = (val >> (8 * (len - 1 - i))) as u8;
            }
            Some(())
        }
    }

    fn rd<C: CpuBus>(addr: Address, size: u32) -> BusAccess<C> {
        BusAccess::read(addr, size, 0)
    }

    fn wr<C: CpuBus>(addr: Address, size: u32, data: u32) -> BusAccess<C> {
        BusAccess::write(addr, size, data, 0)
    }

    fn ack(resp: BusResponse) -> (u32, u32) {
        match resp {
            BusResponse::Ack { width, data, .. } => (width, data),
            BusResponse::Stall(_) => panic!("stall"),
            BusResponse::BusError => panic!("bus error"),
        }
    }

    #[test]
    fn long_port() {
        let mut m = Mem::new();
        let r = bus_access_long(rd::<Bus68020>(4, 4), 0, &mut m, Mem::read, Mem::write);
        assert_eq!(ack(r), (4, 0x1415_1617));

        // Misaligned long takes the 3 bytes up to the end of the port
        let r = bus_access_long(rd::<Bus68020>(5, 4), 0, &mut m, Mem::read, Mem::write);
        assert_eq!(ack(r), (4, 0x15_1617));
        assert_eq!(m.log[1], (5, 3));

        // Byte
        let r = bus_access_long(rd::<Bus68020>(6, 1), 0, &mut m, Mem::read, Mem::write);
        assert_eq!(ack(r), (4, 0x16));
        assert_eq!(m.log[2], (6, 1));
    }

    #[test]
    fn word_port_dynamic_sizing() {
        let mut m = Mem::new();
        // Long on a 16-bit port: the port takes the upper word
        let r = bus_access_word(rd::<Bus68020>(8, 4), 0, &mut m, Mem::read, Mem::write);
        assert_eq!(ack(r), (2, 0x1819));

        let r = bus_access_word(
            wr::<Bus68020>(8, 4, 0xAABB_CCDD),
            0,
            &mut m,
            Mem::read,
            Mem::write,
        );
        assert_eq!(ack(r).0, 2);
        assert_eq!(&m.bytes[8..12], &[0xAA, 0xBB, 0x1A, 0x1B]);
    }

    #[test]
    fn byte_port() {
        let mut m = Mem::new();
        let r = bus_access_byte(
            wr::<Bus68020>(3, 4, 0xAABB_CCDD),
            0,
            &mut m,
            Mem::read,
            Mem::write,
        );
        assert_eq!(ack(r).0, 1);
        assert_eq!(&m.bytes[3..5], &[0xAA, 0x14]);
    }

    #[test]
    fn byte_lane_read() {
        // Word read of a byte device on the upper lane of a 68000 bus
        let mut m = Mem::new();
        let r = bus_access_byte_lane::<2, 0, _, _>(
            rd::<Bus68000>(4, 2),
            0,
            &mut m,
            Mem::read,
            Mem::write,
        );
        assert_eq!(ack(r), (2, 0x14FF));
        assert_eq!(m.log, vec![(4, 1)]);

        // Lower lane
        let r = bus_access_byte_lane::<2, 1, _, _>(
            rd::<Bus68000>(4, 2),
            0,
            &mut m,
            Mem::read,
            Mem::write,
        );
        assert_eq!(ack(r), (2, 0xFF15));

        // Byte read of the other lane still selects the device
        let r = bus_access_byte_lane::<2, 0, _, _>(
            rd::<Bus68000>(5, 1),
            0,
            &mut m,
            Mem::read,
            Mem::write,
        );
        assert_eq!(ack(r), (2, 0xFF));
        assert_eq!(m.log[2], (4, 1));
    }

    #[test]
    fn byte_lane_write_replicated() {
        // Byte write to the odd address of a device on the upper lane
        let mut m = Mem::new();
        let r = bus_access_byte_lane::<2, 0, _, _>(
            wr::<Bus68000>(5, 1, 0xAB),
            0,
            &mut m,
            Mem::read,
            Mem::write,
        );
        assert_eq!(ack(r).0, 2);
        assert_eq!(m.log, vec![(4, 1)]);
        assert_eq!(&m.bytes[4..6], &[0xAB, 0x15]);

        // Byte device on the lowest lane of a 32-bit port
        let r = bus_access_byte_lane::<4, 3, _, _>(
            wr::<Bus68020>(8, 1, 0xCD),
            0,
            &mut m,
            Mem::read,
            Mem::write,
        );
        assert_eq!(ack(r).0, 4);
        assert_eq!(m.bytes[11], 0xCD);
    }

    #[test]
    fn write_lanes_68020() {
        // Byte device on the lowest lane of a 32-bit port, outside of the
        // transfer: it gets what the 68020 drives there (MC68020 UM table 5-3)
        let lane3 = |acc| {
            let mut m = Mem::new();
            let r = bus_access_byte_lane::<4, 3, _, _>(acc, 0, &mut m, Mem::read, Mem::write);
            assert_eq!(ack(r).0, 4);
            assert_eq!(m.log, vec![(3, 1)]);
            m.bytes[3]
        };
        // Word at offset 1: OP2 OP2 OP3 OP2
        assert_eq!(lane3(wr::<Bus68020>(1, 2, 0x1234)), 0x12);
        // 3 bytes at offset 0: OP1 OP2 OP3 OP1
        assert_eq!(lane3(wr::<Bus68020>(0, 3, 0xAABBCC)), 0xAA);

        // On the third lane, long at offset 3: OP0 OP0 OP1 OP0
        let mut m = Mem::new();
        let r = bus_access_byte_lane::<4, 2, _, _>(
            wr::<Bus68020>(3, 4, 0x1122_3344),
            0,
            &mut m,
            Mem::read,
            Mem::write,
        );
        assert_eq!(ack(r).0, 4);
        assert_eq!(m.bytes[2], 0x22);
    }

    #[test]
    fn word_lane() {
        // 16-bit device on the lower half of a 32-bit port
        let mut m = Mem::new();
        let r = bus_access_word_lane::<4, 2, _, _>(
            rd::<Bus68020>(4, 4),
            0,
            &mut m,
            Mem::read,
            Mem::write,
        );
        assert_eq!(ack(r), (4, 0xFFFF_1617));
        assert_eq!(m.log, vec![(6, 2)]);
    }

    #[test]
    fn bus_error() {
        let mut m = Mem::new();
        let r = bus_access_byte(rd::<Bus68020>(3, 1), 0, &mut m, |_, _, _| None, Mem::write);
        assert_eq!(r, BusResponse::BusError);
    }
}
