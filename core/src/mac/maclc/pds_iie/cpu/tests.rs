//! SingleStepTests JSON suites for the 6502-family core.
//!
//! The 6502 and 65C02 suites come from
//! <https://github.com/SingleStepTests/65x02>, in the `testdata/65x02`
//! submodule.  It is large (about 1 GB), so `git submodule update --init`
//! leaves it out; fetch it with
//!
//! ```text
//! git submodule update --init --checkout testdata/65x02
//! ```
//!
//! Without it the tests pass and print that they were skipped.
//!
//! With the `w65816` feature, the 65816 suite runs from a clone of
//! <https://github.com/SingleStepTests/65816> in `testdata/65816`.
//!
//! The harness follows the one siena uses for the 65816 suite.

use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[cfg(feature = "w65816")]
use super::ADDRESS_MASK;
use super::cpu::Cpu;
use super::regs::RegisterFile;
use super::testbus::{Access, Testbus};
use super::{Address, Bus6502, ReadKind, Variant};

/// What a test case is checked for.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Check {
    /// Registers, memory, cycle count and every bus cycle.
    All,
    /// Registers, memory and cycle count, not the bus cycles.
    NoTrace,
    /// Nothing: the suite has no test cases.
    Skip,
    /// MVN/MVP: the suite runs the whole block move as one test case,
    /// the core one byte per step.
    #[cfg(feature = "w65816")]
    MultiStep,
}

/// The checks for an opcode.  Bus cycles are left out where the core
/// cannot reproduce them, or where the suite's data is known to be wrong.
fn checks(variant: Variant, opcode: u8) -> Check {
    match (variant, opcode) {
        // JSR reads the high byte of its target after pushing the return
        // address, but the core's instruction fetcher reads all operand
        // bytes first.
        (Variant::M6502 | Variant::M65C02, 0x20) => Check::NoTrace,
        // BBR/BBS read the branch offset after the zero-page operand: the
        // same fetcher limitation.
        (Variant::M65C02, op) if op & 0x0F == 0x0F => Check::NoTrace,
        // JMP (a) and JMP (a,x): the core makes its dummy read at PC+2,
        // before the pointer, as the W65C02S datasheet (table 6-5) and bus
        // captures of real 65C02s do.  The suite has it between the
        // pointer bytes for JMP (a), and at PC+1 for JMP (a,x).
        (Variant::M65C02, 0x6C | 0x7C) => Check::NoTrace,
        // The suite's WAI and STP files are empty.
        (Variant::M65C02, 0xCB | 0xDB) => Check::Skip,
        // JSL and JSR (a,X) read their last operand byte late, like JSR.
        #[cfg(feature = "w65816")]
        (Variant::W65816, 0x22 | 0xFC) => Check::NoTrace,
        #[cfg(feature = "w65816")]
        (Variant::W65816, 0x44 | 0x54) => Check::MultiStep,
        _ => Check::All,
    }
}

/// The suite's data file for an opcode, or None when the data is missing.
fn testcase_file(variant: Variant, opcode: u8) -> Option<PathBuf> {
    let testdata = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../testdata"));
    let file = match variant {
        Variant::M6502 => testdata.join(format!("65x02/6502/v1/{opcode:02x}.json")),
        Variant::M65C02 => testdata.join(format!("65x02/wdc65c02/v1/{opcode:02x}.json")),
        #[cfg(feature = "w65816")]
        Variant::W65816 => testdata.join(format!("65816/v1/{opcode:02x}.n.json")),
    };
    file.is_file().then_some(file)
}

fn parse_regs(v: &Value, variant: Variant) -> RegisterFile {
    let p8 = |r| v[r].as_u64().unwrap().try_into().unwrap();
    let p16 = |r| v[r].as_u64().unwrap().try_into().unwrap();
    // The 6502/65C02 suites omit the 65816-only bank/direct registers.
    let popt8 = |r| {
        v.get(r)
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
            .try_into()
            .unwrap()
    };
    let popt16 = |r| {
        v.get(r)
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
            .try_into()
            .unwrap()
    };
    RegisterFile {
        c: p16("a"),
        dbr: popt8("dbr"),
        d: popt16("d"),
        k: popt8("pbr"),
        pc: p16("pc"),
        p: p8("p"),
        s: if variant.is_65816() {
            p16("s")
        } else {
            // The 6502/65C02 suites report the 8-bit stack pointer;
            // the core keeps the page-1 stack as a 16-bit register.
            0x0100 | p16("s")
        },
        x: p16("x"),
        y: p16("y"),
        emulation: match v.get("e").map(|e| e.as_u64().unwrap()) {
            Some(0) => false,
            Some(1) => true,
            _ => true,
        },
    }
}

fn parse_ram(v: &Value) -> HashMap<Address, u8> {
    HashMap::from_iter(v.as_array().unwrap().iter().map(|a| {
        (
            a[0].as_u64().unwrap().try_into().unwrap(),
            a[1].as_u64().unwrap().try_into().unwrap(),
        )
    }))
}

