use anyhow::Result;
use num_traits::{PrimInt, WrappingAdd};

use super::Bus;
use crate::tickable::{Tickable, Ticks};

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::fmt::Debug;
use std::hash::Hash;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Access {
    Read,
    Write,
}

#[derive(Copy, Clone, Debug)]
pub struct TraceEntry<T: PrimInt + WrappingAdd> {
    pub addr: T,
    pub access: Access,
    pub val: u8,
    pub cycle: usize,
}

pub struct Testbus<T: PrimInt + WrappingAdd + Hash + Debug> {
    pub mem: HashMap<T, u8>,
    trace: RefCell<Vec<TraceEntry<T>>>,
    cycles: usize,
    trace_enabled: bool,
    mask: T,
}

impl<T> Testbus<T>
where
    T: PrimInt + WrappingAdd + Hash + Debug,
{
    pub fn new(mask: T) -> Self {
        Testbus {
            // Need allocation here, too large for stack.
            mem: HashMap::new(),
            trace: RefCell::new(vec![]),
            cycles: 0,
            trace_enabled: false,
            mask,
        }
    }

    pub fn get_seen_addresses(&self) -> impl Iterator<Item = T> + '_ {
        self.mem.keys().copied()
    }

    pub fn reset_trace(&mut self) {
        self.trace.borrow_mut().clear();
        self.trace_enabled = true;
    }

    pub fn get_trace(&self) -> Vec<TraceEntry<T>> {
        self.trace.borrow().clone()
    }
}

impl<T> Bus<T> for Testbus<T>
where
    T: PrimInt + WrappingAdd + Hash + Debug,
{
    fn get_mask(&self) -> T {
        self.mask
    }

    fn read(&self, addr: T) -> u8 {
        assert_eq!(addr & self.mask, addr);

        let val = *self.mem.get(&addr).unwrap_or(&0);
        if self.trace_enabled {
            self.trace.borrow_mut().push(TraceEntry {
                addr,
                access: Access::Read,
                val,
                cycle: self.cycles,
            });
        }
        val
    }

    fn write(&mut self, addr: T, val: u8) {
        assert_eq!(addr & self.mask, addr);

        if self.trace_enabled {
            self.trace.borrow_mut().push(TraceEntry {
                addr,
                access: Access::Write,
                val,
                cycle: self.cycles,
            });
        }
        self.mem.insert(addr, val);
    }
}

impl<T> Tickable for Testbus<T>
where
    T: PrimInt + WrappingAdd + Hash + Debug,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        self.cycles += ticks;
        Ok(ticks)
    }
}

impl<T> fmt::Display for Testbus<T>
where
    T: PrimInt + WrappingAdd + Hash + Debug,
{
    fn fmt(&self, _f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Result::Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testbus() {
        let mut b = Testbus::<u16>::new(u16::MAX);

        for a in 0..=u16::MAX {
            assert_eq!(b.read(a), 0);
        }
        for a in 0..=u16::MAX {
            b.write(a, a as u8);
        }
        for a in 0..=u16::MAX {
            assert_eq!(b.read(a), a as u8);
        }
    }

    #[test]
    fn in_mask() {
        let mut b = Testbus::<u16>::new(u8::MAX.into());

        b.write(0x10, 1);
    }

    #[test]
    #[should_panic]
    fn out_mask() {
        let mut b = Testbus::<u16>::new(u8::MAX.into());

        b.write(0x100, 1);
    }
}
