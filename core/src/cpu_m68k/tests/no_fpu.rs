//! Line F exception tests for coprocessor instructions on a 68020 without FPU

use crate::bus::Address;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020, CpuM68020Fpu, M68020_ADDRESS_MASK};

const INITIAL_SSP: Address = 0x1000;
const INITIAL_PC: Address = 0x100;
const HANDLER_ADDR: Address = 0x200;
const VECTOR_LINEF: Address = 0x2C;

fn testbus(code: &[u16]) -> Testbus<Address, u8> {
    let mut bus = Testbus::new(M68020_ADDRESS_MASK);
    for (i, word) in code.iter().enumerate() {
        let addr = INITIAL_PC + (i as u32 * 2);
        bus.mem.insert(addr, (word >> 8) as u8);
        bus.mem.insert(addr + 1, *word as u8);
    }
    for (i, b) in HANDLER_ADDR.to_be_bytes().into_iter().enumerate() {
        bus.mem.insert(VECTOR_LINEF + i as u32, b);
    }
    bus
}

macro_rules! run {
    ($cpu:ty, $code:expr) => {{
        let mut cpu = <$cpu>::new(testbus($code));
        cpu.regs.isp = INITIAL_SSP;
        cpu.regs.sr.set_supervisor(true);
        cpu.regs.sr.set_int_prio_mask(7);
        cpu.set_pc(INITIAL_PC).unwrap();
        cpu.prefetch_refill().unwrap();
        cpu.step().unwrap();
        cpu
    }};
}

#[test]
fn fpu_instructions_without_fpu() {
    #[rustfmt::skip]
    let tests: &[(&str, &[u16])] = &[
        ("FNOP", &[0xF280, 0x0000]),
        ("FMOVE", &[0xF200, 0x0000]),
        ("FSAVE", &[0xF327]),
        ("FRESTORE", &[0xF35F]),
        ("FBcc.W", &[0xF281, 0x0010]),
        ("FBcc.L", &[0xF2C1, 0x0000, 0x0010]),
        ("FScc", &[0xF240, 0x0001]),
        ("FDBcc", &[0xF248, 0x0001, 0x0010]),
        ("FTRAPcc", &[0xF27C, 0x0001]),
    ];

    for (name, code) in tests {
        let cpu = run!(CpuM68020<Testbus<Address, u8>>, code);
        assert_eq!(cpu.regs.pc, HANDLER_ADDR, "{}", name);

        // Format 0 stack frame with the address of the instruction
        let sp = cpu.regs.isp;
        let stacked = |o: u32| *cpu.bus.mem.get(&(sp + o)).unwrap_or(&0) as u32;
        let pc = (stacked(2) << 24) | (stacked(3) << 16) | (stacked(4) << 8) | stacked(5);
        let vector = (stacked(6) << 8) | stacked(7);
        assert_eq!(pc, INITIAL_PC, "{}", name);
        assert_eq!(vector, VECTOR_LINEF, "{}", name);
    }
}

#[test]
fn fnop_with_fpu() {
    let cpu = run!(
        CpuM68020Fpu<Testbus<Address, u8>>,
        &[0xF280, 0x0000, 0x4E71]
    );
    assert_eq!(cpu.regs.pc, INITIAL_PC + 4);
}
