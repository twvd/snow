//! MMU instructions of the 68030

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::pmmu::regs::{RootPointerReg, TcReg, TrTranslationReg};
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
use crate::tickable::Ticks;
use crate::types::Word;

type TestCpu = CpuM68030Fpu<Testbus<Address, u8, Bus68020>>;

const INITIAL_PC: Address = 0x1000;
const TABLE_A: Address = 0x10000;
const TABLE_B: Address = 0x11000;
/// Supervisor data
const FC: u8 = 5;

fn write_long(cpu: &mut TestCpu, addr: Address, value: u32) {
    for (i, b) in value.to_be_bytes().into_iter().enumerate() {
        cpu.bus.mem.insert(addr + i as Address, b);
    }
}

fn read_long(cpu: &TestCpu, addr: Address) -> u32 {
    u32::from_be_bytes(std::array::from_fn(|i| cpu.bus.mem[&(addr + i as Address)]))
}

/// Address of the page descriptor of `vaddr`
fn page_desc(vaddr: Address) -> Address {
    TABLE_B + (vaddr >> 12) * 4
}

/// A 68030 with 4 KB pages and two levels of short tables, where the first 16 MB
/// map to themselves, running `code`
fn testcpu(code: &[Word]) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68030_ADDRESS_MASK));
    for (i, &word) in code.iter().chain(&[0x4E71, 0x4E71]).enumerate() {
        let addr = INITIAL_PC + i as Address * 2;
        cpu.bus.mem.insert(addr, (word >> 8) as u8);
        cpu.bus.mem.insert(addr + 1, word as u8);
    }

    // Table A entry 0: short table descriptor of table B
    write_long(&mut cpu, TABLE_A, TABLE_B | 0b10);
    // Table B: page descriptors
    for page in 0..0x20 {
        write_long(&mut cpu, page_desc(page << 12), (page << 12) | 0b01);
    }

    // Enabled, 4 KB pages, TIA = TIB = 10
    cpu.regs.pmmu.tc = TcReg(0x80C0_AA00);
    // Upper limit $7FFF, short table descriptors
    cpu.regs.pmmu.crp = RootPointerReg(0x7FFF_0002_0000_0000 | u64::from(TABLE_A));
    cpu.pmmu_cache_ensure();

    cpu.regs.isp = 0x800;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    cpu.set_pc(INITIAL_PC).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu
}

#[test]
fn test_pload_replaces_atc_entry() {
    // PLOADR #5,(A0)
    let mut cpu = testcpu(&[0xF010, 0x2210 | Word::from(FC)]);
    cpu.regs.write_a::<Address>(0, 0x5000);

    // The ATC has the translation of the page
    assert_eq!(cpu.pmmu_translate(FC, 0x5000, false).unwrap(), 0x5000);

    // PLOAD searches the tables again for the new translation, and sets U
    write_long(&mut cpu, page_desc(0x5000), 0x9000 | 0b01);
    cpu.step().unwrap();
    assert_eq!(cpu.pmmu_translate(FC, 0x5000, false).unwrap(), 0x9000);
    assert_ne!(read_long(&cpu, page_desc(0x5000)) & (1 << 3), 0);
}

/// PTESTR #5,(A0),#`level`,A1
fn ptest_extword(level: u8) -> Word {
    0x8000 | Word::from(level) << 10 | 1 << 9 | 1 << 8 | 1 << 5 | 0x10 | Word::from(FC)
}

/// Runs a PTEST at `level` of $5000, with the page descriptor `desc`, and
/// returns the CPU and the clocks it took
fn ptest(level: u8, desc: u32) -> (TestCpu, Ticks) {
    let mut cpu = testcpu(&[0xF010, ptest_extword(level)]);
    cpu.regs.write_a::<Address>(0, 0x5000);
    write_long(&mut cpu, page_desc(0x5000), desc);
    let start = cpu.cycles;
    cpu.step().unwrap();
    let cycles = cpu.cycles - start;
    (cpu, cycles)
}

#[test]
fn test_ptest_levels() {
    // Level 7 goes down to the page descriptor through the two tables
    let (cpu, full) = ptest(7, 0x5000 | 0b01);
    assert!(!cpu.regs.pmmu.psr.invalid());
    assert_eq!(cpu.regs.pmmu.psr.level_number(), 2);
    assert_eq!(cpu.regs.read_a::<Address>(1), page_desc(0x5000));

    // Level 1 stops at the descriptor of table A, one read earlier
    let (cpu, one) = ptest(1, 0x5000 | 0b01);
    assert!(!cpu.regs.pmmu.psr.invalid());
    assert_eq!(cpu.regs.pmmu.psr.level_number(), 1);
    assert_eq!(cpu.regs.read_a::<Address>(1), TABLE_A);
    assert!(one < full);

    // Level 0 only looks in the ATC, which does not have the page yet
    let (mut cpu, zero) = ptest(0, 0x5000 | 0b01);
    assert!(cpu.regs.pmmu.psr.invalid());
    assert_eq!(cpu.regs.pmmu.psr.level_number(), 0);
    assert!(zero < one);

    // Once the ATC has it, with the M bit
    write_long(&mut cpu, page_desc(0x5000), 0x5000 | 1 << 4 | 0b01);
    cpu.pmmu_translate(FC, 0x5000, false).unwrap();
    cpu.set_pc(INITIAL_PC).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert!(!cpu.regs.pmmu.psr.invalid());
    assert!(cpu.regs.pmmu.psr.modified());
}

#[test]
fn test_ptest_status() {
    // An invalid page descriptor, which A1 still points to
    let (cpu, _) = ptest(7, 0);
    assert!(cpu.regs.pmmu.psr.invalid());
    assert_eq!(cpu.regs.pmmu.psr.level_number(), 2);
    assert_eq!(cpu.regs.read_a::<Address>(1), page_desc(0x5000));

    // PTESTR also reports write protection, and the M bit
    let (cpu, _) = ptest(7, 0x5000 | 1 << 4 | 1 << 2 | 0b01);
    assert!(!cpu.regs.pmmu.psr.invalid());
    assert!(cpu.regs.pmmu.psr.write_protected());
    assert!(cpu.regs.pmmu.psr.modified());
}

#[test]
fn test_ptest_transparent() {
    // TT0 maps the first 16 MB for any function code, read or write
    let mut cpu = testcpu(&[0xF010, ptest_extword(0)]);
    cpu.regs.pmmu.tt[0] = TrTranslationReg(0x0000_8107);
    cpu.regs.write_a::<Address>(0, 0x5000);
    cpu.step().unwrap();
    assert!(cpu.regs.pmmu.psr.transparent());
    assert!(!cpu.regs.pmmu.psr.invalid());
}
