//! Tests for the M68040 MMU

use crate::bus::Address;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::regs::Register;
use crate::cpu_m68k::{CpuM68040Fpu, M68040_ADDRESS_MASK};
use crate::types::{Field32, Long};

type TestCpu = CpuM68040Fpu<Testbus<Address, u8>>;

// Physical addresses of the three table levels
const ROOT_TABLE: Address = 0x0001_0000;
const PTR_TABLE: Address = 0x0001_1000;
const PAGE_TABLE: Address = 0x0001_2000;

// Function codes
const FC_SUPER_DATA: u8 = 5;
const FC_USER_DATA: u8 = 1;
const FC_SUPER_PROGRAM: u8 = 6;

// Descriptor bits
const RESIDENT: Long = 1;
const TABLE_VALID: Long = 2;
const WP: Long = 1 << 2;
const SUPERVISOR: Long = 1 << 7;

fn write_long(cpu: &mut TestCpu, addr: Address, value: Long) {
    for i in 0..4 {
        cpu.bus.mem.insert(addr + i, (value >> (24 - i * 8)) as u8);
    }
}

fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    let byte = |offset: Address| *cpu.bus.mem.get(&(addr + offset)).unwrap_or(&0);

    let mut value = Field32(0);
    value.set_be0(byte(0));
    value.set_be1(byte(1));
    value.set_be2(byte(2));
    value.set_be3(byte(3));
    value.0
}

/// Logical address used by the tests: root index 8, pointer index 33, page index 1
const LOGICAL: Address = 0x1084_2000;

fn testcpu(leaf_flags: Long) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68040_ADDRESS_MASK));

    // Create a page table that points LOGICAL to 0x200000
    write_long(&mut cpu, ROOT_TABLE + 8 * 4, PTR_TABLE | TABLE_VALID);
    write_long(&mut cpu, PTR_TABLE + 33 * 4, PAGE_TABLE | TABLE_VALID);
    write_long(
        &mut cpu,
        PAGE_TABLE + 4,
        0x0020_0000 | RESIDENT | leaf_flags,
    );

    cpu.regs.write(Register::SRP040, ROOT_TABLE);
    cpu.regs.write(Register::URP040, ROOT_TABLE);
    // Enable, 8KB pages
    cpu.regs.write(Register::TC040, 0xC000_u32);
    cpu.pmmu_cache_ensure();
    cpu
}

#[test]
fn translate_page() {
    let mut cpu = testcpu(0);

    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap(),
        0x0020_0000
    );
    // Offset within the page is preserved
    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL + 0x1ABC, false)
            .unwrap(),
        0x0020_1ABC
    );
    // next page is not mapped
    assert!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL + 0x2000, false)
            .is_err()
    );
}

#[test]
fn translate_disabled_is_identity() {
    let mut cpu = testcpu(0);
    cpu.regs.write(Register::TC040, 0_u32);

    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap(),
        LOGICAL
    );
}

#[test]
fn walk_sets_used_and_modified() {
    let mut cpu = testcpu(0);

    cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap();
    assert_eq!(
        read_long(&cpu, ROOT_TABLE + 8 * 4) & (1 << 3),
        1 << 3,
        "root U"
    );
    assert_eq!(
        read_long(&cpu, PTR_TABLE + 33 * 4) & (1 << 3),
        1 << 3,
        "pointer U"
    );
    assert_eq!(read_long(&cpu, PAGE_TABLE + 4) & (1 << 3), 1 << 3, "page U");
    assert_eq!(
        read_long(&cpu, PAGE_TABLE + 4) & (1 << 4),
        0,
        "M not set on read"
    );

    // Test M-bit on leaf at write
    cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).unwrap();
    assert_eq!(
        read_long(&cpu, PAGE_TABLE + 4) & (1 << 4),
        1 << 4,
        "M set on write"
    );
}

#[test]
fn write_protect() {
    // On the leaf
    let mut cpu = testcpu(WP);
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_ok());
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).is_err());

    // Inherited from a table descriptor higher up
    let mut cpu = testcpu(0);
    write_long(&mut cpu, PTR_TABLE + 33 * 4, PAGE_TABLE | TABLE_VALID | WP);
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_ok());
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).is_err());
}

#[test]
fn supervisor_only_page() {
    let mut cpu = testcpu(SUPERVISOR);

    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_ok());
    assert!(cpu.pmmu_translate(FC_USER_DATA, LOGICAL, false).is_err());
}

#[test]
fn indirect_descriptor() {
    let mut cpu = testcpu(0);
    // Leaf points at another descriptor at $13000
    write_long(&mut cpu, PAGE_TABLE + 4, 0x0001_3000 | 2);
    write_long(&mut cpu, 0x0001_3000, 0x0030_0000 | RESIDENT);

    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap(),
        0x0030_0000
    );

    // Two levels of indirection are invalid
    let mut cpu = testcpu(0);
    write_long(&mut cpu, PAGE_TABLE + 4, 0x0001_3000 | 2);
    write_long(&mut cpu, 0x0001_3000, 0x0001_4000 | 2);
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_err());
}

#[test]
fn transparent_translation() {
    let mut cpu = testcpu(0);
    // Data TT: $F9000000-$F9FFFFFF, both user and supervisor
    cpu.regs.write(Register::DTT0, 0xF900_C060_u32);

    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, 0xF900_1234, false)
            .unwrap(),
        0xF900_1234
    );
    // Program access not translated (because DTT0)
    assert!(
        cpu.pmmu_translate(FC_SUPER_PROGRAM, 0xF900_1234, false)
            .is_err()
    );
}

#[test]
fn transparent_translation_write_protect() {
    let mut cpu = testcpu(0);
    cpu.regs.write(Register::DTT0, 0xF900_C064_u32);

    assert!(
        cpu.pmmu_translate(FC_SUPER_DATA, 0xF900_1234, false)
            .is_ok()
    );
    assert!(
        cpu.pmmu_translate(FC_SUPER_DATA, 0xF900_1234, true)
            .is_err()
    );
}

#[test]
fn separate_root_pointers() {
    let mut cpu = testcpu(0);
    // Valid URP, but empty table
    cpu.regs.write(Register::URP040, 0x0002_0000_u32);
    cpu.pmmu_cache_invalidate();

    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_ok());
    assert!(cpu.pmmu_translate(FC_USER_DATA, LOGICAL, false).is_err());
}
