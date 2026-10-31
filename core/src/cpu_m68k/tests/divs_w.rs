//! DIVS.W of the one dividend whose quotient overflows the host division too

use crate::bus::Address;
use crate::bus::access::{Bus68000, Bus68020};
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68000, CpuM68020Fpu, M68000_ADDRESS_MASK, M68020_ADDRESS_MASK};

macro_rules! check {
    ($cpu:expr) => {{
        let cpu = $cpu;
        // DIVS.W #-1,D0
        for (i, b) in [0x81, 0xFC, 0xFF, 0xFF].into_iter().enumerate() {
            cpu.bus.mem.insert(0x1000 + i as Address, b);
        }
        cpu.regs.d[0] = 0x8000_0000;
        cpu.set_pc(0x1000).unwrap();
        cpu.prefetch_refill().unwrap();
        cpu.step().unwrap();
        assert!(cpu.regs.sr.v());
        assert_eq!(cpu.regs.d[0], 0x8000_0000);
    }};
}

#[test]
fn min_by_minus_one_overflows_68000() {
    let mut cpu =
        CpuM68000::<Testbus<Address, u8, Bus68000>>::new(Testbus::new(M68000_ADDRESS_MASK));
    check!(&mut cpu);
}

#[test]
fn min_by_minus_one_overflows_68020() {
    let mut cpu =
        CpuM68020Fpu::<Testbus<Address, u8, Bus68020>>::new(Testbus::new(M68020_ADDRESS_MASK));
    check!(&mut cpu);
}
