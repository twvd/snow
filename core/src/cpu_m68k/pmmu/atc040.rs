//! M68040 address translation caches (MC68040UM 3.3)
//!
//! The 68040 has two ATCs, one in the instruction MMU and one in the data
//! MMU. Each is four-way set associative with 16 sets, 64 entries in all.
//! Software relies on entries being replaced: a translation that is not
//! flushed after its descriptor changed is only used until it is replaced.

use crate::bus::Address;
use crate::cpu_m68k::pmmu::translate::CacheMode;

use serde::{Deserialize, Serialize};

const SETS: usize = 16;
const WAYS: usize = 4;

/// Tag bit of a supervisor entry (FC2). Tags hold the logical page address,
/// which leaves the low 12 bits free.
const TAG_SUPERVISOR: Address = 1 << 0;
/// Tag bit of a valid entry
const TAG_VALID: Address = 1 << 1;

/// Index of the data ATC in CpuM68k::atc040
pub(in crate::cpu_m68k) const ATC040_DATA: usize = 0;
/// Index of the instruction ATC in CpuM68k::atc040
pub(in crate::cpu_m68k) const ATC040_INSTRUCTION: usize = 1;

/// An ATC entry (MC68040UM figure 3-21)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::cpu_m68k) struct Atc040Entry {
    /// Logical page address, FC2 and valid bits, see Atc040::tag()
    pub tag: Address,
    /// Physical page address
    pub paddr: Address,
    /// The table search found a valid page descriptor (R)
    pub resident: bool,
    /// Write protected (W)
    pub wp: bool,
    /// Supervisor only (S)
    pub s: bool,
    /// Global (G): kept by PFLUSHN and PFLUSHAN
    pub global: bool,
    /// Modified (M)
    pub modified: bool,
    /// Cache mode (CM)
    pub cache: CacheMode,
}

/// One ATC
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::cpu_m68k) struct Atc040 {
    sets: [[Atc040Entry; WAYS]; SETS],
    /// Replacement counter: "A 2-bit counter, which is incremented for each
    /// ATC access, points to the entry to replace when an access misses in
    /// the ATC" (MC68040UM 3.3). Accesses that reuse the CPU's last
    /// translations don't reach the ATC and don't count.
    counter: u8,
}

impl Atc040 {
    /// Tag of the page holding `laddr`, for a supervisor or user access
    #[inline(always)]
    pub fn tag(laddr: Address, page_mask: Address, supervisor: bool) -> Address {
        (laddr & !page_mask) | TAG_VALID | if supervisor { TAG_SUPERVISOR } else { 0 }
    }

    /// Logical page address of a tag
    pub fn tag_page(tag: Address) -> Address {
        tag & !(TAG_VALID | TAG_SUPERVISOR)
    }

    /// Whether a tag is for supervisor accesses
    pub fn tag_supervisor(tag: Address) -> bool {
        tag & TAG_SUPERVISOR != 0
    }

    /// The set a page goes in: the four logical address bits just above the
    /// page offset (MC68040UM 3.3)
    #[inline(always)]
    fn set(tag: Address, page_shift: u32) -> usize {
        (tag >> page_shift) as usize % SETS
    }

    /// Looks a tag up, for an access
    #[inline(always)]
    pub fn lookup(&mut self, tag: Address, page_shift: u32) -> Option<Atc040Entry> {
        self.counter = self.counter.wrapping_add(1);
        self.peek(tag, page_shift)
    }

    /// Looks a tag up without counting it as an access
    #[inline(always)]
    pub fn peek(&self, tag: Address, page_shift: u32) -> Option<Atc040Entry> {
        self.sets[Self::set(tag, page_shift)]
            .iter()
            .find(|e| e.tag == tag)
            .copied()
    }

    /// Stores an entry: over the entry with the same tag if any, else in an
    /// invalid entry of its set, else in the one the counter points to.
    /// Returns the tag of the valid entry that was replaced, if any, which
    /// may be the same tag with an older translation.
    pub fn insert(&mut self, entry: Atc040Entry, page_shift: u32) -> Option<Address> {
        let set = &mut self.sets[Self::set(entry.tag, page_shift)];
        let counted = usize::from(self.counter) % WAYS;
        let way = set
            .iter()
            .position(|e| e.tag == entry.tag)
            .or_else(|| set.iter().position(|e| e.tag & TAG_VALID == 0))
            .unwrap_or(counted);
        let old = std::mem::replace(&mut set[way], entry);
        (old.tag & TAG_VALID != 0).then_some(old.tag)
    }

    /// Invalidates the entry with a tag, and returns whether there was one.
    /// With `nonglobal`, keeps it if it is global.
    pub fn flush_tag(&mut self, tag: Address, page_shift: u32, nonglobal: bool) -> bool {
        let set = &mut self.sets[Self::set(tag, page_shift)];
        match set
            .iter_mut()
            .find(|e| e.tag == tag && !(nonglobal && e.global))
        {
            Some(e) => {
                *e = Atc040Entry::default();
                true
            }
            None => false,
        }
    }

    /// Invalidates all entries. With `nonglobal`, keeps the global ones.
    pub fn flush_all(&mut self, nonglobal: bool) {
        for e in self.sets.iter_mut().flatten() {
            if !(nonglobal && e.global) {
                *e = Atc040Entry::default();
            }
        }
    }
}
