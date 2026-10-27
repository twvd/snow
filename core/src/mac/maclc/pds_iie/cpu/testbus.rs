//! Test memory bus for the JSON test suites (ported from siena).

use anyhow::Result;
use std::cell::RefCell;
use std::collections::HashMap;

use super::{Bus6502, ReadKind};
use crate::tickable::Ticks;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Access {
    Read,
    Write,
}

#[derive(Copy, Clone, Debug)]
pub struct TraceEntry {
    pub addr: u32,
    pub access: Access,
    pub val: u8,
    pub cycle: usize,
}

pub struct Testbus {
    mem: HashMap<u32, u8>,
    trace: RefCell<Vec<TraceEntry>>,
    cycles: usize,
    trace_enabled: bool,
    mask: u32,
}

impl Testbus {
    pub fn new(mask: u32) -> Self {
        Self {
            // Need allocation here, too large for stack.
            mem: HashMap::new(),
            trace: RefCell::new(vec![]),
            cycles: 0,
            trace_enabled: false,
            mask,
        }
    }

    pub fn reset_trace(&mut self) {
        self.trace.borrow_mut().clear();
        self.trace_enabled = true;
    }

    pub fn get_trace(&self) -> Vec<TraceEntry> {
        self.trace.borrow().clone()
    }
}

impl Bus6502 for Testbus {
    fn get_mask(&self) -> u32 {
        self.mask
    }

    fn read(&self, addr: u32, _kind: ReadKind) -> u8 {
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

    fn write(&mut self, addr: u32, val: u8) {
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

    fn get_nmi(&mut self) -> bool {
        false
    }
    fn get_int(&mut self) -> bool {
        false
    }

    fn tick(&mut self, cycles: Ticks) -> Result<Ticks> {
        self.cycles += cycles as usize;
        Ok(cycles)
    }
}
