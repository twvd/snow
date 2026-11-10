//! M68030 address translation cache (MC68030UM 9.4)
//!
//! A 22-entry fully associative cache. Its tags are the function code and
//! the logical page address. Software relies on entries being replaced: a
//! translation that is not flushed after its descriptor changed is only
//! used until it is replaced.

use crate::bus::Address;
use crate::cpu_m68k::pmmu::translate::CacheMode;

use serde::{Deserialize, Serialize};

const ENTRIES: usize = 22;

/// Tag bits of the function code. Tags hold the logical page address, which
/// leaves the low 8 bits free.
const TAG_FC: Address = 0b111;
/// Tag bit of a valid entry
const TAG_VALID: Address = 1 << 3;

/// An ATC entry (MC68030UM 9.4)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::cpu_m68k) struct Atc030Entry {
    /// Logical page address, function code and valid bit, see Atc030::tag()
    pub tag: Address,
    /// Physical page address
    pub paddr: Address,
    /// The table search failed (B): an invalid descriptor, a supervisor or
    /// limit violation, or a bus error. Accesses take a bus error.
    pub bus_error: bool,
    /// Write protected (WP)
    pub wp: bool,
    /// Modified (M)
    pub modified: bool,
    /// Cache mode, from CI
    pub cache: CacheMode,
}

/// The ATC
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::cpu_m68k) struct Atc030 {
    entries: [Atc030Entry; ENTRIES],
    /// History bits of the replacement, one per entry. The manual only says
    /// the 68030 uses "a pseudo least recently used algorithm" with "a
    /// validity bit and an internal history bit" per entry: an entry's bit is
    /// set when it is used, and when all are set, all but that one are
    /// cleared. The replacement takes an invalid entry, else the first one
    /// whose bit is clear. Accesses that reuse the CPU's last translations
    /// don't reach the ATC and don't set the bits.
    history: u32,
}

impl Default for Atc030 {
    fn default() -> Self {
        Self {
            entries: [Atc030Entry::default(); ENTRIES],
            history: 0,
        }
    }
}

impl Atc030 {
    /// Tag of the page holding `laddr`, for function code `fc`
    #[inline(always)]
    pub fn tag(laddr: Address, page_mask: Address, fc: u8) -> Address {
        (laddr & !page_mask) | TAG_VALID | (Address::from(fc) & TAG_FC)
    }

    /// Logical page address of a tag
    pub fn tag_page(tag: Address) -> Address {
        tag & !(TAG_VALID | TAG_FC)
    }

    /// Function code of a tag
    pub fn tag_fc(tag: Address) -> u8 {
        (tag & TAG_FC) as u8
    }

    /// Marks an entry as used
    fn used(&mut self, index: usize) {
        self.history |= 1 << index;
        if self.history == (1 << ENTRIES) - 1 {
            self.history = 1 << index;
        }
    }

    /// Looks a tag up, for an access
    #[inline(always)]
    pub fn lookup(&mut self, tag: Address) -> Option<Atc030Entry> {
        let index = self.entries.iter().position(|e| e.tag == tag)?;
        self.used(index);
        Some(self.entries[index])
    }

    /// Looks a tag up without counting it as a use
    pub fn peek(&self, tag: Address) -> Option<Atc030Entry> {
        self.entries.iter().find(|e| e.tag == tag).copied()
    }

    /// Stores an entry: over the entry with the same tag if any, else in an
    /// invalid entry, else in the one the history bits give. Returns the tag
    /// of the valid entry that was replaced, if any, which may be the same
    /// tag with an older translation.
    pub fn insert(&mut self, entry: Atc030Entry) -> Option<Address> {
        let history = self.history;
        let index = self
            .entries
            .iter()
            .position(|e| e.tag == entry.tag)
            .or_else(|| self.entries.iter().position(|e| e.tag & TAG_VALID == 0))
            .or_else(|| (0..ENTRIES).find(|&i| history & (1 << i) == 0))
            .unwrap_or(0);
        let old = std::mem::replace(&mut self.entries[index], entry);
        self.used(index);
        (old.tag & TAG_VALID != 0).then_some(old.tag)
    }

    /// Invalidates the entries whose function code and page `select` takes
    pub fn flush(&mut self, mut select: impl FnMut(u8, Address) -> bool) {
        for e in &mut self.entries {
            if e.tag & TAG_VALID != 0 && select(Self::tag_fc(e.tag), Self::tag_page(e.tag)) {
                *e = Atc030Entry::default();
            }
        }
    }

    /// Invalidates all entries
    pub fn flush_all(&mut self) {
        self.entries = [Atc030Entry::default(); ENTRIES];
    }
}
