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
pub struct TraceEntry<TA: PrimInt + WrappingAdd, TD: PrimInt> {
    pub addr: TA,
    pub access: Access,
    pub val: TD,
    pub cycle: usize,
}

pub struct Testbus<TA: PrimInt + WrappingAdd + Hash + Debug, TD: PrimInt> {
    pub mem: HashMap<TA, TD>,
    trace: RefCell<Vec<TraceEntry<TA, TD>>>,
    cycles: usize,
    trace_enabled: bool,
    mask: TA,
}

impl<TA, TD> Testbus<TA, TD>
where
    TA: PrimInt + WrappingAdd + Hash + Debug,
    TD: PrimInt,
{
    pub fn new(mask: TA) -> Self {
        Testbus {
            mem: HashMap::new(),
            trace: RefCell::new(vec![]),
            cycles: 0,
            trace_enabled: false,
            mask,
        }
    }

    pub fn get_seen_addresses(&self) -> impl Iterator<Item = TA> + '_ {
        self.mem.keys().copied()
    }

    pub fn reset_trace(&mut self) {
        self.trace.borrow_mut().clear();
        self.trace_enabled = true;
    }

    pub fn get_trace(&self) -> Vec<TraceEntry<TA, TD>> {
        self.trace.borrow().clone()
    }
}

impl<TA, TD> Bus<TA, TD> for Testbus<TA, TD>
where
    TA: PrimInt + WrappingAdd + Hash + Debug,
    TD: PrimInt,
{
    fn get_mask(&self) -> TA {
        self.mask
    }

    fn read(&self, addr: TA) -> TD {
        assert_eq!(addr & self.mask, addr);

        let val = *self.mem.get(&addr).unwrap_or(&TD::zero());
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

    fn write(&mut self, addr: TA, val: TD) {
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

impl<TA, TD> Tickable for Testbus<TA, TD>
where
    TA: PrimInt + WrappingAdd + Hash + Debug,
    TD: PrimInt,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        self.cycles += ticks;
        Ok(ticks)
    }
}

impl<TA, TD> fmt::Display for Testbus<TA, TD>
where
    TA: PrimInt + WrappingAdd + Hash + Debug,
    TD: PrimInt,
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
        let mut b = Testbus::<u16, u8>::new(u16::MAX);

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
        let mut b = Testbus::<u16, u8>::new(u8::MAX.into());

        b.write(0x10, 1);
    }

    #[test]
    #[should_panic]
    fn out_mask() {
        let mut b = Testbus::<u16, u8>::new(u8::MAX.into());

        b.write(0x100, 1);
    }
}
