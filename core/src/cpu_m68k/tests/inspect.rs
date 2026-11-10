//! Tests for the accesses of the host to memory: they must leave no trace in
//! the CPU, the caches or the page tables

use crate::bus::access::{Bus68000, Bus68020};
use crate::bus::testbus::Testbus;
use crate::bus::{Address, InspectRun};
use crate::cpu_m68k::bus::InspectView;
use crate::cpu_m68k::pmmu::regs::{RootPointerReg, TcReg};
use crate::cpu_m68k::regs::Register;
use crate::cpu_m68k::{
    CpuM68000, CpuM68020, CpuM68030, CpuM68040, FPU_M68040, FPU_NONE, M68000_ADDRESS_MASK,
    M68020_ADDRESS_MASK, M68030_ADDRESS_MASK, M68040_ADDRESS_MASK,
};
use crate::types::Long;

const FC_SUPER_DATA: u8 = 5;
/// Logical page mapped elsewhere by the tests
const MAPPED: Address = 0x5000;
/// Where MAPPED is
const MAPPED_TO: Address = 0x9000;

type Cpu030 = CpuM68030<Testbus<Address, u8, Bus68020>>;
type Cpu040 = CpuM68040<Testbus<Address, u8, Bus68020>>;

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
    let mut cpu = Cpu030::new(Testbus::new(M68030_ADDRESS_MASK), FPU_NONE);
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
    let mut cpu = Cpu040::new(Testbus::new(M68040_ADDRESS_MASK), FPU_M68040);
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
    let atc040 = cpu.atc040.clone();
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
    assert!(cpu.atc040 == atc040);
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

/// CACR: enable both 68030 caches
const CACR_030: Long = 0x0101;
/// CACR: enable the 68040 data cache
const CACR_040_DATA: Long = 0x8000_0000;
/// CACR: enable the 68040 instruction cache
const CACR_040_INSTR: Long = 0x0000_8000;

/// Asserts that running $body changes nothing in $cpu
macro_rules! assert_no_trace {
    ($cpu:expr, $body:block) => {{
        let caches = $cpu.caches.clone();
        let mem = $cpu.bus.mem.clone();
        let atc = $cpu.pmmu_atc.clone();
        let atc040 = $cpu.atc040.clone();
        let cycles = $cpu.cycles;
        $body;
        assert!($cpu.caches == caches);
        assert!($cpu.bus.mem == mem);
        assert!($cpu.pmmu_atc == atc);
        assert!($cpu.atc040 == atc040);
        assert_eq!($cpu.cycles, cycles);
    }};
}

#[test]
fn no_caches_goes_to_the_bus() {
    let mut cpu = CpuM68000::<Testbus<Address, u8, Bus68000>>::new(
        Testbus::new(M68000_ADDRESS_MASK),
        FPU_NONE,
    );
    cpu.bus.mem.insert(0x1234, 0x56);
    assert_eq!(cpu.inspect_read(0x1234, true), Some(0x56));
    assert_eq!(cpu.inspect_read(0x1234, false), Some(0x56));
    // Upper address bits don't make it out of the 68000
    assert_eq!(cpu.inspect_read(0xFF00_1234, true), Some(0x56));
    assert_eq!(cpu.inspect_write(0x1235, 0x78, false), Some(()));
    assert_eq!(cpu.bus.mem[&0x1235], 0x78);

    cpu.bus.fault_ranges.push(0x2000..=0x2FFF);
    assert_eq!(cpu.inspect_read(0x2000, true), None);
    assert_eq!(cpu.inspect_write(0x2000, 0, true), None);
}

