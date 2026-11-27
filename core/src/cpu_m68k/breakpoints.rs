use super::cpu::{Breakpoint, BusBreakpoint};
use crate::bus::Address;
use serde::{Deserialize, Serialize, Serializer};

#[derive(Default, Deserialize)]
#[serde(from = "Vec<Breakpoint>")]
pub(super) struct Breakpoints {
    items: Vec<Breakpoint>,
    pc: BreakpointBounds,
    reads: BreakpointBounds,
    writes: BreakpointBounds,
    step_out: bool,
}

impl Breakpoints {
    pub(super) fn may_match_pc(&self, pc: Address) -> bool {
        self.pc.overlaps(pc, 1)
    }

    pub(super) fn has_reads(&self) -> bool {
        self.reads.0.is_some()
    }

    pub(super) fn may_match_data_access(&self, write: bool, addr: Address, size: u32) -> bool {
        let bounds = if write { &self.writes } else { &self.reads };
        bounds.overlaps(addr, size)
    }

    pub(super) fn has_step_out(&self) -> bool {
        self.step_out
    }

    pub(super) fn push(&mut self, bp: Breakpoint) {
        self.items.push(bp);
        self.index(bp);
    }

    pub(super) fn retain(&mut self, keep: impl FnMut(&Breakpoint) -> bool) {
        let len = self.items.len();
        self.items.retain(keep);
        if self.items.len() != len {
            *self = std::mem::take(&mut self.items).into();
        }
    }

    fn index(&mut self, bp: Breakpoint) {
        match bp {
            Breakpoint::Execution(addr) | Breakpoint::StepOver(addr) => self.pc.include(addr),
            Breakpoint::Bus(BusBreakpoint::Read, addr) => self.reads.include(addr),
            Breakpoint::Bus(BusBreakpoint::Write, addr) => self.writes.include(addr),
            Breakpoint::Bus(BusBreakpoint::ReadWrite, addr) => {
                self.reads.include(addr);
                self.writes.include(addr);
            }
            Breakpoint::StepOut(_) => self.step_out = true,
            _ => {}
        }
    }
}

impl std::ops::Deref for Breakpoints {
    type Target = [Breakpoint];
    fn deref(&self) -> &[Breakpoint] {
        &self.items
    }
}

// Keep the saved-state representation as the original list. Rebuild the
// bounds when loading a state, just as when replacing the list at runtime.
impl Serialize for Breakpoints {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.items.serialize(serializer)
    }
}

impl From<Vec<Breakpoint>> for Breakpoints {
    fn from(items: Vec<Breakpoint>) -> Self {
        let mut result = Self::default();
        for &bp in &items {
            result.index(bp);
        }
        result.items = items;
        result
    }
}

/// Bounds for the common negative tests in the instruction and memory paths.
/// An access inside the bounds still has to match an individual breakpoint.
#[derive(Default)]
struct BreakpointBounds(Option<(Address, Address)>);

impl BreakpointBounds {
    fn include(&mut self, addr: Address) {
        let (first, last) = self.0.get_or_insert((addr, addr));
        *first = (*first).min(addr);
        *last = (*last).max(addr);
    }

    fn overlaps(&self, addr: Address, size: u32) -> bool {
        self.0
            .is_some_and(|(first, last)| addr <= last && addr.saturating_add(size - 1) >= first)
    }
}
