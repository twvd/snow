//! Enabled floating-point exceptions on the MC68881 and MC68882, from the MC68881/MC68882
//! User's Manual, section 6

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::fpu::storage::BitsExtReal;
use crate::cpu_m68k::{CpuM68020Fpu, CpuM68030Fpu, M68020_ADDRESS_MASK, M68030_ADDRESS_MASK};
use crate::types::{Long, Word};

type Cpu81 = CpuM68020Fpu<Testbus<Address, u8, Bus68020>>;
type Cpu82 = CpuM68030Fpu<Testbus<Address, u8, Bus68020>>;

const CODE: Address = 0x1000;
const HANDLER: Address = 0x2000;
const STACK: Address = 0x8000;
const DATA: Address = 0x3000;
const ONE: BitsExtReal = BitsExtReal(0x3FFF_0000_8000_0000_0000_0000);
const TWO: BitsExtReal = BitsExtReal(0x4000_0000_8000_0000_0000_0000);
const ZERO: BitsExtReal = BitsExtReal(0);

const VEC_BSUN: Word = 0xC0;
const VEC_DZ: Word = 0xC8;
const VEC_OPERR: Word = 0xD0;
const VEC_OVFL: Word = 0xD4;
const VEC_SNAN: Word = 0xD8;

const EN_SNAN: Long = 1 << 14;
const EN_OPERR: Long = 1 << 13;
const EN_OVFL: Long = 1 << 12;
const EN_DZ: Long = 1 << 10;

const RTE: Word = 0x4E73;
/// FNOP
const FNOP: [Word; 2] = [0xF280, 0x0000];
/// FDIV FP1,FP0
const FDIV: [Word; 2] = [0xF200, 0x0420];
/// FADD FP1,FP0
const FADD: [Word; 2] = [0xF200, 0x0422];

macro_rules! helpers {
    ($name:ident, $cpu:ty, $mask:expr) => {
        fn $name(code: &[Word]) -> $cpu {
            let mut cpu = <$cpu>::new(Testbus::new($mask));
            for (i, w) in code.iter().enumerate() {
                let [h, l] = w.to_be_bytes();
                cpu.bus.mem.insert(CODE + 2 * i as Address, h);
                cpu.bus.mem.insert(CODE + 2 * i as Address + 1, l);
            }
            // All the FP vectors to the handler, an RTE by default
            for vector in (0xC0..=0xD8).step_by(4) {
                for (i, b) in HANDLER.to_be_bytes().into_iter().enumerate() {
                    cpu.bus.mem.insert(vector + i as Address, b);
                }
            }
            let [h, l] = RTE.to_be_bytes();
            cpu.bus.mem.insert(HANDLER, h);
            cpu.bus.mem.insert(HANDLER + 1, l);
            cpu.regs.isp = STACK;
            cpu.regs.sr.set_supervisor(true);
            cpu.regs.a[0] = DATA;
            cpu.set_pc(CODE).unwrap();
            cpu.prefetch_refill().unwrap();
            cpu
        }
    };
}
helpers!(cpu81, Cpu81, M68020_ADDRESS_MASK);
helpers!(cpu82, Cpu82, M68030_ADDRESS_MASK);

fn long(mem: &std::collections::HashMap<Address, u8>, a: Address) -> Long {
    (0..4).fold(0, |v, i| {
        (v << 8) | Long::from(*mem.get(&(a + i)).unwrap_or(&0))
    })
}

fn code_with(first: &[Word], second: &[Word]) -> Vec<Word> {
    [first, second].concat()
}

/// A pre-instruction exception: reported at the next floating-point instruction, whose
/// address is in the frame, format $0, and FPIAR has the one that caused it. With DZ
/// the destination is not modified (MC68881UM 6.1, 6.1.6, 6.1.13).
#[test]
fn reported_before_the_next_instruction() {
    let mut cpu = cpu81(&code_with(&FDIV, &FNOP));
    cpu.regs.fpu.fpcr.0 = EN_DZ;
    cpu.regs.fpu.fp[0] = ONE;
    cpu.regs.fpu.fp[1] = ZERO;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[0], ONE);
    assert!(cpu.regs.fpu.fpsr.exs().dz());
    assert_eq!(cpu.regs.isp, STACK, "not at the instruction itself");

    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
    let isp = cpu.regs.isp;
    assert_eq!(isp, STACK - 8);
    assert_eq!(long(&cpu.bus.mem, isp + 2), CODE + 4);
    assert_eq!(long(&cpu.bus.mem, isp + 6) >> 16, Long::from(VEC_DZ));
    assert_eq!(cpu.regs.fpu.fpiar, CODE);
    assert_eq!(cpu.regs.fpu.exc_operand, ZERO);
}

/// On the MC68881 the acknowledge clears it, so a handler can be a single RTE. The
/// instruction then runs (MC68881UM 6.1.13).
#[test]
fn rte_is_enough_on_the_68881() {
    let mut cpu = cpu81(&code_with(&FDIV, &FNOP));
    cpu.regs.fpu.fpcr.0 = EN_DZ;
    cpu.regs.fpu.fp[1] = ZERO;
    cpu.regs.fpu.fp[0] = ONE;
    for _ in 0..4 {
        cpu.step().unwrap();
    }
    assert_eq!(cpu.regs.pc, CODE + 8);
    assert_eq!(cpu.regs.isp, STACK);
}