#[test]
fn read_030_virtual_physical_and_misses() {
    let mut cpu = cpu030();
    cpu.regs.write(Register::CACR, CACR_030);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    write_long!(cpu, MAPPED_TO + 0x100, 0xAABB_CCDD);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1122_3344);
    // Something behind the back of the cache
    write_long!(cpu, MAPPED_TO, 0x5566_7788);

    assert_no_trace!(cpu, {
        // Hit, as the CPU would see it
        assert_eq!(cpu.inspect_read(MAPPED + 1, false), Some(0x22));
        assert_eq!(cpu.inspect_read(MAPPED_TO + 1, true), Some(0x22));
        // The next long word was not loaded
        assert_eq!(cpu.inspect_read(MAPPED + 4, false), Some(0));
        // Same line, another page: the entry is not for this address
        assert_eq!(cpu.inspect_read(MAPPED_TO + 0x100, true), Some(0xAA));
        assert_eq!(cpu.inspect_read(MAPPED + 0x100, false), Some(0xAA));
        // No translation
        assert_eq!(cpu.inspect_read(0x20000, false), None);
        // A physical address that isn't the page: memory
        assert_eq!(cpu.inspect_read(MAPPED, true), Some(0));
    });

    // A disabled cache isn't looked at
    cpu.regs.write(Register::CACR, 0_u32);
    assert_eq!(cpu.inspect_read(MAPPED_TO, true), Some(0x55));
    assert_eq!(cpu.inspect_read(MAPPED, false), Some(0x55));
}

#[test]
fn read_030_virtual_needs_the_tag_of_the_space() {
    let mut cpu = cpu030();
    cpu.regs.write(Register::CACR, CACR_030);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    cpu.regs.sr.set_supervisor(false);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1122_3344);
    write_long!(cpu, MAPPED_TO, 0x5566_7788);

    assert_eq!(cpu.inspect_read(MAPPED, false), Some(0x11));
    // The supervisor would miss, and read memory
    cpu.regs.sr.set_supervisor(true);
    assert_eq!(cpu.inspect_read(MAPPED, false), Some(0x55));
    // A physical read finds the entry in whichever space it is
    assert_eq!(cpu.inspect_read(MAPPED_TO, true), Some(0x11));
}

#[test]
fn write_030_updates_memory_and_entries() {
    for phys in [true, false] {
        let mut cpu = cpu030();
        cpu.regs.write(Register::CACR, CACR_030);
        write_long!(cpu, MAPPED_TO, 0x1122_3344);
        write_long!(cpu, MAPPED_TO + 0x100, 0xAABB_CCDD);
        assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1122_3344);
        assert_eq!(cpu.read_ticks_program::<Long>(MAPPED).unwrap(), 0x1122_3344);

        let addr = if phys { MAPPED_TO } else { MAPPED };
        let tables = read_long!(cpu, TABLE_B + (MAPPED >> 12) * 4);
        let atc = cpu.pmmu_atc.clone();
        let cycles = cpu.cycles;
        assert_eq!(cpu.inspect_write(addr + 1, 0x99, phys), Some(()));
        assert_eq!(cpu.cycles, cycles);
        assert!(cpu.pmmu_atc == atc);
        assert_eq!(read_long!(cpu, MAPPED_TO), 0x1199_3344);
        assert_eq!(read_long!(cpu, TABLE_B + (MAPPED >> 12) * 4), tables);

        // Both entries hit, with the new value, without a bus cycle
        cpu.bus.reset_trace();
        assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1199_3344);
        assert_eq!(cpu.read_ticks_program::<Long>(MAPPED).unwrap(), 0x1199_3344);
        assert!(cpu.bus.get_trace().is_empty());

        // Same entries, another page: they are left alone
        let addr = if phys { MAPPED_TO } else { MAPPED } + 0x100;
        let caches = cpu.caches.clone();
        assert_eq!(cpu.inspect_write(addr, 0x77, phys), Some(()));
        assert!(cpu.caches == caches);
        assert_eq!(read_long!(cpu, MAPPED_TO + 0x100), 0x77BB_CCDD);
    }
}

#[test]
fn write_030_reaches_the_entry_of_another_space() {
    let mut cpu = cpu030();
    cpu.regs.write(Register::CACR, CACR_030);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    cpu.regs.sr.set_supervisor(false);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1122_3344);

    // From the supervisor view, to an entry tagged with user data
    cpu.regs.sr.set_supervisor(true);
    assert_eq!(cpu.inspect_write(MAPPED, 0x99, false), Some(()));
    cpu.regs.sr.set_supervisor(false);
    cpu.bus.reset_trace();
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x9922_3344);
    assert!(cpu.bus.get_trace().is_empty());
}

