//! Tests for the M68040 FPU

use arpfloat::Float;

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::fpu::instruction::opmode;
use crate::cpu_m68k::fpu::regs::{Busy040, Unimp040};
use crate::cpu_m68k::fpu::storage::BitsExtReal;
use crate::cpu_m68k::{CpuM68040Fpu, M68040_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68040Fpu<Testbus<Address, u8, Bus68020>>;

fn ext(v: f64) -> BitsExtReal {
    BitsExtReal::from(&Float::from_f64(v).cast(SEMANTICS_EXTENDED))
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
    cpu.regs.fpu.used = true;
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
        cpu.regs.fpu.fp[2] = BitsExtReal::inf(false);
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
    const INF: [Long; 3] = [0x7FFF_0000, 0, 0];

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

const CODE: Address = 0x1000;
const HANDLER: Address = 0x2000;
const STACK: Address = 0x8000;
const DATA: Address = 0x3000;
const FRAME: Address = 0x4000;
const VECTOR: Address = 0xDC;
const ONE: BitsExtReal = BitsExtReal(0x3FFF_0000_8000_0000_0000_0000);
/// The smallest denormalized number
const TINY: BitsExtReal = BitsExtReal(1);

/// FADD FP1,FP0
const FADD: [Word; 2] = [0xF200, 0x0422];
/// FNOP
const FNOP: [Word; 2] = [0xF280, 0x0000];
/// FSAVE (A1)
const FSAVE: [Word; 1] = [0xF311];
/// FRESTORE (A1)
const FRESTORE: [Word; 1] = [0xF351];

fn cpu_with(code: &[Word]) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68040_ADDRESS_MASK));
    for (i, w) in code.iter().enumerate() {
        put(&mut cpu, CODE + 2 * i as Address, &w.to_be_bytes());
    }
    put(&mut cpu, VECTOR, &HANDLER.to_be_bytes());
    cpu.regs.isp = STACK;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.a[0] = DATA;
    cpu.regs.a[1] = FRAME;
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu
}

fn put(cpu: &mut TestCpu, at: Address, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        cpu.bus.mem.insert(at + i as Address, *b);
    }
}

fn peek(cpu: &TestCpu, at: Address) -> u8 {
    *cpu.bus.mem.get(&at).unwrap_or(&0)
}

fn peek_long(cpu: &TestCpu, at: Address) -> Long {
    (0..4).fold(0, |v, i| (v << 8) | Long::from(peek(cpu, at + i)))
}

/// An unsupported data type: the instruction waits, and the next floating-point instruction
/// reports it, pre-instruction, with its own address in a format $0 frame (MC68040UM 9.6.2)
#[test]
fn unsupported_source_reported_at_the_next_instruction() {
    let mut cpu = cpu_with(&[FADD, FNOP].concat());
    cpu.regs.fpu.fp[0] = ONE;
    cpu.regs.fpu.fp[1] = TINY;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[0], ONE, "not executed");
    assert_eq!(cpu.regs.isp, STACK);

    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(cpu.regs.isp, STACK - 8);
    assert_eq!(peek_long(&cpu, STACK - 6), CODE + 4);
    assert_eq!(peek_long(&cpu, STACK - 2) >> 16, VECTOR);
}

/// FSAVE takes the exception out, in a busy frame, and FRESTORE of the frame with E1 clear
/// has the FPU finish the instruction on what is in it (MC68040UM 9.8, the M68040FPSP)
#[test]
fn unsupported_frame_and_completion() {
    let mut cpu = cpu_with(&[&FADD[..], &FSAVE, &FNOP, &FRESTORE].concat());
    cpu.regs.fpu.fp[0] = ONE;
    cpu.regs.fpu.fp[1] = TINY;
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.isp, STACK, "FSAVE doesn't report it");
    assert_eq!(peek_long(&cpu, FRAME), 0x4160_0000);
    assert_eq!(peek(&cpu, FRAME + 0x3C) >> 5, 0b100, "STAG");
    assert_eq!(peek_long(&cpu, FRAME + 0x40) >> 16, 0x0422, "CMDREG1B");
    assert_eq!(peek(&cpu, FRAME + 0x44) >> 5, 0, "DTAG");
    assert_eq!(peek(&cpu, FRAME + 0x48), 1 << 2, "E1");
    assert_eq!(peek(&cpu, FRAME + 0x49), 0, "T");
    assert_eq!(peek_long(&cpu, FRAME + 0x4C), 0x3FFF_0000, "FPTEMP");
    assert_eq!(peek_long(&cpu, FRAME + 0x60), 1, "ETEMP");

    cpu.step().unwrap();
    assert_eq!(cpu.regs.isp, STACK, "nothing left to report");

    // The handler's work: normalized, with a negative exponent, and E1 clear
    put(&mut cpu, FRAME + 0x3C, &[1 << 4]);
    put(&mut cpu, FRAME + 0x48, &[0]);
    put(&mut cpu, FRAME + 0x08, &[0xFE]);
    put(
        &mut cpu,
        FRAME + 0x58,
        &[0x7F, 0xC1, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0],
    );
    cpu.regs.fpu.fpcr.0 = 0x30;
    cpu.step().unwrap();
    assert_eq!(
        cpu.regs.fpu.fp[0],
        BitsExtReal(0x3FFF_0000_8000_0000_0000_0001),
        "1 + the denormalized number, rounded up by FPCR"
    );
}