/// On the MC68882 it stays: an RTE alone takes it again. The handler has to save the state,
/// set the exception pending bit, active low, and restore (MC68881UM 6.1.13, 6.4.2.2).
#[test]
fn handler_must_clear_it_on_the_68882() {
    let mut cpu = cpu82(&code_with(&FDIV, &FNOP));
    cpu.regs.fpu.fpcr.0 = EN_DZ;
    cpu.regs.fpu.fp[1] = ZERO;
    cpu.regs.fpu.fp[0] = ONE;
    cpu.step().unwrap();
    for _ in 0..3 {
        cpu.step().unwrap();
        assert_eq!(cpu.regs.pc, HANDLER);
        cpu.step().unwrap();
        assert_eq!(cpu.regs.pc, CODE + 4);
    }

    // FSAVE -(A7), BSET #3,$38(A7), FRESTORE (A7)+, RTE
    let mut cpu = cpu82(&code_with(&FDIV, &FNOP));
    let handler: [Word; 5] = [0xF327, 0x08EF, 0x0003, 0x0038, 0xF35F];
    for (i, w) in handler.into_iter().chain([RTE]).enumerate() {
        let [h, l] = w.to_be_bytes();
        cpu.bus.mem.insert(HANDLER + 2 * i as Address, h);
        cpu.bus.mem.insert(HANDLER + 2 * i as Address + 1, l);
    }
    cpu.regs.fpu.fpcr.0 = EN_DZ;
    cpu.regs.fpu.fp[1] = ZERO;
    cpu.regs.fpu.fp[0] = ONE;
    for _ in 0..7 {
        cpu.step().unwrap();
    }
    assert_eq!(cpu.regs.pc, CODE + 8);
    assert_eq!(cpu.regs.isp, STACK);
}

/// Left as it is, the bit makes the FRESTORE bring the exception back: FSAVE and FRESTORE
/// don't report it themselves, and the frame has the operand (MC68881UM figure 6-5, 6.4.2.2)
#[test]
fn idle_frame_holds_the_pending_exception() {
    // FDIV, FSAVE -(A7), FRESTORE (A7)+, FNOP
    let mut cpu = cpu82(&[FDIV[0], FDIV[1], 0xF327, 0xF35F, FNOP[0], FNOP[1]]);
    cpu.regs.fpu.fpcr.0 = EN_DZ;
    cpu.regs.fpu.fp[1] = ZERO;
    cpu.regs.fpu.fp[0] = ONE;
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert!(!cpu.regs.fpu.exc_pending);
    let frame = STACK - 60;
    assert_eq!(cpu.regs.isp, frame);
    assert_eq!(long(&cpu.bus.mem, frame) >> 16, 0x1F38);
    // The command word
    assert_eq!(long(&cpu.bus.mem, frame + 4), 0x0420_FFFF);
    assert_eq!(long(&cpu.bus.mem, frame + 0x38) & (1 << 27), 0);
    cpu.step().unwrap();
    assert!(cpu.regs.fpu.exc_pending);
    assert_eq!(cpu.regs.pc, CODE + 8);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
}

/// FMOVEM and FMOVE of a control register don't report it (MC68881UM FMOVE, FMOVEM), and
/// don't negate it. An exception bit written to FPSR isn't pending either (MC68040UM 9.7
/// says it, and MC68881UM 6.4.2.2 has EXC PEND only from an instruction).
#[test]
fn what_does_not_report_it() {
    // FDIV, FMOVEM.X FP0,(A0), FMOVE.L FPSR,D0, FNOP
    let mut cpu = cpu81(&[
        FDIV[0], FDIV[1], 0xF210, 0xF080, 0xF200, 0xA800, FNOP[0], FNOP[1],
    ]);
    cpu.regs.fpu.fpcr.0 = EN_DZ;
    cpu.regs.fpu.fp[1] = ZERO;
    cpu.regs.fpu.fp[0] = ONE;
    cpu.step().unwrap();
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.isp, STACK);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);

    // FMOVE.L D0,FPSR, FNOP
    let mut cpu = cpu81(&[0xF200, 0x8800, FNOP[0], FNOP[1]]);
    cpu.regs.fpu.fpcr.0 = EN_DZ;
    cpu.regs.d[0] = 1 << 10;
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.isp, STACK);
}

