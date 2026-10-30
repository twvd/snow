//! Self-checking fixture ROMs from benletchford/m68k-rs (MIT), run on
//! Snow's 68020/68030: the binary links at 0x10000, signals pass/fail
//! through the test device at 0x100000, and ends in `stop #0x2700`.
//! See `fixtures/README.md` for the harness contract and attribution.
//!
//! Portions derived from m68k-rs (https://github.com/benletchford/m68k-rs),
//! Copyright (c) 2026 Ben Letchford, used under the MIT License:
//!
//! Permission is hereby granted, free of charge, to any person obtaining a
//! copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to
//! permit persons to whom the Software is furnished to do so, subject to
//! the following conditions:
//!
//! The above copyright notice and this permission notice shall be included
//! in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//! OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
//! IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
//! CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
//! TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
//! SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use crate::bus::Address;
use crate::bus::access::{Bus68000, Bus68020};
use crate::bus::testbus::{TestDevice, Testbus};
use crate::cpu_m68k::disassembler::Disassembler;
use crate::cpu_m68k::{
    CpuM68000, CpuM68020Fpu, CpuM68030Fpu, M68000_ADDRESS_MASK, M68020_ADDRESS_MASK,
    M68030_ADDRESS_MASK,
};

/// 32-bit data bus (68020/68030).
type TestBus = Testbus<Address, u8, Bus68020>;
/// 16-bit data bus (68000).
type TestBus16 = Testbus<Address, u8, Bus68000>;

/// Fixture link address (entry `_start`), per the Musashi/m68k-rs harness.
const FIXTURE_BASE: Address = 0x1_0000;
/// Instruction budget if the fixture neither signals nor stops. The
/// Musashi BCD fixtures run ~720k instructions in nested DBF loops.
const MAX_STEPS: usize = 1_000_000;

/// Runs a fixture ROM on a CPU of type `$cpu_ty`: loads the binary, sets
/// SSP = 0x3F0 and SR = 0x2700 (supervisor, interrupts masked), starts at
/// `FIXTURE_BASE`, and steps until the test device has signalled or the
/// budget is spent. Returns (pass_count, fail_count). On failure, prints
/// the tail of the instruction history and the register file.
macro_rules! run_fixture {
    ($name:ident, $cpu_ty:ty, $mask:expr) => {
        fn $name(cpu: &mut $cpu_ty, binary: &[u8]) -> (u32, u32) {
            for (i, &byte) in binary.iter().enumerate() {
                cpu.bus.mem.insert(FIXTURE_BASE + i as Address, byte);
            }
            cpu.bus.test_device = Some(TestDevice::default());
            cpu.regs.sr.set_supervisor(true);
            cpu.regs.sr.set_int_prio_mask(7);
            cpu.regs.isp = 0x3F0;
            cpu.set_pc(FIXTURE_BASE).expect("set_pc");
            cpu.prefetch_refill().expect("prefetch");
            let mut history: Vec<Address> = Vec::with_capacity(1024);
            for _ in 0..MAX_STEPS {
                let verdict = {
                    let device = cpu.bus.test_device.as_ref().unwrap();
                    device.pass_count + device.fail_count > 0
                };
                if verdict {
                    break;
                }
                history.push(cpu.regs.pc);
                if history.len() > 1024 {
                    history.remove(0);
                }
                if cpu.step().is_err() {
                    break;
                }
            }
            let device = cpu.bus.test_device.take().unwrap();
            if device.fail_count > 0 || device.pass_count == 0 {
                eprintln!(
                    "fixture {}: pass={} fail={} — last PCs: {:X?}",
                    stringify!($name),
                    device.pass_count,
                    device.fail_count,
                    &history[history.len().saturating_sub(80)..]
                );
                eprintln!(
                    "D0={:08X} D1={:08X} D2={:08X} D3={:08X} D4={:08X} D5={:08X} D6={:08X} D7={:08X}",
                    cpu.regs.d[0],
                    cpu.regs.d[1],
                    cpu.regs.d[2],
                    cpu.regs.d[3],
                    cpu.regs.d[4],
                    cpu.regs.d[5],
                    cpu.regs.d[6],
                    cpu.regs.d[7]
                );
                eprintln!(
                    "A0={:08X} A1={:08X} A2={:08X} A3={:08X} A4={:08X} A5={:08X} A6={:08X} USP={:08X} ISP={:08X} SR={:04X}",
                    cpu.regs.a[0],
                    cpu.regs.a[1],
                    cpu.regs.a[2],
                    cpu.regs.a[3],
                    cpu.regs.a[4],
                    cpu.regs.a[5],
                    cpu.regs.a[6],
                    cpu.regs.usp,
                    cpu.regs.isp,
                    cpu.regs.sr.0
                );
                let tail: Vec<Address> = history.iter().rev().take(24).copied().collect();
                for &pc in tail.iter().rev() {
                    let bytes: Vec<u8> = (0..16)
                        .map(|o| *cpu.bus.mem.get(&pc.wrapping_add(o)).unwrap_or(&0))
                        .collect();
                    let mut iter = bytes.into_iter();
                    let mut dis = Disassembler::from(&mut iter, pc);
                    if let Some(entry) = dis.next() {
                        eprintln!("  {:08X}: {}", entry.addr, entry.str);
                    }
                }
            }
            (device.pass_count, device.fail_count)
        }
    };
}

