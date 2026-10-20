//! Address translation: the parts shared by the 68851 and 68030 MMU
//! (mmu851.rs) and the 68040 one (mmu040.rs)

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::FpuM68kType;
use crate::cpu_m68k::M68040;
use crate::cpu_m68k::cache::CpuCaches;
use crate::cpu_m68k::cpu::{CpuError, CpuM68k, Group0Details, HistoryEntry};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// How the on-chip caches may treat an access, as the MMU gives it
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheMode {
    /// Cachable, writes go to memory. Also the mode of an access without
    /// translation.
    #[default]
    WriteThrough,
    /// Cachable, writes stay in the cache (68040)
    CopyBack,
    /// Not cachable. The 68040 serialized and non-serialized modes are not
    /// told apart.
    Inhibited,
}

/// Result of an address translation
#[derive(Clone, Copy, Debug)]
pub struct Translation {
    pub paddr: Address,
    pub cache: CacheMode,
}

/// A recent translation, which later accesses of the same kind in the same
/// page reuse without going through the MMU
#[derive(Clone, Copy)]
pub(in crate::cpu_m68k) struct LastTranslation {
    /// Logical address of the page, with the FC in its low bits: a page is
    /// at least 256 bytes
    pub tag: Address,
    /// Address bits of the page
    pub page_mask: Address,
    /// Physical address of the page
    pub paddr: Address,
    pub cache: CacheMode,
}

impl LastTranslation {
    /// No translation: bits 3-7 of a tag are clear
    const NONE: Self = Self {
        tag: 0xF8,
        page_mask: 0,
        paddr: 0,
        cache: CacheMode::WriteThrough,
    };

    #[inline(always)]
    fn hit(&self, fc: u8, laddr: Address) -> bool {
        (laddr & self.page_mask) | Address::from(fc) == self.tag
    }
}

/// No last translations, of any kind
pub(in crate::cpu_m68k) fn last_translations_none() -> [[LastTranslation; LAST_TRANSLATIONS]; 3] {
    [[LastTranslation::NONE; LAST_TRANSLATIONS]; 3]
}

/// Index of the last translations of data reads, instruction fetches and
/// data writes in CpuM68k::last_translation
pub(in crate::cpu_m68k) const LAST_TRANSLATION_READ: usize = 0;
pub(in crate::cpu_m68k) const LAST_TRANSLATION_FETCH: usize = 1;
pub(in crate::cpu_m68k) const LAST_TRANSLATION_WRITE: usize = 2;

/// Last translations kept per kind of access, one per 4 KB page modulo
/// their number (a power of two). 32 is the fastest on a Ryzen 9 9950X3D,
/// 16 and 64 are close; a host with smaller caches may prefer fewer.
pub(in crate::cpu_m68k) const LAST_TRANSLATIONS: usize = 32;
const _: () = assert!(LAST_TRANSLATIONS.is_power_of_two());

/// Entry of the last translations of a kind for logical address `laddr`: by
/// 4 KB page, whatever the page size, which only makes pages of other sizes
/// share or spread over entries
#[inline(always)]
fn last_translation_index(laddr: Address) -> usize {
    (laddr >> 12) as usize % LAST_TRANSLATIONS
}

/// Index in CpuM68k::pmmu_atc tables when URP is in use
pub(in crate::cpu_m68k) const PMMU_ATC_URP: usize = 0;
/// Index in CpuM68k::pmmu_atc tables when SRP is in use
pub(in crate::cpu_m68k) const PMMU_ATC_SRP: usize = 1;
/// Number of ATC tables in CpuM68k::pmmu_atc (one per root pointer)
pub(in crate::cpu_m68k) const PMMU_ATCS: usize = 2;

pub(in crate::cpu_m68k) fn atc_generation_default() -> u32 {
    // 0 is reserved for unused entries
    1
}

