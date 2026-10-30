//! MULS.L/MULU.L condition codes (M68000PRM MULS, MULU)

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020Fpu, M68020_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68020Fpu<Testbus<Address, u8, Bus68020>>;

/// MULx.L D1,<dh:>D0 and returns (D0, D2, NZVC)
fn mull(extword: Word, d0: Long, d1: Long) -> (Long, Long, (bool, bool, bool, bool)) {
    let mut cpu = TestCpu::new(Testbus::new(M68020_ADDRESS_MASK));
    for (i, w) in [0x4C01, extword].into_iter().enumerate() {
        let [h, l] = w.to_be_bytes();
        cpu.bus.mem.insert(0x1000 + 2 * i as Address, h);
        cpu.bus.mem.insert(0x1001 + 2 * i as Address, l);
    }
    cpu.regs.write_d::<Long>(0, d0);
    cpu.regs.write_d::<Long>(1, d1);
    cpu.set_pc(0x1000).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    let sr = cpu.regs.sr;
    (
        cpu.regs.read_d(0),
        cpu.regs.read_d(2),
        (sr.n(), sr.z(), sr.v(), sr.c()),
    )
}

const MULU_32: Word = 0x0000;
const MULS_32: Word = 0x0800;
const MULU_64: Word = 0x0402;
const MULS_64: Word = 0x0C02;

#[test]
fn mulu_32() {
    assert_eq!(mull(MULU_32, 3, 5).0, 15);
    assert_eq!(mull(MULU_32, 3, 5).2, (false, false, false, false));
    // Low 32 bits are zero: Z set from them, and V
    assert_eq!(mull(MULU_32, 0x10000, 0x10000).0, 0);
    assert_eq!(
        mull(MULU_32, 0x10000, 0x10000).2,
        (false, true, true, false)
    );
    // N from bit 31 of the result, which fits
    assert_eq!(mull(MULU_32, 0x8000_0000, 1).2, (true, false, false, false));
}

#[test]
fn muls_32() {
    assert_eq!(mull(MULS_32, 0xFFFF_FFFF, 5).0, 0xFFFF_FFFB);
    assert_eq!(mull(MULS_32, 0xFFFF_FFFF, 5).2, (true, false, false, false));
    // 2^31 does not fit in a signed long word, its low part looks negative
    assert_eq!(mull(MULS_32, 0x10000, 0x8000).2, (true, false, true, false));
    assert_eq!(
        mull(MULS_32, 0x10000, 0x10000).2,
        (false, true, true, false)
    );
}

#[test]
fn mul_64() {
    assert_eq!(
        mull(MULU_64, 0x10000, 0x10000),
        (0, 1, (false, false, false, false))
    );
    assert_eq!(
        mull(MULS_64, 0xFFFF_FFFF, 1),
        (0xFFFF_FFFF, 0xFFFF_FFFF, (true, false, false, false))
    );
    assert_eq!(mull(MULU_64, 0, 5), (0, 0, (false, true, false, false)));
}
