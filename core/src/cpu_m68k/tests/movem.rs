//! MOVEM of the addressing register to its own predecrement (M68000PRM MOVEM)

use crate::bus::Address;
use crate::bus::access::{Bus68000, Bus68020};
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68000, CpuM68020Fpu, M68000_ADDRESS_MASK, M68020_ADDRESS_MASK};

macro_rules! run {
    ($cpu:expr, $code:expr) => {{
        let cpu = $cpu;
        for (i, b) in $code.into_iter().enumerate() {
            cpu.bus.mem.insert(0x1000 + i as Address, b);
        }
        cpu.regs.sr.set_supervisor(true);
        cpu.regs.a[0] = 0x3010;
        cpu.regs.d[0] = 0x1111_2222;
        cpu.set_pc(0x1000).unwrap();
        cpu.prefetch_refill().unwrap();
        cpu.step().unwrap();
        let mem = (0x3008..0x3010)
            .map(|a| cpu.bus.mem[&a])
            .collect::<Vec<u8>>();
        (mem, cpu.regs.a[0])
    }};
}

// MOVEM.L D0/A0,-(A0): A0 goes first, at the higher address
const MOVEM_L: [u8; 4] = [0x48, 0xE0, 0x80, 0x80];

#[test]
fn long_68000_stores_the_initial_value() {
    let mut cpu =
        CpuM68000::<Testbus<Address, u8, Bus68000>>::new(Testbus::new(M68000_ADDRESS_MASK));
    let (mem, a0) = run!(&mut cpu, MOVEM_L);
    assert_eq!(mem, [0x11, 0x11, 0x22, 0x22, 0, 0, 0x30, 0x10]);
    assert_eq!(a0, 0x3008);
}

#[test]
fn long_68020_stores_the_decremented_value() {
    let mut cpu =
        CpuM68020Fpu::<Testbus<Address, u8, Bus68020>>::new(Testbus::new(M68020_ADDRESS_MASK));
    let (mem, a0) = run!(&mut cpu, MOVEM_L);
    assert_eq!(mem, [0x11, 0x11, 0x22, 0x22, 0, 0, 0x30, 0x0C]);
    assert_eq!(a0, 0x3008);
}

#[test]
fn word_68020_stores_the_value_decremented_by_two() {
    let mut cpu =
        CpuM68020Fpu::<Testbus<Address, u8, Bus68020>>::new(Testbus::new(M68020_ADDRESS_MASK));
    for a in 0x3008..0x3010 {
        cpu.bus.mem.insert(a, 0);
    }
    // MOVEM.W D0/A0,-(A0)
    let (mem, a0) = run!(&mut cpu, [0x48, 0xA0, 0x80, 0x80]);
    assert_eq!(mem, [0, 0, 0, 0, 0x22, 0x22, 0x30, 0x0E]);
    assert_eq!(a0, 0x300C);
}
