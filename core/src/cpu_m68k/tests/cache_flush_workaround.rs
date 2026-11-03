//! The cache flush workaround: CPUSH of the 68040 data cache also
//! invalidates the instruction cache

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::regs::Register;
use crate::cpu_m68k::{CpuM68040, FPU_M68040, M68040_ADDRESS_MASK};
use crate::types::Word;

type Cpu040 = CpuM68040<Testbus<Address, u8, Bus68020>>;

/// Code that gets replaced
const CODE: Address = 0x2000;
/// MOVEQ #1,D0
const MOVEQ_1: Word = 0x7001;
/// MOVEQ #2,D0
const MOVEQ_2: Word = 0x7002;
/// CPUSHA DC
const CPUSHA_DC: Word = 0xF478;

fn run_at(cpu: &mut Cpu040, pc: Address) {
    cpu.set_pc(pc).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
}

/// Runs MOVEQ #1,D0 from the instruction cache, replaces it with MOVEQ #2,D0
/// by a data write, flushes the data cache and runs it again: returns D0
fn run_replaced_code(workaround: bool, reset: bool) -> u32 {
    let mut cpu = Cpu040::new(Testbus::new(M68040_ADDRESS_MASK), FPU_M68040);
    cpu.set_cache_flush_workaround(workaround);
    if reset {
        cpu.reset().unwrap();
    }
    cpu.regs.sr.set_supervisor(true);
    // Both caches on, no translation: memory is cached write-through
    cpu.regs.write(Register::CACR, 0x8000_8000_u32);
    for (i, b) in MOVEQ_1.to_be_bytes().into_iter().enumerate() {
        cpu.bus.mem.insert(CODE + i as Address, b);
    }
    for (i, b) in CPUSHA_DC.to_be_bytes().into_iter().enumerate() {
        cpu.bus.mem.insert(0x1000 + i as Address, b);
    }

    run_at(&mut cpu, CODE);
    assert_eq!(cpu.regs.d[0], 1);
    cpu.write_ticks::<Word>(CODE, MOVEQ_2).unwrap();
    run_at(&mut cpu, 0x1000);
    run_at(&mut cpu, CODE);
    cpu.regs.d[0]
}

#[test]
fn data_cache_push_leaves_the_instruction_cache() {
    assert_eq!(run_replaced_code(false, false), 1);
}

#[test]
fn data_cache_push_with_the_workaround_clears_it() {
    assert_eq!(run_replaced_code(true, false), 2);
}

#[test]
fn workaround_stays_after_a_reset() {
    assert_eq!(run_replaced_code(true, true), 2);
}