#[test]
fn write_030_refused_by_memory_leaves_entries_alone() {
    let mut cpu = cpu030();
    cpu.regs.write(Register::CACR, CACR_030);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1122_3344);
    cpu.bus.fault_ranges.push(MAPPED_TO..=MAPPED_TO + 0xFFF);

    assert_no_trace!(cpu, {
        assert_eq!(cpu.inspect_write(MAPPED, 0x99, false), None);
    });
    assert_eq!(cpu.inspect_write(0x20000, 0x99, false), None);
}

#[test]
fn write_030_disabled_cache_still_updates() {
    let mut cpu = cpu030();
    cpu.regs.write(Register::CACR, CACR_030);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1122_3344);

    cpu.regs.write(Register::CACR, 0_u32);
    assert_eq!(cpu.inspect_write(MAPPED_TO, 0x99, true), Some(()));
    cpu.regs.write(Register::CACR, CACR_030);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x9922_3344);
}

#[test]
fn words_are_big_endian() {
    let mut cpu = cpu030();
    assert_eq!(cpu.inspect_write_word(MAPPED, 0x1234, false), Some(()));
    assert_eq!(cpu.bus.mem[&MAPPED_TO], 0x12);
    assert_eq!(cpu.bus.mem[&(MAPPED_TO + 1)], 0x34);
    assert_eq!(cpu.inspect_read_word(MAPPED_TO, true), Some(0x1234));
    // No translation
    assert_eq!(cpu.inspect_read_word(0x20000, false), None);
    assert_eq!(cpu.inspect_write_word(0x20000, 0, false), None);
}

#[test]
fn caches_020() {
    let mut cpu = CpuM68020::<Testbus<Address, u8, Bus68020>>::new(
        Testbus::new(M68020_ADDRESS_MASK),
        FPU_NONE,
    );
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.write(Register::CACR, 1_u32);
    write_long!(cpu, 0x2000, 0x1122_3344);
    assert_eq!(cpu.read_ticks_program::<Long>(0x2000).unwrap(), 0x1122_3344);
    write_long!(cpu, 0x2000, 0x5566_7788);

    // The cache only holds instructions, and this is a view of data
    assert_no_trace!(cpu, {
        assert_eq!(cpu.inspect_read(0x2000, false), Some(0x55));
        assert_eq!(cpu.inspect_read(0x2000, true), Some(0x55));
    });

    // Same entry, another address: left alone
    let caches = cpu.caches.clone();
    assert_eq!(cpu.inspect_write(0x2100, 0x99, true), Some(()));
    assert!(cpu.caches == caches);

    assert_eq!(cpu.inspect_write(0x2000, 0x99, true), Some(()));
    assert_eq!(cpu.bus.mem[&0x2000], 0x99);
    cpu.bus.reset_trace();
    assert_eq!(cpu.read_ticks_program::<Long>(0x2000).unwrap(), 0x9922_3344);
    assert!(cpu.bus.get_trace().is_empty());
}

/// CPUSHA for both caches
const CPUSHA: [u8; 2] = [0xF4, 0xF8];

/// Runs CPUSHA, from a page that isn't in the way of the tests
fn cpusha(cpu: &mut Cpu040) {
    cpu.bus.mem.insert(0x1000, CPUSHA[0]);
    cpu.bus.mem.insert(0x1001, CPUSHA[1]);
    cpu.set_pc(0x1000).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
}