fn run_testcase(variant: Variant, testcase: &Value, check: Check) {
    let regs_initial = parse_regs(&testcase["initial"], variant);
    let regs_final = parse_regs(&testcase["final"], variant);
    let ram_initial = parse_ram(&testcase["initial"]["ram"]);
    let ram_final = parse_ram(&testcase["final"]["ram"]);
    let testcase_cycles = testcase["cycles"].as_array().unwrap();
    let test_cycles = testcase_cycles.len();

    #[cfg(feature = "w65816")]
    let (multi_steps, mask) = (
        check == Check::MultiStep,
        if variant.is_65816() {
            ADDRESS_MASK
        } else {
            0xFFFF
        },
    );
    #[cfg(not(feature = "w65816"))]
    let (multi_steps, mask) = (false, 0xFFFF);

    let mut bus = Testbus::new(mask);
    for (addr, val) in ram_initial {
        bus.write(addr, val);
    }

    let mut cpu = Cpu::<Testbus>::new_pc(bus, variant, 0);
    cpu.regs = regs_initial.clone();
    cpu.bus.reset_trace();

    cpu.step().unwrap();
    let per_step = cpu.cycles;
    // Keep stepping until we reach a consistent state for
    // instructions that are executed multiple times for some
    // test cases (MVN/MVP).
    while multi_steps && (cpu.cycles + per_step) <= test_cycles as u64 {
        cpu.step().unwrap();
    }

    // Extract the bus trace now so we don't record the
    // verification later.
    let bus = std::mem::replace(&mut cpu.bus, Testbus::new(mask));
    let bus_trace = bus.get_trace();

    if multi_steps {
        // Ignore PC for instructions that run in multiple steps
        // (MVN, MVP).
        cpu.regs.pc = regs_final.pc;
    }
    if cpu.regs != regs_final {
        dbg!(testcase);
        println!("Initial: {}", regs_initial);
        println!("Expected:{}", regs_final);
        println!("Actual:  {}", cpu.regs);
        panic!("Registers incorrect");
    }

    for (addr, exp_val) in ram_final {
        let val = bus.read(addr, ReadKind::Data);
        if exp_val != val {
            dbg!(testcase);
            panic!(
                "Addr {:06X} - expected {:02X}, saw {:02X}",
                addr, exp_val, val
            );
        }
    }

    if !multi_steps && cpu.cycles != test_cycles as u64 {
        dbg!(&testcase);
        println!("{}", cpu.dump_state());
        panic!("Saw {} cycles, should be {}", cpu.cycles, test_cycles);
    }

    if check == Check::NoTrace {
        return;
    }

    for trace in &bus_trace {
        let expected = &testcase_cycles[trace.cycle];
        let exp_addr: Address = expected[0].as_u64().unwrap().try_into().unwrap();

        // The 6502/65C02 suites spell accesses "read"/"write"; the
        // 65816 suite uses 8-char strings with r/w at position 3.
        let exp_str = expected[2].as_str().unwrap();
        let exp_access = if exp_str == "read" {
            Access::Read
        } else if exp_str == "write" {
            Access::Write
        } else {
            match exp_str.chars().nth(3).unwrap() {
                'r' => Access::Read,
                'w' => Access::Write,
                _ => unreachable!(),
            }
        };

        if trace.addr != exp_addr || trace.access != exp_access {
            dbg!(&testcase);
            dbg!(&trace);
            dbg!(&expected);
            panic!("Invalid trace");
        }
        if !expected[1].is_null() {
            let exp_val: u8 = expected[1].as_u64().unwrap().try_into().unwrap();
            if trace.val != exp_val {
                dbg!(&testcase);
                dbg!(&trace);
                dbg!(&expected);
                panic!("Invalid trace");
            }
        }
    }
}

/// Runs the suite's test cases for one opcode.
fn run_suite(variant: Variant, opcode: u8) {
    let check = checks(variant, opcode);
    if check == Check::Skip {
        return;
    }
    let Some(file) = testcase_file(variant, opcode) else {
        println!("{variant:?} ${opcode:02X}: no test data, skipped (see tests.rs)");
        return;
    };
    let testcases: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    for testcase in testcases.as_array().unwrap() {
        run_testcase(variant, testcase, check);
    }
}

macro_rules! cpu_test {
    ($testfn:ident, $variant:expr, $opcode:expr) => {
        #[test]
        fn $testfn() {
            run_suite($variant, $opcode);
        }
    };
}

// ---------------------------------------------------------------------------
// 6502 (NMOS): documented opcodes only; illegal opcodes are not modelled.
// ---------------------------------------------------------------------------

