//! What LINK A7 pushes: the decremented SP on a 68040, the old one before it

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020, CpuM68030, CpuM68040, FPU_M68040, FPU_M68881, FPU_M68882};
use crate::cpu_m68k::{M68020_ADDRESS_MASK, M68030_ADDRESS_MASK, M68040_ADDRESS_MASK};
use crate::types::Word;

const SP: Address = 0x8000;

macro_rules! link_a7 {
    ($cpu:ident, $fpu:expr, $mask:expr, $code:expr) => {{
        let mut cpu = $cpu::<Testbus<Address, u8, Bus68020>>::new(Testbus::new($mask), $fpu);
        let code: &[Word] = $code;
        for (i, w) in code.iter().enumerate() {
            let [h, l] = w.to_be_bytes();
            cpu.bus.mem.insert(0x1000 + 2 * i as Address, h);
            cpu.bus.mem.insert(0x1001 + 2 * i as Address, l);
        }
        cpu.regs.isp = SP;
        cpu.regs.sr.set_supervisor(true);
        cpu.set_pc(0x1000).unwrap();
        cpu.prefetch_refill().unwrap();
        cpu.step().unwrap();
        // The stack pointer has the displacement of -16 added
        assert_eq!(*cpu.regs.ssp(), SP - 4 - 16);
        let byte = |a: Address| Address::from(cpu.bus.mem[&a]);
        (byte(SP - 4) << 24) | (byte(SP - 3) << 16) | (byte(SP - 2) << 8) | byte(SP - 1)
    }};
}

const LINK_W: &[Word] = &[0x4E57, 0xFFF0];
const LINK_L: &[Word] = &[0x480F, 0xFFFF, 0xFFF0];

#[test]
fn m68040_pushes_the_decremented_sp() {
    assert_eq!(
        link_a7!(CpuM68040, FPU_M68040, M68040_ADDRESS_MASK, LINK_W),
        SP - 4
    );
    assert_eq!(
        link_a7!(CpuM68040, FPU_M68040, M68040_ADDRESS_MASK, LINK_L),
        SP - 4
    );
}

#[test]
fn m68030_pushes_the_old_sp() {
    assert_eq!(
        link_a7!(CpuM68030, FPU_M68882, M68030_ADDRESS_MASK, LINK_W),
        SP
    );
    assert_eq!(
        link_a7!(CpuM68030, FPU_M68882, M68030_ADDRESS_MASK, LINK_L),
        SP
    );
}

#[test]
fn m68020_pushes_the_old_sp() {
    assert_eq!(
        link_a7!(CpuM68020, FPU_M68881, M68020_ADDRESS_MASK, LINK_W),
        SP
    );
    assert_eq!(
        link_a7!(CpuM68020, FPU_M68881, M68020_ADDRESS_MASK, LINK_L),
        SP
    );
}