#[test]
fn read_040_dirty_data_and_misses() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    write_long!(cpu, MAPPED_TO + 0x100, 0xAABB_CCDD);
    cpu.write_ticks::<Long>(MAPPED, 0x5566_7788).unwrap();
    assert_eq!(read_long!(cpu, MAPPED_TO), 0x1122_3344);

    assert_no_trace!(cpu, {
        assert_eq!(cpu.inspect_read(MAPPED + 1, false), Some(0x66));
        assert_eq!(cpu.inspect_read(MAPPED_TO + 1, true), Some(0x66));
        // A miss loads nothing
        assert_eq!(cpu.inspect_read(MAPPED_TO + 0x100, true), Some(0xAA));
        assert_eq!(cpu.inspect_read(MAPPED + 0x100, false), Some(0xAA));
        assert_eq!(cpu.inspect_read(0x0004_0000, false), None);
    });

    // A disabled data cache isn't looked at
    cpu.regs.write(Register::CACR, 0_u32);
    assert_eq!(cpu.inspect_read(MAPPED_TO, true), Some(0x11));
}

#[test]
fn write_040_dirty_long_stays_in_the_cache() {
    for phys in [true, false] {
        let mut cpu = cpu040();
        cpu.regs.write(Register::CACR, CACR_040_DATA);
        write_long!(cpu, MAPPED_TO, 0x1122_3344);
        cpu.write_ticks::<Long>(MAPPED, 0x5566_7788).unwrap();

        let addr = if phys { MAPPED_TO } else { MAPPED };
        let tables = cpu.bus.mem.clone();
        let cycles = cpu.cycles;
        assert_eq!(cpu.inspect_write(addr + 1, 0x99, phys), Some(()));
        assert_eq!(cpu.cycles, cycles);
        assert!(cpu.bus.mem == tables);

        cpu.bus.reset_trace();
        assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x5599_7788);
        assert!(cpu.bus.get_trace().is_empty());

        // The value gets to memory with the push of the line
        cpusha(&mut cpu);
        assert_eq!(read_long!(cpu, MAPPED_TO), 0x5599_7788);
    }
}

#[test]
fn write_040_dirty_long_needs_no_memory() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    cpu.write_ticks::<Long>(MAPPED, 0x5566_7788).unwrap();

    cpu.bus.fault_ranges.push(MAPPED_TO..=MAPPED_TO + 0xFFF);
    assert_eq!(cpu.inspect_write(MAPPED, 0x99, false), Some(()));
    cpu.bus.reset_trace();
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x9966_7788);
}

#[test]
fn write_040_clean_long_goes_to_memory_and_stays_clean() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1122_3344);

    assert_eq!(cpu.inspect_write(MAPPED_TO + 1, 0x99, true), Some(()));
    assert_eq!(read_long!(cpu, MAPPED_TO), 0x1199_3344);
    cpu.bus.reset_trace();
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1199_3344);
    assert!(cpu.bus.get_trace().is_empty());

    // Not dirty: a push doesn't write over this
    write_long!(cpu, MAPPED_TO, 0xDEAD_BEEF);
    cpusha(&mut cpu);
    assert_eq!(read_long!(cpu, MAPPED_TO), 0xDEAD_BEEF);
}

#[test]
fn write_040_clean_long_next_to_a_dirty_one() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    write_long!(cpu, MAPPED_TO + 4, 0x1122_3344);
    cpu.write_ticks::<Long>(MAPPED, 0x5566_7788).unwrap();

    // The push of a lone dirty long word doesn't carry its neighbours
    assert_eq!(cpu.inspect_write(MAPPED_TO + 4, 0x99, true), Some(()));
    assert_eq!(read_long!(cpu, MAPPED_TO + 4), 0x9922_3344);
    cpusha(&mut cpu);
    assert_eq!(read_long!(cpu, MAPPED_TO), 0x5566_7788);
    assert_eq!(read_long!(cpu, MAPPED_TO + 4), 0x9922_3344);
}

#[test]
fn write_040_clean_copy_stays_if_memory_refuses() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1122_3344);
    cpu.bus.fault_ranges.push(MAPPED_TO..=MAPPED_TO + 0xFFF);

    assert_no_trace!(cpu, {
        assert_eq!(cpu.inspect_write(MAPPED, 0x99, false), None);
    });
}