cpu_test!(pt6502_00, Variant::M6502, 0x00);
cpu_test!(pt6502_01, Variant::M6502, 0x01);
cpu_test!(pt6502_05, Variant::M6502, 0x05);
cpu_test!(pt6502_06, Variant::M6502, 0x06);
cpu_test!(pt6502_08, Variant::M6502, 0x08);
cpu_test!(pt6502_09, Variant::M6502, 0x09);
cpu_test!(pt6502_0a, Variant::M6502, 0x0a);
cpu_test!(pt6502_0d, Variant::M6502, 0x0d);
cpu_test!(pt6502_0e, Variant::M6502, 0x0e);
cpu_test!(pt6502_10, Variant::M6502, 0x10);
cpu_test!(pt6502_11, Variant::M6502, 0x11);
cpu_test!(pt6502_15, Variant::M6502, 0x15);
cpu_test!(pt6502_16, Variant::M6502, 0x16);
cpu_test!(pt6502_18, Variant::M6502, 0x18);
cpu_test!(pt6502_19, Variant::M6502, 0x19);
cpu_test!(pt6502_1d, Variant::M6502, 0x1d);
cpu_test!(pt6502_1e, Variant::M6502, 0x1e);
cpu_test!(pt6502_20, Variant::M6502, 0x20);
cpu_test!(pt6502_21, Variant::M6502, 0x21);
cpu_test!(pt6502_24, Variant::M6502, 0x24);
cpu_test!(pt6502_25, Variant::M6502, 0x25);
cpu_test!(pt6502_26, Variant::M6502, 0x26);
cpu_test!(pt6502_28, Variant::M6502, 0x28);
cpu_test!(pt6502_29, Variant::M6502, 0x29);
cpu_test!(pt6502_2a, Variant::M6502, 0x2a);
cpu_test!(pt6502_2c, Variant::M6502, 0x2c);
cpu_test!(pt6502_2d, Variant::M6502, 0x2d);
cpu_test!(pt6502_2e, Variant::M6502, 0x2e);
cpu_test!(pt6502_30, Variant::M6502, 0x30);
cpu_test!(pt6502_31, Variant::M6502, 0x31);
cpu_test!(pt6502_35, Variant::M6502, 0x35);
cpu_test!(pt6502_36, Variant::M6502, 0x36);
cpu_test!(pt6502_38, Variant::M6502, 0x38);
cpu_test!(pt6502_39, Variant::M6502, 0x39);
cpu_test!(pt6502_3d, Variant::M6502, 0x3d);
cpu_test!(pt6502_3e, Variant::M6502, 0x3e);
cpu_test!(pt6502_40, Variant::M6502, 0x40);
cpu_test!(pt6502_41, Variant::M6502, 0x41);
cpu_test!(pt6502_45, Variant::M6502, 0x45);
cpu_test!(pt6502_46, Variant::M6502, 0x46);
cpu_test!(pt6502_48, Variant::M6502, 0x48);
cpu_test!(pt6502_49, Variant::M6502, 0x49);
cpu_test!(pt6502_4a, Variant::M6502, 0x4a);
cpu_test!(pt6502_4c, Variant::M6502, 0x4c);
cpu_test!(pt6502_4d, Variant::M6502, 0x4d);
cpu_test!(pt6502_4e, Variant::M6502, 0x4e);
cpu_test!(pt6502_50, Variant::M6502, 0x50);
cpu_test!(pt6502_51, Variant::M6502, 0x51);
cpu_test!(pt6502_55, Variant::M6502, 0x55);
cpu_test!(pt6502_56, Variant::M6502, 0x56);
cpu_test!(pt6502_58, Variant::M6502, 0x58);
cpu_test!(pt6502_59, Variant::M6502, 0x59);
cpu_test!(pt6502_5d, Variant::M6502, 0x5d);
cpu_test!(pt6502_5e, Variant::M6502, 0x5e);
cpu_test!(pt6502_60, Variant::M6502, 0x60);
cpu_test!(pt6502_61, Variant::M6502, 0x61);
cpu_test!(pt6502_65, Variant::M6502, 0x65);
cpu_test!(pt6502_66, Variant::M6502, 0x66);
cpu_test!(pt6502_68, Variant::M6502, 0x68);
cpu_test!(pt6502_69, Variant::M6502, 0x69);
cpu_test!(pt6502_6a, Variant::M6502, 0x6a);
cpu_test!(pt6502_6c, Variant::M6502, 0x6c);
cpu_test!(pt6502_6d, Variant::M6502, 0x6d);
cpu_test!(pt6502_6e, Variant::M6502, 0x6e);
cpu_test!(pt6502_70, Variant::M6502, 0x70);
cpu_test!(pt6502_71, Variant::M6502, 0x71);
cpu_test!(pt6502_75, Variant::M6502, 0x75);
cpu_test!(pt6502_76, Variant::M6502, 0x76);
cpu_test!(pt6502_78, Variant::M6502, 0x78);
cpu_test!(pt6502_79, Variant::M6502, 0x79);
cpu_test!(pt6502_7d, Variant::M6502, 0x7d);
cpu_test!(pt6502_7e, Variant::M6502, 0x7e);
cpu_test!(pt6502_81, Variant::M6502, 0x81);
cpu_test!(pt6502_84, Variant::M6502, 0x84);
cpu_test!(pt6502_85, Variant::M6502, 0x85);
cpu_test!(pt6502_86, Variant::M6502, 0x86);
cpu_test!(pt6502_88, Variant::M6502, 0x88);
cpu_test!(pt6502_8a, Variant::M6502, 0x8a);
cpu_test!(pt6502_8c, Variant::M6502, 0x8c);
cpu_test!(pt6502_8d, Variant::M6502, 0x8d);
cpu_test!(pt6502_8e, Variant::M6502, 0x8e);
cpu_test!(pt6502_90, Variant::M6502, 0x90);
cpu_test!(pt6502_91, Variant::M6502, 0x91);
cpu_test!(pt6502_94, Variant::M6502, 0x94);
cpu_test!(pt6502_95, Variant::M6502, 0x95);
cpu_test!(pt6502_96, Variant::M6502, 0x96);
cpu_test!(pt6502_98, Variant::M6502, 0x98);
cpu_test!(pt6502_99, Variant::M6502, 0x99);
cpu_test!(pt6502_9a, Variant::M6502, 0x9a);
cpu_test!(pt6502_9d, Variant::M6502, 0x9d);
cpu_test!(pt6502_a0, Variant::M6502, 0xa0);
cpu_test!(pt6502_a1, Variant::M6502, 0xa1);
cpu_test!(pt6502_a2, Variant::M6502, 0xa2);
cpu_test!(pt6502_a4, Variant::M6502, 0xa4);
cpu_test!(pt6502_a5, Variant::M6502, 0xa5);
cpu_test!(pt6502_a6, Variant::M6502, 0xa6);
cpu_test!(pt6502_a8, Variant::M6502, 0xa8);
cpu_test!(pt6502_a9, Variant::M6502, 0xa9);
cpu_test!(pt6502_aa, Variant::M6502, 0xaa);
cpu_test!(pt6502_ac, Variant::M6502, 0xac);
cpu_test!(pt6502_ad, Variant::M6502, 0xad);
cpu_test!(pt6502_ae, Variant::M6502, 0xae);
cpu_test!(pt6502_b0, Variant::M6502, 0xb0);
cpu_test!(pt6502_b1, Variant::M6502, 0xb1);
cpu_test!(pt6502_b4, Variant::M6502, 0xb4);
cpu_test!(pt6502_b5, Variant::M6502, 0xb5);
cpu_test!(pt6502_b6, Variant::M6502, 0xb6);
cpu_test!(pt6502_b8, Variant::M6502, 0xb8);
cpu_test!(pt6502_b9, Variant::M6502, 0xb9);
cpu_test!(pt6502_ba, Variant::M6502, 0xba);
cpu_test!(pt6502_bc, Variant::M6502, 0xbc);
cpu_test!(pt6502_bd, Variant::M6502, 0xbd);
cpu_test!(pt6502_be, Variant::M6502, 0xbe);
cpu_test!(pt6502_c0, Variant::M6502, 0xc0);
cpu_test!(pt6502_c1, Variant::M6502, 0xc1);
cpu_test!(pt6502_c4, Variant::M6502, 0xc4);
cpu_test!(pt6502_c5, Variant::M6502, 0xc5);
cpu_test!(pt6502_c6, Variant::M6502, 0xc6);
cpu_test!(pt6502_c8, Variant::M6502, 0xc8);
cpu_test!(pt6502_c9, Variant::M6502, 0xc9);
cpu_test!(pt6502_ca, Variant::M6502, 0xca);
cpu_test!(pt6502_cc, Variant::M6502, 0xcc);
cpu_test!(pt6502_cd, Variant::M6502, 0xcd);
cpu_test!(pt6502_ce, Variant::M6502, 0xce);
cpu_test!(pt6502_d0, Variant::M6502, 0xd0);
cpu_test!(pt6502_d1, Variant::M6502, 0xd1);
cpu_test!(pt6502_d5, Variant::M6502, 0xd5);
cpu_test!(pt6502_d6, Variant::M6502, 0xd6);
cpu_test!(pt6502_d8, Variant::M6502, 0xd8);
cpu_test!(pt6502_d9, Variant::M6502, 0xd9);
cpu_test!(pt6502_dd, Variant::M6502, 0xdd);
cpu_test!(pt6502_de, Variant::M6502, 0xde);
cpu_test!(pt6502_e0, Variant::M6502, 0xe0);
cpu_test!(pt6502_e1, Variant::M6502, 0xe1);
cpu_test!(pt6502_e4, Variant::M6502, 0xe4);
cpu_test!(pt6502_e5, Variant::M6502, 0xe5);
cpu_test!(pt6502_e6, Variant::M6502, 0xe6);
cpu_test!(pt6502_e8, Variant::M6502, 0xe8);
cpu_test!(pt6502_e9, Variant::M6502, 0xe9);
cpu_test!(pt6502_ea, Variant::M6502, 0xea);
cpu_test!(pt6502_ec, Variant::M6502, 0xec);
cpu_test!(pt6502_ed, Variant::M6502, 0xed);
cpu_test!(pt6502_ee, Variant::M6502, 0xee);
cpu_test!(pt6502_f0, Variant::M6502, 0xf0);
cpu_test!(pt6502_f1, Variant::M6502, 0xf1);
cpu_test!(pt6502_f5, Variant::M6502, 0xf5);
cpu_test!(pt6502_f6, Variant::M6502, 0xf6);
cpu_test!(pt6502_f8, Variant::M6502, 0xf8);
cpu_test!(pt6502_f9, Variant::M6502, 0xf9);
cpu_test!(pt6502_fd, Variant::M6502, 0xfd);
cpu_test!(pt6502_fe, Variant::M6502, 0xfe);