/// SNAN and OPERR leave the destination, OVFL stores as if disabled and has the wrapped
/// operand for the handler (MC68881UM 6.1.2 to 6.1.4)
#[test]
fn what_stays_in_the_destination() {
    let snan = BitsExtReal(0x7FFF_0000_A000_0000_0000_0000);
    let mut cpu = cpu81(&code_with(&FADD, &FNOP));
    cpu.regs.fpu.fpcr.0 = EN_SNAN;
    cpu.regs.fpu.fp[0] = ONE;
    cpu.regs.fpu.fp[1] = snan;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[0], ONE);
    assert_eq!(cpu.regs.fpu.exc_operand, snan);
    cpu.step().unwrap();
    assert_eq!(
        long(&cpu.bus.mem, STACK - 8 + 6) >> 16,
        Long::from(VEC_SNAN)
    );

    // Infinity minus infinity
    let mut cpu = cpu81(&code_with(&FADD, &FNOP));
    cpu.regs.fpu.fpcr.0 = EN_OPERR;
    cpu.regs.fpu.fp[0] = BitsExtReal::inf(false);
    cpu.regs.fpu.fp[1] = BitsExtReal::inf(true);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[0], BitsExtReal::inf(false));
    cpu.step().unwrap();
    assert_eq!(
        long(&cpu.bus.mem, STACK - 8 + 6) >> 16,
        Long::from(VEC_OPERR)
    );

    // FMUL FP1,FP0 with the largest exponent: 2^16384
    let mut cpu = cpu81(&[0xF200, 0x0423, FNOP[0], FNOP[1]]);
    cpu.regs.fpu.fpcr.0 = EN_OVFL;
    cpu.regs.fpu.fp[0] = BitsExtReal(0x7FFE_0000_8000_0000_0000_0000);
    cpu.regs.fpu.fp[1] = TWO;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[0], BitsExtReal::inf(false));
    // Exponent 16384 + $3FFF - $6000 (MC68881UM 6.1.4)
    assert_eq!(
        cpu.regs.fpu.exc_operand,
        BitsExtReal(0x7FFF_0000_8000_0000_0000_0000).with_e(16384 + 0x3FFF - 0x6000)
    );
    cpu.step().unwrap();
    assert_eq!(
        long(&cpu.bus.mem, STACK - 8 + 6) >> 16,
        Long::from(VEC_OVFL)
    );
}

/// The highest priority one is reported (MC68881UM 6.1.9). No instruction sets these two, so
/// they go in FPSR by hand, which FRESTORE's pending bit can also do.
#[test]
fn priority() {
    let mut cpu = cpu81(&FNOP);
    cpu.regs.fpu.fpcr.0 = EN_DZ | EN_SNAN;
    cpu.regs.fpu.fpsr.0 = EN_DZ | EN_SNAN;
    cpu.regs.fpu.exc_pending = true;
    cpu.step().unwrap();
    assert_eq!(
        long(&cpu.bus.mem, STACK - 8 + 6) >> 16,
        Long::from(VEC_SNAN)
    );
}

/// FMOVE to memory stores, then reports it in the middle of the instruction, with both
/// addresses and the effective address, and RTE goes on after it (MC68881UM 6.1, 6.1.3,
/// 6.1.13)
#[test]
fn fmove_to_memory_reports_after_the_store() {
    // FMOVE.L FP1,(A0), NOP
    let mut cpu = cpu81(&[0xF210, 0x6080, 0x4E71, 0x4E71]);
    cpu.regs.fpu.fpcr.0 = EN_OPERR;
    cpu.regs.fpu.fp[1] = BitsExtReal::inf(false);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(long(&cpu.bus.mem, DATA), 0x7FFF_FFFF);
    let isp = cpu.regs.isp;
    assert_eq!(isp, STACK - 20);
    assert_eq!(long(&cpu.bus.mem, isp + 2), CODE + 4);
    assert_eq!(
        long(&cpu.bus.mem, isp + 6) >> 16,
        0x9000 | Long::from(VEC_OPERR)
    );
    assert_eq!(long(&cpu.bus.mem, isp + 8), CODE);
    assert_eq!(long(&cpu.bus.mem, isp + 0x10), DATA);

    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, CODE + 4);
    assert_eq!(cpu.regs.isp, STACK);
    assert!(!cpu.regs.fpu.exc_pending);
}

/// BSUN is taken before the predicate is evaluated, and the instruction runs again after
/// the handler, which has to deal with it (MC68881UM 6.1.1)
#[test]
fn bsun_before_the_predicate() {
    // FSGT D0
    let mut cpu = cpu81(&[0xF240, 0x0012, 0x4E71, 0x4E71]);
    cpu.regs.fpu.fpcr.0 = 1 << 15;
    cpu.regs.fpu.fpsr.0 = 1 << 24;
    cpu.regs.d[0] = 0x1234_5678;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(cpu.regs.d[0], 0x1234_5678);
    assert_eq!(long(&cpu.bus.mem, STACK - 8 + 2), CODE);
    assert_eq!(
        long(&cpu.bus.mem, STACK - 8 + 6) >> 16,
        Long::from(VEC_BSUN)
    );
    assert!(cpu.regs.fpu.fpsr.exs().bsun());
    // Not enabled: the predicate goes as usual
    let mut cpu = cpu81(&[0xF240, 0x0012, 0x4E71, 0x4E71]);
    cpu.regs.fpu.fpsr.0 = 1 << 24;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, CODE + 4);
}
