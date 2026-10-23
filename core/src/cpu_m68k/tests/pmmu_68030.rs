//! MMU instructions of the 68030

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::pmmu::regs::{RootPointerReg, TcReg};
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
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