/// The instruction goes on with what the handler left in FPSR, and FCMP leaves its
/// destination alone (the M68040FPSP, gen_except)
#[test]
fn unsupported_completion_status_and_fcmp() {
    // FCMP FP1,FP0
    let mut cpu = cpu_with(&[&[0xF200, 0x0438][..], &FSAVE, &FRESTORE].concat());
    let unnormal = BitsExtReal(0x3FFF_0000_4000_0000_0000_0000);
    cpu.regs.fpu.fp[0] = unnormal;
    cpu.regs.fpu.fp[1] = ONE;
    cpu.step().unwrap();
    cpu.step().unwrap();
    // Normalized in the frame, with INEX1 of the handler's doing
    put(&mut cpu, FRAME + 0x48, &[0]);
    put(&mut cpu, FRAME + 0x08, &[0xFE]);
    put(
        &mut cpu,
        FRAME + 0x4C,
        &[0x3F, 0xFE, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0],
    );
    cpu.regs.fpu.fpsr.exs_mut().set_inex1(true);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[0], unnormal);
    assert!(cpu.regs.fpu.fpsr.exs().inex1());
    assert!(cpu.regs.fpu.fpsr.aexc().inex());
}

/// A denormalized single precision source is in the frame as it is in memory, with a tag
/// of its own, and FSQRT has its own code (MC68040UM figure 9-9, 9.8)
#[test]
fn unsupported_frame_contents() {
    // FADD.S (A0),FP0
    let mut cpu = cpu_with(&[&[0xF210, 0x4422][..], &FSAVE].concat());
    put(&mut cpu, DATA, &1u32.to_be_bytes());
    cpu.regs.fpu.fp[0] = ONE;
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(peek(&cpu, FRAME + 0x3C) >> 5, 0b101);
    assert_eq!(peek_long(&cpu, FRAME + 0x58), 0);
    assert_eq!(peek_long(&cpu, FRAME + 0x5C), 1 << 8);

    // FSQRT FP1,FP0
    let mut cpu = cpu_with(&[&[0xF200, 0x0404][..], &FSAVE].concat());
    cpu.regs.fpu.fp[1] = TINY;
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(peek_long(&cpu, FRAME + 0x40) >> 16, 0x0405);
}

/// FMOVE to packed decimal real: at once, post-instruction, with the next instruction and
/// the effective address in a format $3 frame, T set, and no write (MC68040UM 9.6.2,
/// table 9-16)
#[test]
fn unsupported_move_out() {
    // FMOVE.P FP0,(A0){#3}
    let mut cpu = cpu_with(&[&[0xF210, 0x6C03][..], &FSAVE].concat());
    cpu.regs.fpu.fp[0] = ONE;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(cpu.regs.isp, STACK - 12);
    assert_eq!(peek_long(&cpu, STACK - 10), CODE + 4);
    assert_eq!(peek_long(&cpu, STACK - 6) >> 16, 0x3000 | VECTOR);
    assert_eq!(peek_long(&cpu, STACK - 4), DATA);
    assert_eq!(peek_long(&cpu, DATA), 0);

    cpu.set_pc(CODE + 4).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert_eq!(peek(&cpu, FRAME + 0x48), 1 << 2, "E1");
    assert_eq!(peek(&cpu, FRAME + 0x49), 1 << 4, "T");
    assert_eq!(peek_long(&cpu, FRAME + 0x58), 0x3FFF_0000);
}