/// A resolved Address Translation Cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::cpu_m68k) struct PmmuAtcEntry {
    /// Physical page base address (low PS bits are zero).
    pub paddr: Address,
    /// Write-protect bit inherited from any table descriptor on the walk
    /// or from the leaf page descriptor.
    pub wp: bool,
    /// Supervisor-only bit inherited from any long-format descriptor on the walk.
    pub s: bool,
    /// Physical address of the leaf page descriptor (long-word aligned).
    /// Needed so writes can set the descriptor's M (modified) bit without
    /// replaying the full table walk.
    pub leaf_desc_addr: Address,
    /// Whether the M bit of the leaf descriptor is already set. When false,
    /// the next write through this entry must RMW the descriptor to set M.
    pub modified: bool,
    /// Bit mask of the function codes this slot has served (bit i = FC i).
    /// The direct-mapped slot is shared across function codes: translation
    /// does not depend on FC unless SRE or FCL are set, and the hardware
    /// may replace ATC entries at any time, so sharing is always legal.
    /// The mask exists only so PFLUSH/PLOAD can select entries precisely.
    #[serde(default)]
    pub fc_mask: u8,
    /// Cache mode of the page.
    #[serde(default)]
    pub cache: CacheMode,
    // If < current generation, then this entry has been flushed.
    // 0 means never used.
    #[serde(default)]
    pub generation: u32,
}

/// Custom (de)serialization for the PMMU ATC tables.
///
/// ATC is a linear table for O(1) lookup, which can get pretty large in serialized
/// form so this is stored as key/value + size instead.
pub(in crate::cpu_m68k) mod atc_serde {
    use super::{PMMU_ATCS, PmmuAtcEntry};
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct AtcTable {
        size: usize,
        entries: Vec<(usize, PmmuAtcEntry)>,
    }

    impl AtcTable {
        fn from_slots(slots: &[Option<PmmuAtcEntry>]) -> Self {
            Self {
                size: slots.len(),
                entries: slots
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| e.map(|e| (i, e)))
                    .collect(),
            }
        }

        fn into_slots(self) -> Option<Vec<Option<PmmuAtcEntry>>> {
            let mut slots = vec![None; self.size];
            for (i, e) in self.entries {
                *slots.get_mut(i)? = Some(e);
            }
            Some(slots)
        }
    }

    pub(in crate::cpu_m68k) fn serialize<S>(
        atc: &[Vec<Option<PmmuAtcEntry>>; PMMU_ATCS],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let tables = [AtcTable::from_slots(&atc[0]), AtcTable::from_slots(&atc[1])];
        tables.serialize(serializer)
    }

    pub(in crate::cpu_m68k) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<[Vec<Option<PmmuAtcEntry>>; PMMU_ATCS], D::Error>
    where
        D: Deserializer<'de>,
    {
        let [t0, t1] = <[AtcTable; PMMU_ATCS]>::deserialize(deserializer)?;
        let err = || D::Error::custom("ATC entry index out of range for table size");
        Ok([
            t0.into_slots().ok_or_else(err)?,
            t1.into_slots().ok_or_else(err)?,
        ])
    }
}

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
    TCaches,
> CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU, TCaches>
where
    TBus: Bus<Address, u8> + IrqSource,
    TCaches: CpuCaches,
{
    /// Enlarges ATC size if needed by configuration
    pub(in crate::cpu_m68k) fn pmmu_cache_ensure(&mut self) {
        let cache_size = if CPU_TYPE == M68040 {
            if !self.mmu040_enabled() {
                return;
            }
            (Address::MAX >> self.mmu040_page_shift()) as usize + 1
        } else {
            if !self.regs.pmmu.tc.enable() {
                return;
            }
            (Address::MAX >> (self.regs.pmmu.tc.is() + self.regs.pmmu.tc.ps() as Address)) as usize
                + 1
        };
        if self.pmmu_atc.iter().map(|atc| atc.len()).min().unwrap() < cache_size {
            log::debug!("Expanding cache size: {}", cache_size);
            self.pmmu_atc
                .iter_mut()
                .for_each(|atc| atc.resize(cache_size, None));
        }
    }

    /// Forgets the last translations, after anything that may change
    /// translations: ATC flushes, MMU register writes, reset
    pub(in crate::cpu_m68k) fn pmmu_translation_changed(&mut self) {
        self.last_translation = last_translations_none();
    }

    /// One of the last translations of kind `slot` for an access at `laddr`
    #[inline(always)]
    pub(in crate::cpu_m68k) fn last_translation_find(
        &self,
        slot: usize,
        fc: u8,
        laddr: Address,
    ) -> Option<Translation> {
        let e = &self.last_translation[slot][last_translation_index(laddr)];
        if !e.hit(fc, laddr) {
            return None;
        }
        Some(Translation {
            paddr: e.paddr | (laddr & !e.page_mask),
            cache: e.cache,
        })
    }

    /// Keeps a translation for later accesses of kind `slot`
    pub(in crate::cpu_m68k) fn last_translation_add(
        &mut self,
        slot: usize,
        fc: u8,
        laddr: Address,
        tr: &Translation,
    ) {
        let page_mask = !self.pmmu_page_mask();
        self.last_translation[slot][last_translation_index(laddr)] = LastTranslation {
            tag: (laddr & page_mask) | Address::from(fc),
            page_mask,
            paddr: tr.paddr & page_mask,
            cache: tr.cache,
        };
    }

    /// Address bits within an MMU page
    pub(in crate::cpu_m68k) fn pmmu_page_mask(&self) -> Address {
        let shift = if CPU_TYPE == M68040 {
            self.mmu040_page_shift()
        } else if self.regs.pmmu.tc.enable() {
            Address::from(self.regs.pmmu.tc.ps())
        } else {
            12
        };
        (1 << shift) - 1
    }

    /// Flushes complete ATC
    pub(in crate::cpu_m68k) fn pmmu_cache_invalidate(&mut self) {
        self.pmmu_translation_changed();
        // Incrementing generation invalidates all entries with a lower generation,
        // making ATC flushes very cheap rather than zeroizing the entire cache.
        self.pmmu_atc_generation = self.pmmu_atc_generation.wrapping_add(1);

        if self.pmmu_atc_generation == 0 {
            // Handle wraparound with a full cache purge.
            self.pmmu_atc.iter_mut().for_each(|atc| atc.fill(None));
            self.pmmu_atc_generation = 1;
        }
    }

    /// Looks an address up in the ATC
    #[inline(always)]
    pub(in crate::cpu_m68k) fn pmmu_atc_lookup(
        &self,
        atc: usize,
        key: usize,
    ) -> Option<PmmuAtcEntry> {
        match self.pmmu_atc[atc][key] {
            Some(e) if e.generation == self.pmmu_atc_generation => Some(e),
            _ => None,
        }
    }

    /// Translates a logical address
    pub(in crate::cpu_m68k) fn pmmu_translate(
        &mut self,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<Translation> {
        if !PMMU {
            return Ok(Translation {
                paddr: vaddr,
                cache: CacheMode::WriteThrough,
            });
        }

        if CPU_TYPE == M68040 {
            self.mmu040_translate(fc, vaddr, writing)
        } else {
            self.mmu851_translate(fc, vaddr, writing)
        }
    }

    /// Records a page-fault history entry if history recording is enabled.
    pub(in crate::cpu_m68k) fn pmmu_record_pagefault(&mut self, vaddr: Address, writing: bool) {
        if self.history_enabled {
            self.history.push_back(HistoryEntry::Pagefault {
                address: vaddr,
                write: writing,
            });
        }
    }

    /// Builds the Group-0 BusError stack frame error value for a page fault.
    pub(in crate::cpu_m68k) fn pmmu_pagefault_to_buserror(
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> anyhow::Error {
        anyhow!(CpuError::BusError(Group0Details {
            function_code: fc,
            ir: 0,
            instruction: false,
            read: !writing,
            address: vaddr,
            start_pc: 0,
            size: 0,
            atc_fault: true,
        }))
    }
}

#[cfg(all(test, feature = "savestates"))]
mod tests {
    use super::*;

    fn sample_entry(paddr: Address) -> PmmuAtcEntry {
        PmmuAtcEntry {
            paddr,
            wp: true,
            s: false,
            leaf_desc_addr: paddr + 0x10,
            modified: true,
            fc_mask: 0,
            cache: CacheMode::WriteThrough,
            generation: atc_generation_default(),
        }
    }

    #[test]
    fn atc_serde_roundtrip() {
        let mut table0 = vec![None; 1024];
        table0[3] = Some(sample_entry(0x1000));
        table0[1000] = Some(sample_entry(0x2000));
        let mut table1 = vec![None; 64];
        table1[0] = Some(sample_entry(0x3000));

        let atc = [table0, table1];

        #[derive(serde::Serialize, serde::Deserialize)]
        struct Wrap {
            #[serde(with = "super::atc_serde")]
            atc: [Vec<Option<PmmuAtcEntry>>; PMMU_ATCS],
        }

        let bytes = postcard::to_allocvec(&Wrap { atc: atc.clone() }).unwrap();
        let restored: Wrap = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(restored.atc[0].len(), 1024);
        assert_eq!(restored.atc[1].len(), 64);
        assert_eq!(restored.atc, atc);
    }
}
