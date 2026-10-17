//! MOVEC control registers per CPU

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68030Fpu, CpuM68040Fpu, M68030_ADDRESS_MASK, M68040_ADDRESS_MASK};
use crate::types::Word;

const CODE: Address = 0x1000;
const ILLEGAL_HANDLER: Address = 0x2000;

/// Runs MOVEC D0,<reg> then MOVEC <reg>,D1 and tells whether the register
/// exists: the value comes back, or the first MOVEC is an illegal instruction.
macro_rules! movec_exists {
    ($cpu:ty, $mask:expr, $reg:expr) => {{
        let mut cpu = <$cpu>::new(Testbus::new($mask));
        let code: [Word; 4] = [0x4E7B, $reg, 0x4E7A, 0x1000 | $reg];
        for (i, w) in code.iter().enumerate() {
            let addr = CODE + 2 * i as Address;
            cpu.bus.mem.insert(addr, (w >> 8) as u8);
            cpu.bus.mem.insert(addr + 1, *w as u8);
        }
        for (i, b) in ILLEGAL_HANDLER.to_be_bytes().iter().enumerate() {
            cpu.bus.mem.insert(0x10 + i as Address, *b);
        }
        cpu.regs.isp = 0x8000;
        cpu.regs.sr.set_supervisor(true);
        cpu.regs.write_d(0, 0x8000_0000_u32);
        cpu.set_pc(CODE).unwrap();
        cpu.prefetch_refill().unwrap();

        cpu.step().unwrap();
        if cpu.regs.pc == ILLEGAL_HANDLER {
            // The frame points at the MOVEC
            assert_eq!(cpu.bus.mem[&(0x8000 - 8 + 5)], CODE as u8);
            false
        } else {
            cpu.step().unwrap();
            assert_eq!(cpu.regs.pc, CODE + 8);
            true
        }
    }};
}

fn exists_030(reg: Word) -> bool {
    movec_exists!(
        CpuM68030Fpu<Testbus<Address, u8, Bus68020>>,
        M68030_ADDRESS_MASK,
        reg
    )
}

fn exists_040(reg: Word) -> bool {
    movec_exists!(
        CpuM68040Fpu<Testbus<Address, u8, Bus68020>>,
        M68040_ADDRESS_MASK,
        reg
    )
}

#[test]
fn m68030_registers() {
    for reg in [0x000, 0x001, 0x002, 0x800, 0x801, 0x802, 0x803, 0x804] {
        assert!(exists_030(reg), "{:03X}", reg);
    }
    // The 68040 MMU registers and undefined codes are illegal
    for reg in [
        0x003, 0x004, 0x005, 0x006, 0x007, 0x805, 0x806, 0x807, 0x008, 0x808,
    ] {
        assert!(!exists_030(reg), "{:03X}", reg);
    }
}

#[test]
fn m68040_registers() {
    for reg in [
        0x000, 0x001, 0x002, 0x003, 0x004, 0x005, 0x006, 0x007, 0x800, 0x801, 0x803, 0x804, 0x805,
        0x806, 0x807,
    ] {
        assert!(exists_040(reg), "{:03X}", reg);
    }
    // No CAAR on the 68040
    for reg in [0x802, 0x008, 0x808] {
        assert!(!exists_040(reg), "{:03X}", reg);
    }
}