#[test]
fn write_040_miss_loads_nothing() {
    let mut cpu = cpu040();
    cpu.regs
        .write(Register::CACR, CACR_040_DATA | CACR_040_INSTR);
    let caches = cpu.caches.clone();
    let cycles = cpu.cycles;
    assert_eq!(cpu.inspect_write(MAPPED, 0x99, false), Some(()));
    assert!(cpu.caches == caches);
    assert_eq!(cpu.cycles, cycles);
    assert_eq!(cpu.bus.mem[&MAPPED_TO], 0x99);
}

#[test]
fn write_040_updates_the_instruction_cache() {
    let mut cpu = cpu040();
    cpu.regs
        .write(Register::CACR, CACR_040_DATA | CACR_040_INSTR);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    assert_eq!(cpu.read_ticks_program::<Long>(MAPPED).unwrap(), 0x1122_3344);

    assert_eq!(cpu.inspect_write(MAPPED_TO, 0x99, true), Some(()));
    assert_eq!(read_long!(cpu, MAPPED_TO), 0x9922_3344);
    cpu.bus.reset_trace();
    assert_eq!(cpu.read_ticks_program::<Long>(MAPPED).unwrap(), 0x9922_3344);
    assert!(cpu.bus.get_trace().is_empty());
}

#[test]
fn write_040_disabled_data_cache_writes_both() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    cpu.write_ticks::<Long>(MAPPED, 0x5566_7788).unwrap();

    // The CPU would read memory now, and push the line later
    cpu.regs.write(Register::CACR, 0_u32);
    assert_eq!(cpu.inspect_write(MAPPED_TO, 0x99, true), Some(()));
    assert_eq!(read_long!(cpu, MAPPED_TO), 0x9922_3344);
    assert_eq!(cpu.inspect_read(MAPPED_TO, true), Some(0x99));
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x9966_7788);
}

#[test]
fn write_040_ignores_write_protection_and_sets_no_bits() {
    let mut cpu = cpu040();
    let desc = PAGE_040 + (MAPPED >> 12) * 4;
    write_long!(cpu, desc, MAPPED_TO | RESIDENT | COPYBACK | (1 << 2));
    assert_eq!(cpu.inspect_write(MAPPED, 0x99, false), Some(()));
    assert_eq!(cpu.bus.mem[&MAPPED_TO], 0x99);
    assert_eq!(
        read_long!(cpu, desc),
        MAPPED_TO | RESIDENT | COPYBACK | (1 << 2)
    );
}

#[test]
fn bulk_040_physical_shows_dirty_data_and_stops_at_holes() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    write_long!(cpu, MAPPED_TO + 4, 0xAABB_CCDD);
    cpu.write_ticks::<Long>(MAPPED, 0x5566_7788).unwrap();
    cpu.bus
        .fault_ranges
        .push(MAPPED_TO + 0x10..=MAPPED_TO + 0x13);

    let mut out = [0; 0x20];
    assert_no_trace!(cpu, {
        let run = cpu.inspect_read_bulk(MAPPED_TO, InspectView::Physical, &mut out);
        assert_eq!(run, InspectRun::Data(0x10));
    });
    assert_eq!(out[..8], [0x55, 0x66, 0x77, 0x88, 0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(
        cpu.inspect_read_bulk(MAPPED_TO + 0x10, InspectView::Physical, &mut out),
        InspectRun::Hole(4)
    );
    // Starting in the middle of a long word in the cache
    assert_eq!(
        cpu.inspect_read_bulk(MAPPED_TO + 2, InspectView::Physical, &mut out[..4]),
        InspectRun::Data(4)
    );
    assert_eq!(out[..4], [0x77, 0x88, 0xAA, 0xBB]);
}

#[test]
fn bulk_040_virtual_ends_at_the_page() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    cpu.write_ticks::<Long>(MAPPED + 0xFFC, 0x5566_7788)
        .unwrap();
    cpu.bus.mem.insert(MAPPED + 0x1000, 0x99);

    let mut out = [0; 0x20];
    assert_no_trace!(cpu, {
        assert_eq!(
            cpu.inspect_read_bulk(MAPPED + 0xFFC, InspectView::Virtual, &mut out),
            InspectRun::Data(4)
        );
        assert_eq!(out[..4], [0x55, 0x66, 0x77, 0x88]);
        assert_eq!(
            cpu.inspect_read_bulk(MAPPED + 0x1000, InspectView::Virtual, &mut out),
            InspectRun::Data(0x20)
        );
        assert_eq!(out[0], 0x99);
        // No translation: to the end of the page, at most
        assert_eq!(
            cpu.inspect_read_bulk(0x0004_0FF0, InspectView::Virtual, &mut out),
            InspectRun::Hole(0x10)
        );
    });
}