// ---------------------------------------------------------------------------
// 65C02: all 256 opcodes are defined.
// ---------------------------------------------------------------------------

cpu_test!(pt65c02_00, Variant::M65C02, 0x00);
cpu_test!(pt65c02_01, Variant::M65C02, 0x01);
cpu_test!(pt65c02_02, Variant::M65C02, 0x02);
cpu_test!(pt65c02_03, Variant::M65C02, 0x03);
cpu_test!(pt65c02_04, Variant::M65C02, 0x04);
cpu_test!(pt65c02_05, Variant::M65C02, 0x05);
cpu_test!(pt65c02_06, Variant::M65C02, 0x06);
cpu_test!(pt65c02_07, Variant::M65C02, 0x07);
cpu_test!(pt65c02_08, Variant::M65C02, 0x08);
cpu_test!(pt65c02_09, Variant::M65C02, 0x09);
cpu_test!(pt65c02_0a, Variant::M65C02, 0x0a);
cpu_test!(pt65c02_0b, Variant::M65C02, 0x0b);
cpu_test!(pt65c02_0c, Variant::M65C02, 0x0c);
cpu_test!(pt65c02_0d, Variant::M65C02, 0x0d);
cpu_test!(pt65c02_0e, Variant::M65C02, 0x0e);
cpu_test!(pt65c02_0f, Variant::M65C02, 0x0f);
cpu_test!(pt65c02_10, Variant::M65C02, 0x10);
cpu_test!(pt65c02_11, Variant::M65C02, 0x11);
cpu_test!(pt65c02_12, Variant::M65C02, 0x12);
cpu_test!(pt65c02_13, Variant::M65C02, 0x13);
cpu_test!(pt65c02_14, Variant::M65C02, 0x14);
cpu_test!(pt65c02_15, Variant::M65C02, 0x15);
cpu_test!(pt65c02_16, Variant::M65C02, 0x16);
cpu_test!(pt65c02_17, Variant::M65C02, 0x17);
cpu_test!(pt65c02_18, Variant::M65C02, 0x18);
cpu_test!(pt65c02_19, Variant::M65C02, 0x19);
cpu_test!(pt65c02_1a, Variant::M65C02, 0x1a);
cpu_test!(pt65c02_1b, Variant::M65C02, 0x1b);
cpu_test!(pt65c02_1c, Variant::M65C02, 0x1c);
cpu_test!(pt65c02_1d, Variant::M65C02, 0x1d);
cpu_test!(pt65c02_1e, Variant::M65C02, 0x1e);
cpu_test!(pt65c02_1f, Variant::M65C02, 0x1f);
cpu_test!(pt65c02_20, Variant::M65C02, 0x20);
cpu_test!(pt65c02_21, Variant::M65C02, 0x21);
cpu_test!(pt65c02_22, Variant::M65C02, 0x22);
cpu_test!(pt65c02_23, Variant::M65C02, 0x23);
cpu_test!(pt65c02_24, Variant::M65C02, 0x24);
cpu_test!(pt65c02_25, Variant::M65C02, 0x25);
cpu_test!(pt65c02_26, Variant::M65C02, 0x26);
cpu_test!(pt65c02_27, Variant::M65C02, 0x27);
cpu_test!(pt65c02_28, Variant::M65C02, 0x28);
cpu_test!(pt65c02_29, Variant::M65C02, 0x29);
cpu_test!(pt65c02_2a, Variant::M65C02, 0x2a);
cpu_test!(pt65c02_2b, Variant::M65C02, 0x2b);
cpu_test!(pt65c02_2c, Variant::M65C02, 0x2c);
cpu_test!(pt65c02_2d, Variant::M65C02, 0x2d);
cpu_test!(pt65c02_2e, Variant::M65C02, 0x2e);
cpu_test!(pt65c02_2f, Variant::M65C02, 0x2f);
cpu_test!(pt65c02_30, Variant::M65C02, 0x30);
cpu_test!(pt65c02_31, Variant::M65C02, 0x31);
cpu_test!(pt65c02_32, Variant::M65C02, 0x32);
cpu_test!(pt65c02_33, Variant::M65C02, 0x33);
cpu_test!(pt65c02_34, Variant::M65C02, 0x34);
cpu_test!(pt65c02_35, Variant::M65C02, 0x35);
cpu_test!(pt65c02_36, Variant::M65C02, 0x36);
cpu_test!(pt65c02_37, Variant::M65C02, 0x37);
cpu_test!(pt65c02_38, Variant::M65C02, 0x38);
cpu_test!(pt65c02_39, Variant::M65C02, 0x39);
cpu_test!(pt65c02_3a, Variant::M65C02, 0x3a);
cpu_test!(pt65c02_3b, Variant::M65C02, 0x3b);
cpu_test!(pt65c02_3c, Variant::M65C02, 0x3c);
cpu_test!(pt65c02_3d, Variant::M65C02, 0x3d);
cpu_test!(pt65c02_3e, Variant::M65C02, 0x3e);
cpu_test!(pt65c02_3f, Variant::M65C02, 0x3f);
cpu_test!(pt65c02_40, Variant::M65C02, 0x40);
cpu_test!(pt65c02_41, Variant::M65C02, 0x41);
cpu_test!(pt65c02_42, Variant::M65C02, 0x42);
cpu_test!(pt65c02_43, Variant::M65C02, 0x43);
cpu_test!(pt65c02_44, Variant::M65C02, 0x44);
cpu_test!(pt65c02_45, Variant::M65C02, 0x45);
cpu_test!(pt65c02_46, Variant::M65C02, 0x46);
cpu_test!(pt65c02_47, Variant::M65C02, 0x47);
cpu_test!(pt65c02_48, Variant::M65C02, 0x48);
cpu_test!(pt65c02_49, Variant::M65C02, 0x49);
cpu_test!(pt65c02_4a, Variant::M65C02, 0x4a);
cpu_test!(pt65c02_4b, Variant::M65C02, 0x4b);
cpu_test!(pt65c02_4c, Variant::M65C02, 0x4c);
cpu_test!(pt65c02_4d, Variant::M65C02, 0x4d);
cpu_test!(pt65c02_4e, Variant::M65C02, 0x4e);
cpu_test!(pt65c02_4f, Variant::M65C02, 0x4f);
cpu_test!(pt65c02_50, Variant::M65C02, 0x50);
cpu_test!(pt65c02_51, Variant::M65C02, 0x51);
cpu_test!(pt65c02_52, Variant::M65C02, 0x52);
cpu_test!(pt65c02_53, Variant::M65C02, 0x53);
cpu_test!(pt65c02_54, Variant::M65C02, 0x54);
cpu_test!(pt65c02_55, Variant::M65C02, 0x55);
cpu_test!(pt65c02_56, Variant::M65C02, 0x56);
cpu_test!(pt65c02_57, Variant::M65C02, 0x57);
cpu_test!(pt65c02_58, Variant::M65C02, 0x58);
cpu_test!(pt65c02_59, Variant::M65C02, 0x59);
cpu_test!(pt65c02_5a, Variant::M65C02, 0x5a);
cpu_test!(pt65c02_5b, Variant::M65C02, 0x5b);
cpu_test!(pt65c02_5c, Variant::M65C02, 0x5c);
cpu_test!(pt65c02_5d, Variant::M65C02, 0x5d);
cpu_test!(pt65c02_5e, Variant::M65C02, 0x5e);
cpu_test!(pt65c02_5f, Variant::M65C02, 0x5f);
cpu_test!(pt65c02_60, Variant::M65C02, 0x60);
cpu_test!(pt65c02_61, Variant::M65C02, 0x61);
cpu_test!(pt65c02_62, Variant::M65C02, 0x62);
cpu_test!(pt65c02_63, Variant::M65C02, 0x63);
cpu_test!(pt65c02_64, Variant::M65C02, 0x64);
cpu_test!(pt65c02_65, Variant::M65C02, 0x65);
cpu_test!(pt65c02_66, Variant::M65C02, 0x66);
cpu_test!(pt65c02_67, Variant::M65C02, 0x67);
cpu_test!(pt65c02_68, Variant::M65C02, 0x68);
cpu_test!(pt65c02_69, Variant::M65C02, 0x69);
cpu_test!(pt65c02_6a, Variant::M65C02, 0x6a);
cpu_test!(pt65c02_6b, Variant::M65C02, 0x6b);
cpu_test!(pt65c02_6c, Variant::M65C02, 0x6c);
cpu_test!(pt65c02_6d, Variant::M65C02, 0x6d);
cpu_test!(pt65c02_6e, Variant::M65C02, 0x6e);
cpu_test!(pt65c02_6f, Variant::M65C02, 0x6f);
cpu_test!(pt65c02_70, Variant::M65C02, 0x70);
cpu_test!(pt65c02_71, Variant::M65C02, 0x71);
cpu_test!(pt65c02_72, Variant::M65C02, 0x72);
cpu_test!(pt65c02_73, Variant::M65C02, 0x73);
cpu_test!(pt65c02_74, Variant::M65C02, 0x74);
cpu_test!(pt65c02_75, Variant::M65C02, 0x75);
cpu_test!(pt65c02_76, Variant::M65C02, 0x76);
cpu_test!(pt65c02_77, Variant::M65C02, 0x77);
cpu_test!(pt65c02_78, Variant::M65C02, 0x78);
cpu_test!(pt65c02_79, Variant::M65C02, 0x79);
cpu_test!(pt65c02_7a, Variant::M65C02, 0x7a);
cpu_test!(pt65c02_7b, Variant::M65C02, 0x7b);
cpu_test!(pt65c02_7c, Variant::M65C02, 0x7c);
cpu_test!(pt65c02_7d, Variant::M65C02, 0x7d);
cpu_test!(pt65c02_7e, Variant::M65C02, 0x7e);
cpu_test!(pt65c02_7f, Variant::M65C02, 0x7f);
cpu_test!(pt65c02_80, Variant::M65C02, 0x80);
cpu_test!(pt65c02_81, Variant::M65C02, 0x81);
cpu_test!(pt65c02_82, Variant::M65C02, 0x82);
cpu_test!(pt65c02_83, Variant::M65C02, 0x83);
cpu_test!(pt65c02_84, Variant::M65C02, 0x84);
cpu_test!(pt65c02_85, Variant::M65C02, 0x85);
cpu_test!(pt65c02_86, Variant::M65C02, 0x86);
cpu_test!(pt65c02_87, Variant::M65C02, 0x87);
cpu_test!(pt65c02_88, Variant::M65C02, 0x88);
cpu_test!(pt65c02_89, Variant::M65C02, 0x89);
cpu_test!(pt65c02_8a, Variant::M65C02, 0x8a);
cpu_test!(pt65c02_8b, Variant::M65C02, 0x8b);
cpu_test!(pt65c02_8c, Variant::M65C02, 0x8c);
cpu_test!(pt65c02_8d, Variant::M65C02, 0x8d);
cpu_test!(pt65c02_8e, Variant::M65C02, 0x8e);
cpu_test!(pt65c02_8f, Variant::M65C02, 0x8f);
cpu_test!(pt65c02_90, Variant::M65C02, 0x90);
cpu_test!(pt65c02_91, Variant::M65C02, 0x91);
cpu_test!(pt65c02_92, Variant::M65C02, 0x92);
cpu_test!(pt65c02_93, Variant::M65C02, 0x93);
cpu_test!(pt65c02_94, Variant::M65C02, 0x94);
cpu_test!(pt65c02_95, Variant::M65C02, 0x95);
cpu_test!(pt65c02_96, Variant::M65C02, 0x96);
cpu_test!(pt65c02_97, Variant::M65C02, 0x97);
cpu_test!(pt65c02_98, Variant::M65C02, 0x98);
cpu_test!(pt65c02_99, Variant::M65C02, 0x99);
cpu_test!(pt65c02_9a, Variant::M65C02, 0x9a);
cpu_test!(pt65c02_9b, Variant::M65C02, 0x9b);
cpu_test!(pt65c02_9c, Variant::M65C02, 0x9c);
cpu_test!(pt65c02_9d, Variant::M65C02, 0x9d);
cpu_test!(pt65c02_9e, Variant::M65C02, 0x9e);
cpu_test!(pt65c02_9f, Variant::M65C02, 0x9f);
cpu_test!(pt65c02_a0, Variant::M65C02, 0xa0);
cpu_test!(pt65c02_a1, Variant::M65C02, 0xa1);
cpu_test!(pt65c02_a2, Variant::M65C02, 0xa2);
cpu_test!(pt65c02_a3, Variant::M65C02, 0xa3);
cpu_test!(pt65c02_a4, Variant::M65C02, 0xa4);
cpu_test!(pt65c02_a5, Variant::M65C02, 0xa5);
cpu_test!(pt65c02_a6, Variant::M65C02, 0xa6);
cpu_test!(pt65c02_a7, Variant::M65C02, 0xa7);
cpu_test!(pt65c02_a8, Variant::M65C02, 0xa8);
cpu_test!(pt65c02_a9, Variant::M65C02, 0xa9);
cpu_test!(pt65c02_aa, Variant::M65C02, 0xaa);
cpu_test!(pt65c02_ab, Variant::M65C02, 0xab);
cpu_test!(pt65c02_ac, Variant::M65C02, 0xac);
cpu_test!(pt65c02_ad, Variant::M65C02, 0xad);
cpu_test!(pt65c02_ae, Variant::M65C02, 0xae);
cpu_test!(pt65c02_af, Variant::M65C02, 0xaf);
cpu_test!(pt65c02_b0, Variant::M65C02, 0xb0);
cpu_test!(pt65c02_b1, Variant::M65C02, 0xb1);
cpu_test!(pt65c02_b2, Variant::M65C02, 0xb2);
cpu_test!(pt65c02_b3, Variant::M65C02, 0xb3);
cpu_test!(pt65c02_b4, Variant::M65C02, 0xb4);
cpu_test!(pt65c02_b5, Variant::M65C02, 0xb5);
cpu_test!(pt65c02_b6, Variant::M65C02, 0xb6);
cpu_test!(pt65c02_b7, Variant::M65C02, 0xb7);
cpu_test!(pt65c02_b8, Variant::M65C02, 0xb8);
cpu_test!(pt65c02_b9, Variant::M65C02, 0xb9);
cpu_test!(pt65c02_ba, Variant::M65C02, 0xba);
cpu_test!(pt65c02_bb, Variant::M65C02, 0xbb);
cpu_test!(pt65c02_bc, Variant::M65C02, 0xbc);
cpu_test!(pt65c02_bd, Variant::M65C02, 0xbd);
cpu_test!(pt65c02_be, Variant::M65C02, 0xbe);
cpu_test!(pt65c02_bf, Variant::M65C02, 0xbf);
cpu_test!(pt65c02_c0, Variant::M65C02, 0xc0);
cpu_test!(pt65c02_c1, Variant::M65C02, 0xc1);
cpu_test!(pt65c02_c2, Variant::M65C02, 0xc2);
cpu_test!(pt65c02_c3, Variant::M65C02, 0xc3);
cpu_test!(pt65c02_c4, Variant::M65C02, 0xc4);
cpu_test!(pt65c02_c5, Variant::M65C02, 0xc5);
cpu_test!(pt65c02_c6, Variant::M65C02, 0xc6);
cpu_test!(pt65c02_c7, Variant::M65C02, 0xc7);
cpu_test!(pt65c02_c8, Variant::M65C02, 0xc8);
cpu_test!(pt65c02_c9, Variant::M65C02, 0xc9);
cpu_test!(pt65c02_ca, Variant::M65C02, 0xca);
cpu_test!(pt65c02_cb, Variant::M65C02, 0xcb);
cpu_test!(pt65c02_cc, Variant::M65C02, 0xcc);
cpu_test!(pt65c02_cd, Variant::M65C02, 0xcd);
cpu_test!(pt65c02_ce, Variant::M65C02, 0xce);
cpu_test!(pt65c02_cf, Variant::M65C02, 0xcf);
cpu_test!(pt65c02_d0, Variant::M65C02, 0xd0);
cpu_test!(pt65c02_d1, Variant::M65C02, 0xd1);
cpu_test!(pt65c02_d2, Variant::M65C02, 0xd2);
cpu_test!(pt65c02_d3, Variant::M65C02, 0xd3);
cpu_test!(pt65c02_d4, Variant::M65C02, 0xd4);
cpu_test!(pt65c02_d5, Variant::M65C02, 0xd5);
cpu_test!(pt65c02_d6, Variant::M65C02, 0xd6);
cpu_test!(pt65c02_d7, Variant::M65C02, 0xd7);
cpu_test!(pt65c02_d8, Variant::M65C02, 0xd8);
cpu_test!(pt65c02_d9, Variant::M65C02, 0xd9);
cpu_test!(pt65c02_da, Variant::M65C02, 0xda);
cpu_test!(pt65c02_db, Variant::M65C02, 0xdb);
cpu_test!(pt65c02_dc, Variant::M65C02, 0xdc);
cpu_test!(pt65c02_dd, Variant::M65C02, 0xdd);
cpu_test!(pt65c02_de, Variant::M65C02, 0xde);
cpu_test!(pt65c02_df, Variant::M65C02, 0xdf);
cpu_test!(pt65c02_e0, Variant::M65C02, 0xe0);
cpu_test!(pt65c02_e1, Variant::M65C02, 0xe1);
cpu_test!(pt65c02_e2, Variant::M65C02, 0xe2);
cpu_test!(pt65c02_e3, Variant::M65C02, 0xe3);
cpu_test!(pt65c02_e4, Variant::M65C02, 0xe4);
cpu_test!(pt65c02_e5, Variant::M65C02, 0xe5);
cpu_test!(pt65c02_e6, Variant::M65C02, 0xe6);
cpu_test!(pt65c02_e7, Variant::M65C02, 0xe7);
cpu_test!(pt65c02_e8, Variant::M65C02, 0xe8);
cpu_test!(pt65c02_e9, Variant::M65C02, 0xe9);
cpu_test!(pt65c02_ea, Variant::M65C02, 0xea);
cpu_test!(pt65c02_eb, Variant::M65C02, 0xeb);
cpu_test!(pt65c02_ec, Variant::M65C02, 0xec);
cpu_test!(pt65c02_ed, Variant::M65C02, 0xed);
cpu_test!(pt65c02_ee, Variant::M65C02, 0xee);
cpu_test!(pt65c02_ef, Variant::M65C02, 0xef);
cpu_test!(pt65c02_f0, Variant::M65C02, 0xf0);
cpu_test!(pt65c02_f1, Variant::M65C02, 0xf1);
cpu_test!(pt65c02_f2, Variant::M65C02, 0xf2);
cpu_test!(pt65c02_f3, Variant::M65C02, 0xf3);
cpu_test!(pt65c02_f4, Variant::M65C02, 0xf4);
cpu_test!(pt65c02_f5, Variant::M65C02, 0xf5);
cpu_test!(pt65c02_f6, Variant::M65C02, 0xf6);
cpu_test!(pt65c02_f7, Variant::M65C02, 0xf7);
cpu_test!(pt65c02_f8, Variant::M65C02, 0xf8);
cpu_test!(pt65c02_f9, Variant::M65C02, 0xf9);
cpu_test!(pt65c02_fa, Variant::M65C02, 0xfa);
cpu_test!(pt65c02_fb, Variant::M65C02, 0xfb);
cpu_test!(pt65c02_fc, Variant::M65C02, 0xfc);
cpu_test!(pt65c02_fd, Variant::M65C02, 0xfd);
cpu_test!(pt65c02_fe, Variant::M65C02, 0xfe);
cpu_test!(pt65c02_ff, Variant::M65C02, 0xff);

// ---------------------------------------------------------------------------
// 65816 (native mode), as in siena.
// ---------------------------------------------------------------------------

#[cfg(feature = "w65816")]
#[test]
fn pt65816_suite() {
    for opcode in 0x00..=0xFF {
        run_suite(Variant::W65816, opcode);
    }
}
