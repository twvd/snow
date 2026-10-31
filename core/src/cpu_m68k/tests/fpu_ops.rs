//! 6888x behaviour from the MC68881/MC68882 User's Manual

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68030Fpu<Testbus<Address, u8, Bus68020>>;

const CODE: Address = 0x1000;

fn testcpu(code: &[Word]) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68030_ADDRESS_MASK));
    for (i, w) in code.iter().enumerate() {
        let [h, l] = w.to_be_bytes();
        cpu.bus.mem.insert(CODE + 2 * i as Address, h);
        cpu.bus.mem.insert(CODE + 2 * i as Address + 1, l);
    }
    // Privilege violation
    cpu.bus.mem.insert(0x22, 0x04);
    cpu.regs.isp = 0x8000;
    cpu.regs.usp = 0x7000;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.a[0] = 0x3000;
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu
}

fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    let b = |a: Address| Long::from(*cpu.bus.mem.get(&a).unwrap_or(&0));
    (b(addr) << 24) | (b(addr + 1) << 16) | (b(addr + 2) << 8) | b(addr + 3)
}

#[test]
fn fmovem_control_to_memory_ascends() {
    // FMOVEM.L FPCR/FPSR,(A0)
    let mut cpu = testcpu(&[0xF210, 0xB800]);
    cpu.regs.fpu.fpcr.0 = 0x10;
    cpu.regs.fpu.fpsr.0 = 0x0800_0000;
    cpu.step().unwrap();
    assert_eq!(read_long(&cpu, 0x3000), 0x10);
    assert_eq!(read_long(&cpu, 0x3004), 0x0800_0000);
    assert_eq!(cpu.regs.a[0], 0x3000);
}

#[test]
fn fmovem_control_predecrement_has_the_same_layout() {
    // FMOVEM.L FPCR/FPSR/FPIAR,-(A0)
    let mut cpu = testcpu(&[0xF220, 0xBC00]);
    cpu.regs.fpu.fpcr.0 = 0x10;
    cpu.regs.fpu.fpsr.0 = 0x0800_0000;
    cpu.regs.fpu.fpiar = 0x1234;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.a[0], 0x3000 - 12);
    assert_eq!(read_long(&cpu, 0x3000 - 12), 0x10);
    assert_eq!(read_long(&cpu, 0x3000 - 8), 0x0800_0000);
    assert_eq!(read_long(&cpu, 0x3000 - 4), 0x1234);
}

#[test]
fn fmovem_control_save_and_restore() {
    // FMOVEM.L FPCR/FPSR,-(A0) ; FMOVEM.L (A0)+,FPCR/FPSR
    let mut cpu = testcpu(&[0xF220, 0xB800, 0xF218, 0x9800]);
    cpu.regs.fpu.fpcr.0 = 0x10;
    cpu.regs.fpu.fpsr.0 = 0x0800_0000;
    cpu.step().unwrap();
    cpu.regs.fpu.fpcr.0 = 0;
    cpu.regs.fpu.fpsr.0 = 0;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fpcr.0, 0x10);
    assert_eq!(cpu.regs.fpu.fpsr.0, 0x0800_0000);
    assert_eq!(cpu.regs.a[0], 0x3000);
}