#[test]
fn bulk_040_program_sees_the_instruction_cache() {
    let mut cpu = cpu040();
    cpu.regs
        .write(Register::CACR, CACR_040_DATA | CACR_040_INSTR);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    assert_eq!(cpu.read_ticks_program::<Long>(MAPPED).unwrap(), 0x1122_3344);
    // Changed behind the cache
    cpu.write_ticks::<Long>(MAPPED, 0x5566_7788).unwrap();

    let mut out = [0; 4];
    cpu.inspect_read_bulk(MAPPED, InspectView::Program, &mut out);
    assert_eq!(out, [0x11, 0x22, 0x33, 0x44]);
    cpu.inspect_read_bulk(MAPPED, InspectView::Virtual, &mut out);
    assert_eq!(out, [0x55, 0x66, 0x77, 0x88]);
}

#[test]
fn bulk_030_finds_entries_by_view() {
    let mut cpu = cpu030();
    cpu.regs.write(Register::CACR, CACR_030);
    write_long!(cpu, MAPPED_TO, 0x1122_3344);
    write_long!(cpu, MAPPED_TO + 0x100, 0xAABB_CCDD);
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0x1122_3344);
    assert_eq!(cpu.read_ticks_program::<Long>(MAPPED).unwrap(), 0x1122_3344);
    // Behind the back of the caches
    write_long!(cpu, MAPPED_TO, 0x5566_7788);

    let mut out = [0; 8];
    assert_no_trace!(cpu, {
        assert_eq!(
            cpu.inspect_read_bulk(MAPPED_TO, InspectView::Physical, &mut out),
            InspectRun::Data(8)
        );
        assert_eq!(out[..4], [0x11, 0x22, 0x33, 0x44]);
        cpu.inspect_read_bulk(MAPPED, InspectView::Virtual, &mut out);
        assert_eq!(out[..4], [0x11, 0x22, 0x33, 0x44]);
        cpu.inspect_read_bulk(MAPPED, InspectView::Program, &mut out);
        assert_eq!(out[..4], [0x11, 0x22, 0x33, 0x44]);
        // Same line, another page: memory
        cpu.inspect_read_bulk(MAPPED_TO + 0x100, InspectView::Physical, &mut out);
        assert_eq!(out[..4], [0xAA, 0xBB, 0xCC, 0xDD]);
    });

    // Another space misses, as a read would
    cpu.regs.sr.set_supervisor(false);
    cpu.inspect_read_bulk(MAPPED, InspectView::Virtual, &mut out);
    assert_eq!(out[..4], [0x55, 0x66, 0x77, 0x88]);
    // Disabled caches aren't looked at
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.write(Register::CACR, 0_u32);
    cpu.inspect_read_bulk(MAPPED_TO, InspectView::Physical, &mut out);
    assert_eq!(out[..4], [0x55, 0x66, 0x77, 0x88]);
}

#[test]
fn bulk_no_caches_goes_to_the_bus() {
    let mut cpu = CpuM68000::<Testbus<Address, u8, Bus68000>>::new(
        Testbus::new(M68000_ADDRESS_MASK),
        FPU_NONE,
    );
    cpu.bus.mem.insert(0x1234, 0x56);
    let mut out = [0; 2];
    for view in [
        InspectView::Physical,
        InspectView::Virtual,
        InspectView::Program,
    ] {
        assert_eq!(
            cpu.inspect_read_bulk(0x1234, view, &mut out),
            InspectRun::Data(2)
        );
        assert_eq!(out[0], 0x56);
    }
}