run_fixture!(run_fixture_000, CpuM68000<TestBus16>, M68000_ADDRESS_MASK);
run_fixture!(run_fixture_020, CpuM68020Fpu<TestBus>, M68020_ADDRESS_MASK);
run_fixture!(run_fixture_030, CpuM68030Fpu<TestBus>, M68030_ADDRESS_MASK);

/// The fixture signalled pass at least once and never failed.
fn assert_passed(name: &str, (pass, fail): (u32, u32)) {
    assert!(fail == 0 && pass >= 1, "{name}: pass={pass}, fail={fail}");
}

macro_rules! fixture_test {
    ($name:ident, $bin:literal, $run:ident, $cpu:ident, $mask:expr) => {
        #[test]
        fn $name() {
            let mut cpu = $cpu::new(Testbus::new($mask));
            let counts = $run(&mut cpu, include_bytes!($bin));
            assert_passed(stringify!($name), counts);
        }
    };
    ($(#[$attr:meta])* $name:ident, $bin:literal, $run:ident, $cpu:ident, $mask:expr) => {
        $(#[$attr])*
        #[test]
        fn $name() {
            let mut cpu = $cpu::new(Testbus::new($mask));
            let counts = $run(&mut cpu, include_bytes!($bin));
            assert_passed(stringify!($name), counts);
        }
    };
}

// 68020 fixtures. CpuM68020Fpu: M68020 + 68881, no PMMU.
fixture_test!(
    fixture_bitfield_ops,
    "fixtures/m68020/bin/bitfield_ops.bin",
    run_fixture_020,
    CpuM68020Fpu,
    M68020_ADDRESS_MASK
);
fixture_test!(
    fixture_cas,
    "fixtures/m68020/bin/cas.bin",
    run_fixture_020,
    CpuM68020Fpu,
    M68020_ADDRESS_MASK
);
fixture_test!(
    fixture_long_muldiv,
    "fixtures/m68020/bin/long_muldiv.bin",
    run_fixture_020,
    CpuM68020Fpu,
    M68020_ADDRESS_MASK
);
fixture_test!(
    fixture_chk2_cmp2,
    "fixtures/m68020/bin/chk2_cmp2.bin",
    run_fixture_020,
    CpuM68020Fpu,
    M68020_ADDRESS_MASK
);
fixture_test!(
    fixture_boundary_edge,
    "fixtures/m68020/bin/boundary_edge.bin",
    run_fixture_020,
    CpuM68020Fpu,
    M68020_ADDRESS_MASK
);
fixture_test!(
    fixture_msp_test,
    "fixtures/m68020/bin/msp_test.bin",
    run_fixture_020,
    CpuM68020Fpu,
    M68020_ADDRESS_MASK
);
// T0 trace-on-change-of-flow is not implemented: Snow's SR mask forces
// the T0 bit to zero, so the fixture sees no trace exceptions. Expected
// to fail until T0 exists.
fixture_test!(
    #[ignore = "T0 trace-on-change-of-flow not implemented"]
    fixture_trace_t0,
    "fixtures/m68020/bin/trace_t0.bin",
    run_fixture_020,
    CpuM68020Fpu,
    M68020_ADDRESS_MASK
);
// CALLM/RTM are not implemented (68020-only module-call pair).
fixture_test!(
    #[ignore = "CALLM/RTM not implemented"]
    fixture_callm_020,
    "fixtures/m68020/bin/callm_020.bin",
    run_fixture_020,
    CpuM68020Fpu,
    M68020_ADDRESS_MASK
);

// 68030 fixtures. CpuM68030Fpu: M68030 + 68882, PMMU present (off by
// default in these fixtures).
fixture_test!(
    fixture_cache_030,
    "fixtures/m68030/bin/cache_030.bin",
    run_fixture_030,
    CpuM68030Fpu,
    M68030_ADDRESS_MASK
);
// Negative test: MOVE16 is 68040-only and must take Line-F on the 68030.
fixture_test!(
    fixture_move16_030,
    "fixtures/m68030/bin/move16_030.bin",
    run_fixture_030,
    CpuM68030Fpu,
    M68030_ADDRESS_MASK
);

// ---------------------------------------------------------------------------
// Musashi test ROMs (kstenerud/Musashi, MIT, see fixtures/README.md for their
// authors and licenses): mc68040 set = the 68020+
// instruction extensions, run on Snow's 68020; mc68000 set = the classic
// 68000 suite, run on Snow's 68000. Same device contract as the extra
// fixtures (Musashi test/entry.s: TEST_FAIL_REG 0x100000,
// TEST_PASS_REG 0x100004, INTERRUPT_REG 0x10000C, STACK_BASE 0x3F0).
// ---------------------------------------------------------------------------

macro_rules! musashi_test {
    ($name:ident, $bin:literal, $run:ident, $cpu:ident, $mask:expr) => {
        #[test]
        fn $name() {
            let mut cpu = $cpu::new(Testbus::new($mask));
            let counts = $run(&mut cpu, include_bytes!($bin));
            assert_passed(stringify!($name), counts);
        }
    };
    ($(#[$attr:meta])* $name:ident, $bin:literal, $run:ident, $cpu:ident, $mask:expr) => {
        $(#[$attr])*
        #[test]
        fn $name() {
            let mut cpu = $cpu::new(Testbus::new($mask));
            let counts = $run(&mut cpu, include_bytes!($bin));
            assert_passed(stringify!($name), counts);
        }
    };
}

macro_rules! musashi_040 {
    ($name:ident, $bin:literal) => {
        musashi_test!($name, $bin, run_fixture_020, CpuM68020Fpu, M68020_ADDRESS_MASK);
    };
    ($(#[$attr:meta])* $name:ident, $bin:literal) => {
        musashi_test!($(#[$attr])* $name, $bin, run_fixture_020, CpuM68020Fpu, M68020_ADDRESS_MASK);
    };
}

macro_rules! musashi_000 {
    ($name:ident, $bin:literal) => {
        musashi_test!($name, $bin, run_fixture_000, CpuM68000, M68000_ADDRESS_MASK);
    };
    ($(#[$attr:meta])* $name:ident, $bin:literal) => {
        musashi_test!($(#[$attr])* $name, $bin, run_fixture_000, CpuM68000, M68000_ADDRESS_MASK);
    };
}

musashi_040!(musashi_040_bfchg, "fixtures/musashi/mc68040/bfchg.bin");
musashi_040!(musashi_040_bfclr, "fixtures/musashi/mc68040/bfclr.bin");
musashi_040!(musashi_040_bfext, "fixtures/musashi/mc68040/bfext.bin");
musashi_040!(musashi_040_bfffo, "fixtures/musashi/mc68040/bfffo.bin");
musashi_040!(musashi_040_bfins, "fixtures/musashi/mc68040/bfins.bin");
musashi_040!(musashi_040_bfset, "fixtures/musashi/mc68040/bfset.bin");
musashi_040!(musashi_040_bftst, "fixtures/musashi/mc68040/bftst.bin");
musashi_040!(musashi_040_cas, "fixtures/musashi/mc68040/cas.bin");
// The Musashi chk2/cmp2 fixtures use unsigned/reversed-bound expectations;
// the signed cputest semantics in the extra fixtures are authoritative
// (same rationale m68k-rs uses to ignore them).
musashi_040!(
    #[ignore = "legacy Musashi fixture uses unsigned/reversed-bound expectations"]
    musashi_040_chk2,
    "fixtures/musashi/mc68040/chk2.bin"
);
musashi_040!(
    #[ignore = "legacy Musashi fixture uses unsigned/reversed-bound expectations"]
    musashi_040_cmp2,
    "fixtures/musashi/mc68040/cmp2.bin"
);
musashi_040!(
    musashi_040_divs_long,
    "fixtures/musashi/mc68040/divs_long.bin"
);
musashi_040!(
    musashi_040_divu_long,
    "fixtures/musashi/mc68040/divu_long.bin"
);
// The instruction after the write to the test device's interrupt register
// expects the handler to have run already. That is Musashi's device: the
// manuals define no such latency for an asynchronous request, and in Snow one
// more instruction runs.
musashi_040!(
    #[ignore = "assumes no latency between raising the request and taking the interrupt"]
    musashi_040_interrupt,
    "fixtures/musashi/mc68040/interrupt.bin"
);
musashi_040!(musashi_040_jmp, "fixtures/musashi/mc68040/jmp.bin");
musashi_040!(
    musashi_040_mul_long,
    "fixtures/musashi/mc68040/mul_long.bin"
);
musashi_040!(musashi_040_rtd, "fixtures/musashi/mc68040/rtd.bin");
musashi_040!(musashi_040_shifts3, "fixtures/musashi/mc68040/shifts3.bin");
musashi_040!(musashi_040_trapcc, "fixtures/musashi/mc68040/trapcc.bin");

/// EXTB.L decodes and executes on the 68020+ (the decode gate was fixed
/// from M68000; see test_extb_l_is_illegal_on_68000).
#[test]
fn extb_l_executes_on_68020() {
    let mut cpu = CpuM68020Fpu::<TestBus>::new(Testbus::new(M68020_ADDRESS_MASK));
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.d[0] = 0x1234_5680; // byte 0x80 = -128
    cpu.bus.mem.insert(0x1000, 0x49); // EXTB.l D0
    cpu.bus.mem.insert(0x1001, 0xC0);
    cpu.set_pc(0x1000).expect("set_pc");
    cpu.prefetch_refill().expect("prefetch");
    cpu.step().expect("step");
    assert_eq!(cpu.regs.d[0], 0xFFFF_FF80, "byte sign-extended to long");
    assert!(cpu.regs.sr.n(), "N set");
    assert!(!cpu.regs.sr.z(), "Z cleared");
}

// The BCD fixtures drive the loop counters (0x99..0x00) into non-BCD
// digits (0x8F etc.). Snow matches a digit-wise BCD reference for every
// valid-digit case, but the checksums were computed from Musashi's
// behaviour on invalid digits, which the PRM leaves undefined.
musashi_000!(
    #[ignore = "checksum covers non-BCD inputs; undefined per PRM (Snow verified correct on valid digits)"]
    musashi_000_abcd,
    "fixtures/musashi/mc68000/abcd.bin"
);
musashi_000!(musashi_000_adda, "fixtures/musashi/mc68000/adda.bin");
musashi_000!(musashi_000_add, "fixtures/musashi/mc68000/add.bin");
musashi_000!(musashi_000_add_i, "fixtures/musashi/mc68000/add_i.bin");
musashi_000!(musashi_000_addq, "fixtures/musashi/mc68000/addq.bin");
musashi_000!(musashi_000_addx, "fixtures/musashi/mc68000/addx.bin");
musashi_000!(musashi_000_and, "fixtures/musashi/mc68000/and.bin");
musashi_000!(
    musashi_000_andi_to_ccr,
    "fixtures/musashi/mc68000/andi_to_ccr.bin"
);
musashi_000!(
    musashi_000_andi_to_sr,
    "fixtures/musashi/mc68000/andi_to_sr.bin"
);
musashi_000!(musashi_000_bcc, "fixtures/musashi/mc68000/bcc.bin");
musashi_000!(musashi_000_bchg, "fixtures/musashi/mc68000/bchg.bin");
musashi_000!(musashi_000_bclr, "fixtures/musashi/mc68000/bclr.bin");
musashi_000!(musashi_000_bool_i, "fixtures/musashi/mc68000/bool_i.bin");
musashi_000!(musashi_000_bset, "fixtures/musashi/mc68000/bset.bin");
musashi_000!(musashi_000_bsr, "fixtures/musashi/mc68000/bsr.bin");
musashi_000!(musashi_000_btst, "fixtures/musashi/mc68000/btst.bin");
musashi_000!(musashi_000_chk, "fixtures/musashi/mc68000/chk.bin");
musashi_000!(musashi_000_cmpa, "fixtures/musashi/mc68000/cmpa.bin");
musashi_000!(musashi_000_cmp, "fixtures/musashi/mc68000/cmp.bin");
musashi_000!(musashi_000_cmpm, "fixtures/musashi/mc68000/cmpm.bin");
musashi_000!(musashi_000_dbcc, "fixtures/musashi/mc68000/dbcc.bin");
// DIVS/DIVU on the 68000: every division in these fixtures overflows
// (|quotient| > 32767), and the flag checksums cover N/Z in overflow
// cases, which the PRM leaves undefined ("undefined if overflow or
// divide by zero occurs", M68000PRM DIVS/DIVU). Snow's data results
// match exactly and its defined-case flags are correct; the checksum
// captures Musashi's choice for the undefined bits.
musashi_000!(
    #[ignore = "flag checksum covers overflow cases where N/Z are undefined per PRM"]
    musashi_000_divs,
    "fixtures/musashi/mc68000/divs.bin"
);
musashi_000!(
    #[ignore = "flag checksum covers overflow cases where N/Z are undefined per PRM"]
    musashi_000_divu,
    "fixtures/musashi/mc68000/divu.bin"
);
musashi_000!(musashi_000_eor, "fixtures/musashi/mc68000/eor.bin");
musashi_000!(
    musashi_000_eori_to_ccr,
    "fixtures/musashi/mc68000/eori_to_ccr.bin"
);
musashi_000!(
    musashi_000_eori_to_sr,
    "fixtures/musashi/mc68000/eori_to_sr.bin"
);
musashi_000!(musashi_000_exg, "fixtures/musashi/mc68000/exg.bin");
musashi_000!(musashi_000_ext, "fixtures/musashi/mc68000/ext.bin");
musashi_000!(musashi_000_lea_pea, "fixtures/musashi/mc68000/lea_pea.bin");
musashi_000!(musashi_000_lea_tas, "fixtures/musashi/mc68000/lea_tas.bin");
musashi_000!(musashi_000_lea_tst, "fixtures/musashi/mc68000/lea_tst.bin");
musashi_000!(musashi_000_links, "fixtures/musashi/mc68000/links.bin");
musashi_000!(musashi_000_move, "fixtures/musashi/mc68000/move.bin");
musashi_000!(musashi_000_movem, "fixtures/musashi/mc68000/movem.bin");
musashi_000!(musashi_000_movep, "fixtures/musashi/mc68000/movep.bin");
musashi_000!(musashi_000_moveq, "fixtures/musashi/mc68000/moveq.bin");
musashi_000!(
    musashi_000_move_usp,
    "fixtures/musashi/mc68000/move_usp.bin"
);
musashi_000!(
    musashi_000_move_xxx_flags,
    "fixtures/musashi/mc68000/move_xxx_flags.bin"
);
musashi_000!(musashi_000_muls, "fixtures/musashi/mc68000/muls.bin");
musashi_000!(musashi_000_mulu, "fixtures/musashi/mc68000/mulu.bin");
musashi_000!(musashi_000_nbcd, "fixtures/musashi/mc68000/nbcd.bin");
musashi_000!(musashi_000_negs, "fixtures/musashi/mc68000/negs.bin");
musashi_000!(
    musashi_000_op_cmp_i,
    "fixtures/musashi/mc68000/op_cmp_i.bin"
);
musashi_000!(musashi_000_or, "fixtures/musashi/mc68000/or.bin");
musashi_000!(
    musashi_000_ori_to_ccr,
    "fixtures/musashi/mc68000/ori_to_ccr.bin"
);
musashi_000!(
    musashi_000_ori_to_sr,
    "fixtures/musashi/mc68000/ori_to_sr.bin"
);
musashi_000!(musashi_000_rox, "fixtures/musashi/mc68000/rox.bin");
musashi_000!(musashi_000_roxx, "fixtures/musashi/mc68000/roxx.bin");
musashi_000!(musashi_000_rtr, "fixtures/musashi/mc68000/rtr.bin");
musashi_000!(
    #[ignore = "checksum covers non-BCD inputs; undefined per PRM (Snow verified correct on valid digits)"]
    musashi_000_sbcd,
    "fixtures/musashi/mc68000/sbcd.bin"
);
musashi_000!(musashi_000_scc, "fixtures/musashi/mc68000/scc.bin");
musashi_000!(musashi_000_shifts, "fixtures/musashi/mc68000/shifts.bin");
musashi_000!(musashi_000_shifts2, "fixtures/musashi/mc68000/shifts2.bin");
musashi_000!(musashi_000_suba, "fixtures/musashi/mc68000/suba.bin");
musashi_000!(musashi_000_sub, "fixtures/musashi/mc68000/sub.bin");
musashi_000!(musashi_000_sub_i, "fixtures/musashi/mc68000/sub_i.bin");
musashi_000!(musashi_000_subq, "fixtures/musashi/mc68000/subq.bin");
musashi_000!(musashi_000_subx, "fixtures/musashi/mc68000/subx.bin");
musashi_000!(musashi_000_swap, "fixtures/musashi/mc68000/swap.bin");
musashi_000!(musashi_000_trapv, "fixtures/musashi/mc68000/trapv.bin");
