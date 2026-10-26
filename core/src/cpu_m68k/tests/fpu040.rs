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
