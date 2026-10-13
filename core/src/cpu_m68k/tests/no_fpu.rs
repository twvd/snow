//! Tests for coprocessor instructions and the FPU detection on a 68020 without FPU

use crate::bus::Address;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020, CpuM68020Fpu, M68020_ADDRESS_MASK};

const INITIAL_SSP: Address = 0x1000;
const INITIAL_PC: Address = 0x100;
const HANDLER_ADDR: Address = 0x200;
const VECTOR_LINEF: Address = 0x2C;
const BUSERR_HANDLER_ADDR: Address = 0x300;
const VECTOR_BUSERR: Address = 0x08;

fn testbus(code: &[u16]) -> Testbus<Address, u8> {
    let mut bus = Testbus::new(M68020_ADDRESS_MASK);
    for (i, word) in code.iter().enumerate() {
        let addr = INITIAL_PC + (i as u32 * 2);
        bus.mem.insert(addr, (word >> 8) as u8);
        bus.mem.insert(addr + 1, *word as u8);
    }
    for (vector, handler) in [
        (VECTOR_LINEF, HANDLER_ADDR),
        (VECTOR_BUSERR, BUSERR_HANDLER_ADDR),
    ] {
        for (i, b) in handler.to_be_bytes().into_iter().enumerate() {
            bus.mem.insert(vector + i as u32, b);
        }
    }
    bus
}

macro_rules! run {
    ($cpu:ty, $code:expr) => {
        run!($cpu, $code, 1)
    };
    ($cpu:ty, $code:expr, $steps:expr) => {{
        let mut cpu = <$cpu>::new(testbus($code));
        cpu.regs.isp = INITIAL_SSP;
        cpu.regs.sr.set_supervisor(true);
        cpu.regs.sr.set_int_prio_mask(7);
        cpu.set_pc(INITIAL_PC).unwrap();
        cpu.prefetch_refill().unwrap();
        for _ in 0..$steps {
            cpu.step().unwrap();
        }
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

/// MOVES from the response register of the FPU in the CPU space
#[rustfmt::skip]
const FPU_PROBE: &[u16] = &[
    0x7207,                         // MOVEQ #7,D1
    0x4E7B, 0x1000,                 // MOVEC D1,SFC
    0x0E79, 0x1000, 0x0002, 0x2000, // MOVES.W $22000,D1
    0x4E71,
];

#[test]
fn fpu_probe_without_fpu() {
    let cpu = run!(CpuM68020<Testbus<Address, u8>>, FPU_PROBE, 3);
    assert_eq!(cpu.regs.pc, BUSERR_HANDLER_ADDR);
}

#[test]
fn fpu_probe_with_fpu() {
    let cpu = run!(CpuM68020Fpu<Testbus<Address, u8>>, FPU_PROBE, 3);
    assert_eq!(cpu.regs.pc, INITIAL_PC + 14);
    assert_eq!(cpu.regs.d[1] & 0xFFFF, 0x0802);
}
