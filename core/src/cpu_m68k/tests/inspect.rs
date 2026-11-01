//! Tests for the accesses of the host to memory: they must leave no trace in
//! the CPU, the caches or the page tables

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::pmmu::regs::{RootPointerReg, TcReg};
use crate::cpu_m68k::regs::Register;
use crate::cpu_m68k::{CpuM68030, CpuM68040Fpu, M68030_ADDRESS_MASK, M68040_ADDRESS_MASK};
use crate::types::Long;

const FC_SUPER_DATA: u8 = 5;
/// Logical page mapped elsewhere by the tests
const MAPPED: Address = 0x5000;
/// Where MAPPED is
const MAPPED_TO: Address = 0x9000;

type Cpu030 = CpuM68030<Testbus<Address, u8, Bus68020>>;
type Cpu040 = CpuM68040Fpu<Testbus<Address, u8, Bus68020>>;

macro_rules! write_long {
    ($cpu:expr, $addr:expr, $value:expr) => {
        for (i, b) in Long::to_be_bytes($value).into_iter().enumerate() {
            $cpu.bus.mem.insert($addr + i as Address, b);
        }
    };
}

macro_rules! read_long {
    ($cpu:expr, $addr:expr) => {
        Long::from_be_bytes(std::array::from_fn(|i| {
            $cpu.bus
                .mem
                .get(&($addr + i as Address))
                .copied()
                .unwrap_or(0)
        }))
    };
}

const TABLE_A: Address = 0x10000;
const TABLE_B: Address = 0x11000;

/// A 68030 with 4 KB pages and two levels of short tables: the first 128 KB
/// map to themselves but for MAPPED, and page $20000 is invalid
fn cpu030() -> Cpu030 {
    let mut cpu = Cpu030::new(Testbus::new(M68030_ADDRESS_MASK));
    write_long!(cpu, TABLE_A, TABLE_B | 0b10);
    for page in 0..0x20 {
        write_long!(cpu, TABLE_B + page * 4, (page << 12) | 0b01);
    }
    write_long!(cpu, TABLE_B + (MAPPED >> 12) * 4, MAPPED_TO | 0b01);
    // Enabled, 4 KB pages, TIA = TIB = 10
    cpu.regs.pmmu.tc = TcReg(0x80C0_AA00);
    cpu.regs.pmmu.crp = RootPointerReg(0x7FFF_0002_0000_0000 | u64::from(TABLE_A));
    cpu.pmmu_cache_ensure();
    cpu.regs.sr.set_supervisor(true);
    cpu
}

const ROOT_040: Address = 0x10000;
const PTR_040: Address = 0x11000;
const PAGE_040: Address = 0x12000;
const RESIDENT: Long = 1;
const TABLE_VALID: Long = 2;
const COPYBACK: Long = 1 << 5;

/// A 68040 with 4 KB pages: the first 64 pages map to themselves but for
/// MAPPED, in copyback mode, and the next pointer entry is invalid
fn cpu040() -> Cpu040 {
    let mut cpu = Cpu040::new(Testbus::new(M68040_ADDRESS_MASK));
    write_long!(cpu, ROOT_040, PTR_040 | TABLE_VALID);
    write_long!(cpu, PTR_040, PAGE_040 | TABLE_VALID);
    for page in 0..0x40 {
        write_long!(cpu, PAGE_040 + page * 4, (page << 12) | RESIDENT | COPYBACK);
    }
    write_long!(
        cpu,
        PAGE_040 + (MAPPED >> 12) * 4,
        MAPPED_TO | RESIDENT | COPYBACK
    );
    cpu.regs.write(Register::SRP040, ROOT_040);
    cpu.regs.write(Register::URP040, ROOT_040);
    cpu.regs.write(Register::TC040, 0x8000_u32);
    cpu.pmmu_cache_ensure();
    cpu.regs.sr.set_supervisor(true);
    cpu
}

#[test]
fn translate_030_walks_without_a_trace() {
    let mut cpu = cpu030();
    let tables = cpu.bus.mem.clone();
    let atc = cpu.pmmu_atc.clone();
    let cycles = cpu.cycles;
    let psr = cpu.regs.pmmu.psr;
    cpu.regs.pmmu.last_desc = 0x1234;

    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, MAPPED + 0x123, true),
        Some(MAPPED_TO + 0x123)
    );
    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, 0x2000, false),
        Some(0x2000)
    );
    // Table B only has 32 entries with a descriptor: the others read as invalid
    assert_eq!(cpu.inspect_translate(FC_SUPER_DATA, 0x20000, false), None);
    // Beyond the entries of table A
    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, 0x0040_0000, false),
        None
    );

    // No U or M bits, no ATC entry, no time, no MMU register
    assert!(cpu.bus.mem == tables);
    assert!(cpu.pmmu_atc == atc);
    assert_eq!(cpu.cycles, cycles);
    assert_eq!(cpu.regs.pmmu.psr.0, psr.0);
    assert_eq!(cpu.regs.pmmu.last_desc, 0x1234);
}

