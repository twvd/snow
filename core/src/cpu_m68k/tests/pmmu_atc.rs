//! Tests for the PMMU address translation cache

use crate::bus::Address;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::pmmu::translate::{PMMU_ATC_SRP, PmmuAtcEntry};
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};

type TestCpu = CpuM68030Fpu<Testbus<Address, u8>>;

const KEY: usize = 4;

fn testcpu() -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68030_ADDRESS_MASK));
    cpu.pmmu_atc.iter_mut().for_each(|t| t.resize(16, None));
    cpu
}

fn entry(cpu: &TestCpu, paddr: Address) -> PmmuAtcEntry {
    PmmuAtcEntry {
        paddr,
        wp: false,
        s: false,
        leaf_desc_addr: 0x1000,
        modified: false,
        generation: cpu.pmmu_atc_generation,
    }
}

/// A flush moves the generation on rather than clearing the tables, so stale
/// entries are left behind but must never be handed out again.
#[test]
fn flush_invalidates_by_generation() {
    let mut cpu = testcpu();
    let e = entry(&cpu, 0x0020_0000);
    cpu.pmmu_atc[PMMU_ATC_SRP][KEY] = Some(e);

    assert_eq!(cpu.pmmu_atc_lookup(PMMU_ATC_SRP, KEY), Some(e));

    cpu.pmmu_cache_invalidate();
    assert_eq!(cpu.pmmu_atc_lookup(PMMU_ATC_SRP, KEY), None);
    assert!(cpu.pmmu_atc[PMMU_ATC_SRP][KEY].is_some(),);

    // Refilling the slot in the current generation makes it usable again
    let e2 = entry(&cpu, 0x0040_0000);
    cpu.pmmu_atc[PMMU_ATC_SRP][KEY] = Some(e2);
    assert_eq!(cpu.pmmu_atc_lookup(PMMU_ATC_SRP, KEY), Some(e2));
}

#[test]
fn generation_wraparound_clears_the_tables() {
    let mut cpu = testcpu();
    cpu.pmmu_atc_generation = u32::MAX;

    let e = entry(&cpu, 0x0020_0000);
    cpu.pmmu_atc[PMMU_ATC_SRP][KEY] = Some(e);
    assert_eq!(cpu.pmmu_atc_lookup(PMMU_ATC_SRP, KEY), Some(e));

    cpu.pmmu_cache_invalidate();

    assert_eq!(cpu.pmmu_atc_generation, 1);
    assert_eq!(cpu.pmmu_atc[PMMU_ATC_SRP][KEY], None);
    assert_eq!(cpu.pmmu_atc_lookup(PMMU_ATC_SRP, KEY), None);
}
