//! Tests for the M68040 FPU

use arpfloat::Float;

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::fpu::instruction::opmode;
use crate::cpu_m68k::fpu::regs::Unimp040;
use crate::cpu_m68k::{CpuM68040Fpu, M68040_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68040Fpu<Testbus<Address, u8, Bus68020>>;

fn ext(v: f64) -> Float {
    Float::from_f64(v).cast(SEMANTICS_EXTENDED)
}

#[test]
fn rounded_add() {
    let mut cpu = TestCpu::new(Testbus::new(M68040_ADDRESS_MASK));
    // Exact in double precision, not in single
    let small = ext(2f64.powi(-30));
    let one = ext(1.0);

    let r = cpu.fpu_alu_op(opmode::FSADD, &small, &one).unwrap();
    assert_eq!(r.as_f64(), 1.0);
    let r = cpu.fpu_alu_op(opmode::FDADD, &small, &one).unwrap();
    assert_eq!(r.as_f64(), 1.0 + 2f64.powi(-30));
    let r = cpu.fpu_alu_op(opmode::FADD, &small, &one).unwrap();
    assert_eq!(r.as_f64(), 1.0 + 2f64.powi(-30));
}

/// FSIN FP1,FP2, with 1.0 in FP1 and infinity in FP2
const UNIMP: Unimp040 = Unimp040 {
    cmdreg1b: 0x050E,
    etemp: [0x3FFF_0000, 0x8000_0000, 0],
    stag: 0,
    fptemp: [0x7FFF_0000, 0, 0],
    dtag: 2,
    packed: false,
};

#[test]
fn unimp_frame() {
    let frame = UNIMP.to_frame();
    let longs: Vec<Long> = frame
        .chunks_exact(4)
        .map(|b| Long::from_be_bytes(b.try_into().unwrap()))
        .collect();
    assert_eq!(
        longs,
        [
            0x4130_0000,
            0,
            0,
            // STAG
            0,
            // CMDREG1B
            0x050E_0000,
            // DTAG
            0x4000_0000,
            // E1 clear: the source is not packed decimal
            0,
            // FPTEMP
            0x7FFF_0000,
            0,
            0,
            // ETEMP
            0x3FFF_0000,
            0x8000_0000,
            0,
        ]
    );
    assert_eq!(Unimp040::from_frame(&frame), UNIMP);

    // E1 for a packed decimal source
    let packed = Unimp040 {
        packed: true,
        ..UNIMP
    };
    let frame = packed.to_frame();
    assert_eq!(frame[0x18..0x1C], [0x04, 0, 0, 0]);
    assert_eq!(Unimp040::from_frame(&frame), packed);
}

#[test]
fn unimp_fsave_frestore() {
    const CODE: Address = 0x1000;
    const STACK: Address = 0x8000;
    #[rustfmt::skip]
    let code: [Word; 4] = [
        0xF327, // FSAVE -(A7)
        0xF327, // FSAVE -(A7)
        0xF35F, // FRESTORE (A7)+
        0xF35F, // FRESTORE (A7)+
    ];
    let mut cpu = TestCpu::new(Testbus::new(M68040_ADDRESS_MASK));
    for (i, w) in code.iter().enumerate() {
        let addr = CODE + 2 * i as Address;
        cpu.bus.mem.insert(addr, (w >> 8) as u8);
        cpu.bus.mem.insert(addr + 1, *w as u8);
    }
    cpu.regs.isp = STACK;
    cpu.regs.sr.set_supervisor(true);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.regs.fpu.unimp040 = Some(UNIMP);

    // The unimplemented instruction state frame, then the FPU is idle
    cpu.step().unwrap();
    let frame_addr = STACK - Unimp040::FRAME_SIZE as Address;
    assert_eq!(cpu.regs.isp, frame_addr);
    let frame: Vec<u8> = (0..Unimp040::FRAME_SIZE as Address)
        .map(|i| cpu.bus.mem[&(frame_addr + i)])
        .collect();
    assert_eq!(frame, UNIMP.to_frame());
    assert_eq!(cpu.regs.fpu.unimp040, None);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.isp, frame_addr - 4);
    let idle: Vec<u8> = (0..4).map(|i| cpu.bus.mem[&(frame_addr - 4 + i)]).collect();
    assert_eq!(idle, [0x41, 0, 0, 0]);

    // Restoring the idle frame, then the unimplemented instruction one
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.unimp040, None);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.isp, STACK);
    assert_eq!(cpu.regs.fpu.unimp040, Some(UNIMP));
}

mod unimplemented {
    use super::*;
    use crate::cpu_m68k::fpu::regs::Unimp040;
    use crate::types::{Long, Word};

    const CODE: Address = 0x1000;
    const HANDLER: Address = 0x2000;
    const STACK: Address = 0x8000;
    const OPERAND: Address = 0x3000;

    fn write_word(cpu: &mut TestCpu, addr: Address, value: Word) {
        cpu.bus.mem.insert(addr, (value >> 8) as u8);
        cpu.bus.mem.insert(addr + 1, value as u8);
    }

    fn write_long(cpu: &mut TestCpu, addr: Address, value: Long) {
        write_word(cpu, addr, (value >> 16) as Word);
        write_word(cpu, addr + 2, value as Word);
    }