#[test]
fn translate_030_same_as_an_access() {
    let mut cpu = cpu030();
    for addr in [0x1234, MAPPED, MAPPED + 0xFFF, 0x1F000] {
        let inspected = cpu.inspect_translate(FC_SUPER_DATA, addr, false);
        let real = cpu
            .pmmu_translate(FC_SUPER_DATA, addr, false)
            .unwrap()
            .paddr;
        assert_eq!(inspected, Some(real));
        // Now from the ATC
        assert_eq!(
            cpu.inspect_translate(FC_SUPER_DATA, addr, false),
            Some(real)
        );
    }
}

#[test]
fn translate_030_ignores_protection() {
    let mut cpu = cpu030();
    // Write protected page
    write_long!(cpu, TABLE_B + (MAPPED >> 12) * 4, MAPPED_TO | 0b101);
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, MAPPED, true).is_err());
    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, MAPPED, true),
        Some(MAPPED_TO)
    );
}

#[test]
fn translate_030_uses_a_stale_atc_entry() {
    let mut cpu = cpu030();
    cpu.pmmu_translate(FC_SUPER_DATA, MAPPED, false).unwrap();
    // The CPU goes on using the ATC entry until it is flushed
    write_long!(cpu, TABLE_B + (MAPPED >> 12) * 4, 0xA000 | 0b01);
    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, MAPPED, false),
        Some(MAPPED_TO)
    );
}

#[test]
fn translate_030_unreadable_tables() {
    let mut cpu = cpu030();
    cpu.bus.fault_ranges.push(TABLE_B..=TABLE_B + 0xFFF);
    assert_eq!(cpu.inspect_translate(FC_SUPER_DATA, 0x2000, false), None);
}

#[test]
fn translate_040_walks_without_a_trace() {
    let mut cpu = cpu040();
    let tables = cpu.bus.mem.clone();
    let atc = cpu.pmmu_atc.clone();
    let cycles = cpu.cycles;

    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, MAPPED + 0x123, true),
        Some(MAPPED_TO + 0x123)
    );
    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, 0x2000, false),
        Some(0x2000)
    );
    // Next pointer entry, next root entry
    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, 0x0004_0000, false),
        None
    );
    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, 0x0200_0000, false),
        None
    );

    assert!(cpu.bus.mem == tables);
    assert!(cpu.pmmu_atc == atc);
    assert_eq!(cpu.cycles, cycles);
}

#[test]
fn translate_040_same_as_an_access() {
    let mut cpu = cpu040();
    for addr in [0x1234, MAPPED, MAPPED + 0xFFF, 0x3F000] {
        let inspected = cpu.inspect_translate(FC_SUPER_DATA, addr, false);
        let real = cpu
            .pmmu_translate(FC_SUPER_DATA, addr, false)
            .unwrap()
            .paddr;
        assert_eq!(inspected, Some(real));
        assert_eq!(
            cpu.inspect_translate(FC_SUPER_DATA, addr, false),
            Some(real)
        );
    }
}

#[test]
fn translate_040_indirect_descriptor() {
    let mut cpu = cpu040();
    write_long!(cpu, 0x13000, 0xB000 | RESIDENT);
    write_long!(cpu, PAGE_040 + 3 * 4, 0x13000 | 0b10);
    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, 0x3010, false),
        Some(0xB010)
    );
    // No second level of indirection
    write_long!(cpu, 0x13000, 0x14000 | 0b10);
    assert_eq!(cpu.inspect_translate(FC_SUPER_DATA, 0x3010, false), None);
    assert_eq!(read_long!(cpu, 0x13000), 0x14000 | 0b10);
}

#[test]
fn translate_040_reads_tables_from_the_data_cache() {
    let mut cpu = cpu040();
    // Enable the data cache, change a page descriptor with a copyback write
    cpu.regs.write(Register::CACR, 0x8000_0000_u32);
    let desc = PAGE_040 + (0x7000 >> 12) * 4;
    cpu.write_ticks::<Long>(desc, 0xC000 | RESIDENT | COPYBACK)
        .unwrap();
    assert_eq!(read_long!(cpu, desc), 0x7000 | RESIDENT | COPYBACK);

    assert_eq!(
        cpu.inspect_translate(FC_SUPER_DATA, 0x7000, false),
        Some(0xC000)
    );
}
