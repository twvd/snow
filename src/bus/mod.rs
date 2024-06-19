pub mod testbus;

use crate::tickable::Tickable;

use num_traits::{PrimInt, WrappingAdd};

/// Main CPU address data type (actually 24-bit)
pub type Address = u32;

/// Main CPU address mask
pub const ADDRESS_MASK: Address = 0x00FFFFFF;

/// Main CPU total address space
pub const ADDRESS_SPACE_SIZE: usize = 16 * 1024 * 1024;
pub const ADDRESS_SPACE: u32 = 16 * 1024 * 1024;

pub trait BusMember<T: PrimInt> {
    fn read(&self, addr: T) -> Option<u8>;
    fn write(&mut self, addr: T, val: u8) -> Option<()>;
}

pub trait Bus<T: PrimInt + WrappingAdd>: Tickable {
    fn read(&self, addr: T) -> u8;
    fn write(&mut self, addr: T, val: u8);
    fn get_mask(&self) -> T;

    /// Write 16-bits to addr + 1 and addr (specific access order),
    /// in little endian.
    fn write16(&mut self, addr: T, val: u16) {
        self.write(addr.wrapping_add(&T::one()), (val >> 8) as u8);
        self.write(addr, val as u8);
    }

    /// Write 16-bits to addr and addr + 1 (specific access order),
    /// in little endian.
    /// This access order is inverted, for operations that
    /// require that..
    fn write16_acc_low(&mut self, addr: T, val: u16) {
        self.write(addr, val as u8);
        self.write(addr.wrapping_add(&T::one()), (val >> 8) as u8);
    }

    /// Read 16-bits from addr and addr + 1,
    /// from little endian.
    fn read16(&self, addr: T) -> u16 {
        let l = self.read(addr);
        let h = self.read(addr.wrapping_add(&T::one()));
        l as u16 | (h as u16) << 8
    }
}

impl<T> core::fmt::Debug for dyn Bus<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Bus")
    }
}

pub struct BusIterator<'a, T: PrimInt + WrappingAdd> {
    bus: &'a dyn Bus<T>,
    next: T,
}

impl<'a, T: PrimInt + WrappingAdd> BusIterator<'a, T> {
    pub fn new_from(bus: &'a dyn Bus<T>, offset: T) -> Self {
        Self { bus, next: offset }
    }

    pub fn new(bus: &'a dyn Bus<T>) -> Self {
        Self::new_from(bus, T::zero())
    }
}

impl<'a, T: PrimInt + WrappingAdd> Iterator for BusIterator<'a, T> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        let curr = self.next;
        self.next = self.next.wrapping_add(&T::one()) & self.bus.get_mask();

        Some(self.bus.read(curr))
    }
}

#[cfg(test)]
mod tests {
    use super::testbus::Testbus;
    use super::*;

    fn testbus() -> Testbus<Address> {
        let mut b = Testbus::<Address>::new(ADDRESS_MASK);
        for a in 0..ADDRESS_SPACE {
            b.write(a, a as u8);
        }
        b
    }

    #[test]
    fn busiterator_new() {
        let b = testbus();
        let mut i = BusIterator::new(&b);

        for a in 0..=ADDRESS_MASK {
            assert_eq!(i.next(), Some(a as u8));
        }
        // Should wrap around at the end
        assert_eq!(i.next(), Some(0));
    }

    #[test]
    fn busiterator_new_from() {
        let b = testbus();
        let mut i = BusIterator::new_from(&b, 5);

        for a in 5..=ADDRESS_MASK {
            assert_eq!(i.next(), Some(a as u8));
        }
        // Should wrap around at the end
        assert_eq!(i.next(), Some(0));
    }

    #[test]
    fn bus_write16() {
        let mut b = testbus();
        b.write16(0x1000, 0x55AA);
        assert_eq!(b.read(0x1000), 0xAA);
        assert_eq!(b.read(0x1001), 0x55);
    }

    #[test]
    fn bus_read16() {
        let mut b = testbus();
        b.write(0x1000, 0xAA);
        b.write(0x1001, 0x55);
        assert_eq!(b.read16(0x1000), 0x55AA);
    }
}
