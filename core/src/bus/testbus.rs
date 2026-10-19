use anyhow::Result;
use num_traits::{PrimInt, WrappingAdd};

use super::access::CpuBus;
use super::{Address, Bus, IrqSource};
use crate::bus::access::{BusAccess, BusResponse};
use crate::tickable::{Tickable, Ticks};

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::RangeInclusive;

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
    pub cpu_cycle: Ticks,
}

/// Test bus for a CPU with the data bus `C`
pub struct Testbus<TA: PrimInt + WrappingAdd + Hash + Debug, TD: PrimInt, C> {
    pub mem: HashMap<TA, TD>,
    trace: RefCell<Vec<TraceEntry<TA, TD>>>,
    bus_cycles: Ticks,
    cpu_cycles: Ticks,
    trace_enabled: bool,
    mask: TA,
    pub irq_level: Option<u8>,
    _cpu: PhantomData<C>,
    /// Port widths for sized accesses, the CPU's data bus width elsewhere
    pub port_widths: Vec<(RangeInclusive<Address>, u32)>,
    /// Ranges that answer with a bus error
    pub bus_errors: Vec<RangeInclusive<Address>>,
}

impl<TA, TD, C> Testbus<TA, TD, C>
where
    TA: PrimInt + WrappingAdd + Hash + Debug,
    TD: PrimInt,
{
    pub fn new(mask: TA) -> Self {
        Self {
            mem: HashMap::new(),
            trace: RefCell::new(vec![]),
            bus_cycles: 0,
            cpu_cycles: 0,
            trace_enabled: false,
            mask,
            irq_level: None,
            _cpu: PhantomData,
            port_widths: vec![],
            bus_errors: vec![],
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

    pub fn read(&mut self, addr: TA) -> TD {
        assert_eq!(addr & self.mask, addr);

        #[allow(clippy::or_fun_call)]
        let val = *self.mem.get(&addr).unwrap_or(&TD::zero());

        if self.trace_enabled {
            self.trace.borrow_mut().push(TraceEntry {
                addr,
                access: Access::Read,
                val,
                cpu_cycle: self.cpu_cycles,
            });
        }
        val
    }

    pub fn write(&mut self, addr: TA, val: TD) {
        assert_eq!(addr & self.mask, addr);

        if self.trace_enabled {
            self.trace.borrow_mut().push(TraceEntry {
                addr,
                access: Access::Write,
                val,
                cpu_cycle: self.cpu_cycles,
            });
        }
        self.mem.insert(addr, val);
    }
}

impl<TA, TD, C: CpuBus> Bus<TA, TD> for Testbus<TA, TD, C>
where
    TA: PrimInt + WrappingAdd + Hash + Debug,
    TD: PrimInt,
{
    type Cpu = C;

    fn get_mask(&self) -> TA {
        self.mask
    }

    fn access(&mut self, acc: BusAccess<C>) -> Result<BusResponse> {
        if self.bus_errors.iter().any(|r| r.contains(&acc.addr)) {
            return Ok(BusResponse::BusError);
        }
        let width = self
            .port_widths
            .iter()
            .find(|(r, _)| r.contains(&acc.addr))
            .map_or(C::WIDTH, |&(_, w)| w);
        let len = (width - acc.addr % width).min(acc.size);
        let mut data = 0;
        for i in 0..len {
            let addr = TA::from(acc.addr.wrapping_add(i)).unwrap();
            if acc.write {
                let val = TD::from((acc.data >> (8 * (acc.size - 1 - i))) & 0xFF).unwrap();
                self.write(addr, val);
            } else {
                let val = self.read(addr);
                data = (data << 8) | val.to_u32().unwrap();
            }
        }
        Ok(BusResponse::ack(width, data, 0))
    }

    fn reset(&mut self, _hard: bool) -> Result<bool> {
        Ok(false)
    }

    fn cpu_tick(&mut self, ticks: Ticks) -> Result<()> {
        self.cpu_cycles += ticks;
        Ok(())
    }
}

impl<TA, TD, C: CpuBus> Tickable for Testbus<TA, TD, C>
where
    TA: PrimInt + WrappingAdd + Hash + Debug,
    TD: PrimInt,
{
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        self.bus_cycles += ticks;
        Ok(ticks)
    }
}

impl<TA, TD, C: CpuBus> IrqSource for Testbus<TA, TD, C>
where
    TA: PrimInt + WrappingAdd + Hash + Debug,
    TD: PrimInt,
{
    fn get_irq(&mut self) -> Option<u8> {
        self.irq_level
    }
}

impl<TA, TD, C> fmt::Display for Testbus<TA, TD, C>
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
    use crate::bus::access::Bus68000;

    #[test]
    fn testbus() {
        let mut b = Testbus::<u16, u8, Bus68000>::new(u16::MAX);

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
        let mut b = Testbus::<u16, u8, Bus68000>::new(u8::MAX.into());

        b.write(0x10, 1);
    }

    #[test]
    #[should_panic]
    fn out_mask() {
        let mut b = Testbus::<u16, u8, Bus68000>::new(u8::MAX.into());

        b.write(0x100, 1);
    }
}