#[test]
fn watch_copyback_hit_and_aliases() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    // Nothing watched
    cpu.write_ticks::<Long>(MAPPED, 1).unwrap();
    assert!(!cpu.inspect_take_watch_hit());

    // A copyback hit never reaches the bus
    cpu.inspect_watch(&[(MAPPED_TO, 4, InspectView::Physical)]);
    cpu.write_ticks::<Long>(MAPPED, 2).unwrap();
    assert!(cpu.inspect_take_watch_hit());
    assert!(!cpu.inspect_take_watch_hit());
    assert_eq!(read_long!(cpu, MAPPED_TO), 0);

    // Next to the range, then overlapping it
    cpu.write_ticks::<Long>(MAPPED + 4, 3).unwrap();
    assert!(!cpu.inspect_take_watch_hit());
    cpu.write_ticks::<u16>(MAPPED + 2, 4).unwrap();
    assert!(cpu.inspect_take_watch_hit());

    // Watching by logical address catches a write through another mapping
    cpu.inspect_watch(&[(MAPPED, 4, InspectView::Virtual)]);
    write_long!(cpu, PAGE_040 + 0x20 * 4, MAPPED_TO | RESIDENT);
    cpu.write_ticks::<Long>(0x20000, 5).unwrap();
    assert!(cpu.inspect_take_watch_hit());

    // Host writes too
    assert_eq!(cpu.inspect_write(MAPPED, 6, false), Some(()));
    assert!(cpu.inspect_take_watch_hit());

    cpu.inspect_watch(&[]);
    assert_eq!(cpu.inspect_write(MAPPED, 6, false), Some(()));
    assert!(!cpu.inspect_take_watch_hit());
}

#[test]
fn watch_030_write_and_generation() {
    let mut cpu = cpu030();
    cpu.regs.write(Register::CACR, CACR_030);
    let generation = cpu.mmu_generation();
    cpu.inspect_watch(&[(MAPPED, 8, InspectView::Virtual)]);
    cpu.write_ticks::<Long>(MAPPED + 4, 1).unwrap();
    assert!(cpu.inspect_take_watch_hit());
    cpu.write_ticks::<Long>(MAPPED + 8, 1).unwrap();
    assert!(!cpu.inspect_take_watch_hit());
    // Reads don't count, nor do searching tables or the U bit elsewhere
    assert_eq!(cpu.read_ticks::<Long>(MAPPED).unwrap(), 0);
    assert!(!cpu.inspect_take_watch_hit());
    assert_eq!(cpu.mmu_generation(), generation);

    cpu.pmmu_cache_invalidate();
    assert_ne!(cpu.mmu_generation(), generation);
}

#[test]
fn watch_unmapped_range_watches_nothing() {
    let mut cpu = cpu030();
    cpu.inspect_watch(&[(0x20000, 0x10, InspectView::Virtual)]);
    assert!(cpu.watch_ranges.is_empty());
    assert!(!cpu.watch_active);
}

#[test]
fn watch_spans_pages() {
    let mut cpu = cpu030();
    cpu.inspect_watch(&[(MAPPED - 2, 4, InspectView::Virtual)]);
    assert_eq!(
        cpu.watch_ranges,
        [(MAPPED - 2, MAPPED - 1), (MAPPED_TO, MAPPED_TO + 1)]
    );
}

#[test]
fn watch_cache_invalidate_counts() {
    let mut cpu = cpu040();
    cpu.regs.write(Register::CACR, CACR_040_DATA);
    cpu.inspect_watch(&[(MAPPED_TO, 4, InspectView::Physical)]);
    // CINVA for both caches
    cpu.bus.mem.insert(0x1000, 0xF4);
    cpu.bus.mem.insert(0x1001, 0xD8);
    cpu.set_pc(0x1000).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert!(cpu.inspect_take_watch_hit());
}
