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

pub trait Bus<TA: PrimInt + WrappingAdd, TD: PrimInt>: Tickable {
    fn read(&self, addr: TA) -> TD;
    fn write(&mut self, addr: TA, val: TD);
    fn get_mask(&self) -> TA;
}

impl<TA, TD> core::fmt::Debug for dyn Bus<TA, TD> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Bus")
    }
}

pub struct BusIterator<'a, TA: PrimInt + WrappingAdd, TD: PrimInt> {
    bus: &'a dyn Bus<TA, TD>,
    next: TA,
}

impl<'a, TA: PrimInt + WrappingAdd, TD: PrimInt> BusIterator<'a, TA, TD> {
    pub fn new_from(bus: &'a dyn Bus<TA, TD>, offset: TA) -> Self {
        Self { bus, next: offset }
    }

    pub fn new(bus: &'a dyn Bus<TA, TD>) -> Self {
        Self::new_from(bus, TA::zero())
    }
}

impl<'a, TA: PrimInt + WrappingAdd, TD: PrimInt> Iterator for BusIterator<'a, TA, TD> {
    type Item = TD;

    fn next(&mut self) -> Option<Self::Item> {
        let curr = self.next;
        self.next = self.next.wrapping_add(&TA::one()) & self.bus.get_mask();

        Some(self.bus.read(curr))
    }
}

#[cfg(test)]
mod tests {
    use super::testbus::Testbus;
    use super::*;

    fn testbus() -> Testbus<Address, u8> {
        let mut b = Testbus::<Address, u8>::new(ADDRESS_MASK);
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
}