/// A signaling NAN is flagged before the exception, and stays signaling in the frame, where
/// the handler takes it from (MC68040UM 9.7.2, the M68040FPSP, x_unsupp and res_func)
#[test]
fn unsupported_move_out_snan() {
    let mut cpu = cpu_with(&[&[0xF210, 0x6C03][..], &FSAVE].concat());
    cpu.regs.fpu.fp[0] = BitsExtReal(0x7FFF_0000_A000_0000_0000_0000);
    cpu.step().unwrap();
    assert!(cpu.regs.fpu.fpsr.exs().snan());
    cpu.set_pc(CODE + 4).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert_eq!(peek_long(&cpu, FRAME + 0x5C), 0xA000_0000);
}

const EN_OPERR: Long = 1 << 13;
const EN_OVFL: Long = 1 << 12;
const EN_DZ: Long = 1 << 10;
/// FDIV FP1,FP0
const FDIV: [Word; 2] = [0xF200, 0x0420];
/// FMUL FP1,FP0
const FMUL: [Word; 2] = [0xF200, 0x0423];

/// Arithmetic exceptions take their usual vectors, to the same handler here
fn cpu_with_vectors(code: &[Word]) -> TestCpu {
    let mut cpu = cpu_with(code);
    for vector in (0xC0..=0xD8).step_by(4) {
        put(&mut cpu, vector, &HANDLER.to_be_bytes());
    }
    cpu
}

/// DZ: an E1 exception, pre-instruction at the next floating-point instruction, and the
/// destination isn't modified. The exception stays until the handler clears E1 in the state
/// frame, whatever it restores (MC68040UM 9.7, 9.8).
#[test]
fn enabled_e1_exception() {
    let code = [
        &FDIV[..],
        &FSAVE,
        &FRESTORE,
        &FNOP,
        &FSAVE,
        &FRESTORE,
        &FNOP,
    ]
    .concat();
    let mut cpu = cpu_with_vectors(&code);
    cpu.regs.fpu.fpcr.0 = EN_DZ;
    cpu.regs.fpu.fp[0] = ONE;
    cpu.regs.fpu.fp[1] = BitsExtReal(0);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[0], ONE);
    assert!(cpu.regs.fpu.fpsr.exs().dz());
    assert_eq!(cpu.regs.isp, STACK);

    // FSAVE takes it out and FRESTORE puts it back
    cpu.step().unwrap();
    assert_eq!(peek_long(&cpu, FRAME), 0x4160_0000);
    assert_eq!(peek(&cpu, FRAME + 0x48), 1 << 2, "E1");
    assert_eq!(peek(&cpu, FRAME + 0x49), 0, "T");
    assert_eq!(peek_long(&cpu, FRAME + 0x40) >> 16, 0x0420);
    assert_eq!(peek(&cpu, FRAME + 0x3C) >> 5, 0b001, "STAG");
    assert_eq!(peek_long(&cpu, FRAME + 0x4C), 0x3FFF_0000, "FPTEMP");
    assert_eq!(cpu.regs.fpu.pending040, None);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.pending040, Some(0xC8));

    let fnop = cpu.regs.pc;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(cpu.regs.isp, STACK - 8);
    assert_eq!(peek_long(&cpu, STACK - 6), fnop);
    assert_eq!(peek_long(&cpu, STACK - 2) >> 16, 0xC8);

    // The handler: E1 cleared, then back to the FNOP, which goes through
    cpu.regs.isp = STACK;
    cpu.set_pc(fnop + 4).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    put(&mut cpu, FRAME + 0x48, &[0]);
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.isp, STACK);
    assert_eq!(cpu.regs.fpu.fp[0], ONE, "not an instruction to finish");
}

/// OVFL: an E3 exception, with the destination written, and in the state frame the result
/// before rounding: its exponent out of range, with bit 15 in WB_BYTE (MC68040UM 9.7, 9.8)
#[test]
fn enabled_e3_exception() {
    let mut cpu = cpu_with_vectors(&[&FMUL[..], &FSAVE].concat());
    let big = BitsExtReal(0x7FFE_0000_C000_0000_0000_0001);
    cpu.regs.fpu.fpcr.0 = EN_OVFL;
    cpu.regs.fpu.fp[0] = big;
    cpu.regs.fpu.fp[1] = big;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[0], BitsExtReal::inf(false));
    assert_eq!(cpu.regs.fpu.pending040, Some(0xD4));

    cpu.step().unwrap();
    assert_eq!(peek(&cpu, FRAME + 0x48), 1 << 1, "E3");
    // 1.5^2 x 2^(2 x 16383) = 1.125 x 2^(2 x 16383 + 1): biased, $BFFE in 16 bits
    assert_eq!(peek_long(&cpu, FRAME + 0x18), 0x3FFE_0000, "WBTE");
    assert_eq!(peek_long(&cpu, FRAME + 0x1C), 0x9000_0000, "WBTM");
    assert_eq!(peek(&cpu, FRAME + 0x45) >> 4 & 1, 1, "WBTE15");
    assert_eq!(peek_long(&cpu, FRAME + 0x34) >> 16, 0x0013, "CMDREG3B");
}

