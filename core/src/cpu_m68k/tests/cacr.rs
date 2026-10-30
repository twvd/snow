//! MOVEC to CACR: which bits read back (MC68020UM 6.2, MC68030UM 6.3.1)

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020Fpu, CpuM68030Fpu, M68020_ADDRESS_MASK, M68030_ADDRESS_MASK};

macro_rules! write_cacr {
    ($cpu:ty, $mask:expr) => {{
        let mut cpu = <$cpu>::new(Testbus::new($mask));
        // MOVEC D0,CACR
        for (i, b) in [0x4E, 0x7B, 0x00, 0x02].into_iter().enumerate() {
            cpu.bus.mem.insert(0x1000 + i as Address, b);
        }
        cpu.regs.sr.set_supervisor(true);
        cpu.regs.d[0] = 0xFFFF_FFFF;
        cpu.set_pc(0x1000).unwrap();
        cpu.prefetch_refill().unwrap();
        cpu.step().unwrap();
        cpu.regs.cacr.0
    }};
}

#[test]
fn clear_bits_read_as_zero_68020() {
    let cacr = write_cacr!(
        CpuM68020Fpu<Testbus<Address, u8, Bus68020>>,
        M68020_ADDRESS_MASK
    );
    assert_eq!(cacr, 0x0003);
}

#[test]
fn clear_bits_read_as_zero_68030() {
    let cacr = write_cacr!(
        CpuM68030Fpu<Testbus<Address, u8, Bus68020>>,
        M68030_ADDRESS_MASK
    );
    assert_eq!(cacr, 0x3313);
}