    fn read_word(cpu: &TestCpu, addr: Address) -> Word {
        (Word::from(cpu.bus.mem[&addr]) << 8) | Word::from(cpu.bus.mem[&(addr + 1)])
    }

    fn read_long(cpu: &TestCpu, addr: Address) -> Long {
        (Long::from(read_word(cpu, addr)) << 16) | Long::from(read_word(cpu, addr + 2))
    }

    /// Runs the first instruction of `code`, with 1.0 in FP1 and infinity
    /// in FP2
    fn run(code: &[Word], setup: impl Fn(&mut TestCpu)) -> TestCpu {
        let mut cpu = TestCpu::new(Testbus::new(M68040_ADDRESS_MASK));
        for (i, &w) in code.iter().enumerate() {
            write_word(&mut cpu, CODE + 2 * i as Address, w);
        }
        write_long(&mut cpu, 0x2C, HANDLER);
        cpu.regs.isp = STACK;
        cpu.regs.sr.set_supervisor(true);
        cpu.regs.fpu.fp[1] = ext(1.0);
        cpu.regs.fpu.fp[2] = Float::inf(SEMANTICS_EXTENDED, false);
        setup(&mut cpu);
        cpu.set_pc(CODE).unwrap();
        cpu.prefetch_refill().unwrap();
        cpu.step().unwrap();
        cpu
    }

    /// Checks the format $2 frame of the exception
    fn check_frame(cpu: &TestCpu, next_pc: Address, address: Address) {
        assert_eq!(cpu.regs.pc, HANDLER);
        let sp = STACK - 12;
        assert_eq!(cpu.regs.isp, sp);
        assert_eq!(read_long(cpu, sp + 2), next_pc);
        assert_eq!(read_word(cpu, sp + 6), 0x202C);
        assert_eq!(read_long(cpu, sp + 8), address);
    }

    const ONE: [Long; 3] = [0x3FFF_0000, 0x8000_0000, 0];
    // Snow writes infinity with the explicit integer bit set
    const INF: [Long; 3] = [0x7FFF_0000, 0x8000_0000, 0];

    #[test]
    fn register_source() {
        // FSIN FP1,FP2
        let cpu = run(&[0xF200, 0x050E], |_| ());
        check_frame(&cpu, CODE + 4, 0);
        assert_eq!(
            cpu.regs.fpu.unimp040,
            Some(Unimp040 {
                cmdreg1b: 0x050E,
                etemp: ONE,
                stag: 0,
                fptemp: INF,
                dtag: 2,
                packed: false,
            })
        );
        assert!(cpu.regs.fpu.fp[2].is_inf());
    }

    #[test]
    fn memory_source() {
        // FSIN.X (A0)+,FP2
        let cpu = run(&[0xF218, 0x490E], |cpu| {
            cpu.regs.write_a(0, OPERAND);
            write_long(cpu, OPERAND, 0x4000_0000);
            write_long(cpu, OPERAND + 4, 0x8000_0000);
            write_long(cpu, OPERAND + 8, 0);
        });
        check_frame(&cpu, CODE + 4, OPERAND);
        assert_eq!(cpu.regs.read_a::<Address>(0), OPERAND + 12);
        let unimp = cpu.regs.fpu.unimp040.unwrap();
        assert_eq!(unimp.cmdreg1b, 0x490E);
        assert_eq!(unimp.etemp, [0x4000_0000, 0x8000_0000, 0]);
        assert_eq!(unimp.fptemp, INF);
    }

    #[test]
    fn packed_source() {
        // FSIN.P (A0),FP2
        let cpu = run(&[0xF210, 0x4D0E], |cpu| {
            cpu.regs.write_a(0, OPERAND);
            write_long(cpu, OPERAND, 0x0000_0001);
            write_long(cpu, OPERAND + 4, 0x2345_6789);
            write_long(cpu, OPERAND + 8, 0xABCD_EF01);
        });
        check_frame(&cpu, CODE + 4, OPERAND);
        let unimp = cpu.regs.fpu.unimp040.unwrap();
        assert!(unimp.packed);
        assert_eq!(unimp.etemp, [0, 0x2345_6789, 0xABCD_EF01]);
        assert_eq!(unimp.fptemp, [0, 0, 0x0000_0001]);
        assert_eq!(unimp.dtag, 2);
    }

    #[test]
    fn fmovecr() {
        // FMOVECR #$00,FP2
        let cpu = run(&[0xF200, 0x5D00], |_| ());
        check_frame(&cpu, CODE + 4, 0);
        assert_eq!(cpu.regs.fpu.unimp040.unwrap().cmdreg1b, 0x5D00);
        assert!(cpu.regs.fpu.fp[2].is_inf());
    }

    #[test]
    fn implemented() {
        // FADD FP1,FP0 runs in hardware
        let cpu = run(&[0xF200, 0x0422], |cpu| cpu.regs.fpu.fp[0] = ext(2.0));
        assert_eq!(cpu.regs.pc, CODE + 4);
        assert_eq!(cpu.regs.fpu.unimp040, None);
        assert_eq!(cpu.regs.fpu.fp[0].as_f64(), 3.0);
    }
}