/// Figure 9-11: FSGLDIV and FSGLMUL as the M68040FPSP looks for them in CMDREG3B
#[test]
fn cmdreg3b_mapping() {
    assert_eq!(Busy040::cmdreg3b(0x0024) & 0x7F, 0x30);
    assert_eq!(Busy040::cmdreg3b(0x0027) & 0x7F, 0x33);
    assert_eq!(
        Busy040::cmdreg3b(0x0080 | 0x23),
        0x0093,
        "with the register"
    );
}

/// FMOVE to an integer out of range: OPERR, at once, post-instruction, with the source in
/// ETEMP and FPTEMP for the handler, and T set (MC68040UM 9.7.3, table 9-16)
#[test]
fn enabled_move_out_exception() {
    // FMOVE.L FP0,(A0)
    let mut cpu = cpu_with_vectors(&[&[0xF210, 0x6000][..], &FSAVE].concat());
    let huge = BitsExtReal(0x4040_0000_8000_0000_0000_0000);
    cpu.regs.fpu.fpcr.0 = EN_OPERR;
    cpu.regs.fpu.fp[0] = huge;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(cpu.regs.isp, STACK - 12);
    assert_eq!(peek_long(&cpu, STACK - 10), CODE + 4);
    assert_eq!(peek_long(&cpu, STACK - 6) >> 16, 0x3000 | 0xD0);
    assert_eq!(peek_long(&cpu, STACK - 4), DATA);

    cpu.set_pc(CODE + 4).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert_eq!(peek(&cpu, FRAME + 0x48), 1 << 2, "E1");
    assert_eq!(peek(&cpu, FRAME + 0x49), 1 << 4, "T");
    assert_eq!(peek_long(&cpu, FRAME + 0x58), 0x4040_0000, "ETEMP");
    assert_eq!(peek_long(&cpu, FRAME + 0x4C), 0x4040_0000, "FPTEMP");
}

/// BSUN: at the conditional instruction itself, pre-instruction (MC68040UM 9.7.1)
#[test]
fn enabled_bsun() {
    // FBGT.W, branches on the operand being greater, an IEEE-aware predicate that's false
    // and sets BSUN with a NAN
    let mut cpu = cpu_with_vectors(&[0xF292, 0x0010, 0x4E71]);
    cpu.regs.fpu.fpcr.0 = 1 << 15;
    cpu.regs.fpu.fpsr.set_fpcc_nan(true);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(peek_long(&cpu, STACK - 6), CODE);
    assert_eq!(peek_long(&cpu, STACK - 2) >> 16, 0xC0);
}

/// A frame of an FMOVE to memory, restored with its exception, is reported with the address
/// of the FMOVE, which is where the M68040FPSP writes the result (x_store, gen_except)
#[test]
fn restored_move_out_exception_has_the_address() {
    // FMOVE.P FP0,(A0){#3}; FSAVE (A1); FRESTORE (A1); FNOP
    let code = [&[0xF210, 0x6C03][..], &FSAVE, &FRESTORE, &FNOP].concat();
    let mut cpu = cpu_with_vectors(&code);
    cpu.regs.fpu.fp[0] = ONE;
    cpu.step().unwrap();
    cpu.regs.isp = STACK;
    cpu.set_pc(CODE + 4).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    // As the handler leaves it, for an underflow it found
    cpu.regs.fpu.fpcr.0 = 1 << 11;
    cpu.regs.fpu.fpsr.exs_mut().set_unfl(true);
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(cpu.regs.isp, STACK - 12);
    assert_eq!(peek_long(&cpu, STACK - 10), CODE + 8, "the FNOP again");
    assert_eq!(peek_long(&cpu, STACK - 6) >> 16, 0x3000 | 0xCC);
    assert_eq!(peek_long(&cpu, STACK - 4), DATA);
}
