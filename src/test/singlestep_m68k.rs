use flate2::read::GzDecoder;
use serde::Deserialize;

use std::fs;
use std::path::Path;

use crate::bus::testbus::Testbus;
use crate::bus::{Address, Bus, ADDRESS_MASK};
use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::regs::{RegisterFile, RegisterSR};
use crate::tickable::Ticks;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestcaseState {
    d0: u32,
    d1: u32,
    d2: u32,
    d3: u32,
    d4: u32,
    d5: u32,
    d6: u32,
    d7: u32,
    a0: u32,
    a1: u32,
    a2: u32,
    a3: u32,
    a4: u32,
    a5: u32,
    a6: u32,
    usp: u32,
    ssp: u32,
    sr: u16,
    pc: u32,
    prefetch: [u16; 2],
    ram: Vec<(Address, u8)>,
}

#[derive(Debug, Deserialize)]
/// One (JSON-object) testcase
struct Testcase {
    /// Testcase name
    name: String,

    /// Initial state
    initial: TestcaseState,

    /// Expected state after the test
    r#final: TestcaseState,

    /// Total amount of cycles
    length: Ticks,
}

macro_rules! cpu_test {
    ($testfn:ident, $instr:expr) => {
        #[test]
        fn $testfn() {
            let filename = format!("testdata/680x0/68000/v1/{}.json", $instr);
            let filename_gz = format!("{}.gz", filename);
            let testcases: Vec<Testcase> = if Path::new(&filename).exists() {
                serde_json::from_reader(fs::File::open(filename).unwrap()).unwrap()
            } else {
                serde_json::from_reader(GzDecoder::new(fs::File::open(filename_gz).unwrap()))
                    .unwrap()
            };

            for testcase in testcases {
                run_testcase(testcase);
            }
        }
    };
}

fn create_regs(state: &TestcaseState) -> RegisterFile {
    RegisterFile {
        a: [
            state.a0, state.a1, state.a2, state.a3, state.a4, state.a5, state.a6,
        ],
        d: [
            state.d0, state.d1, state.d2, state.d3, state.d4, state.d5, state.d6, state.d7,
        ],
        usp: state.usp,
        ssp: state.ssp,
        sr: RegisterSR(state.sr),
        pc: state.pc,
    }
}

fn run_testcase(testcase: Testcase) {
    let regs_initial = create_regs(&testcase.initial);
    let regs_final = create_regs(&testcase.r#final);

    let mut bus = Testbus::new(ADDRESS_MASK);
    for (addr, val) in &testcase.initial.ram {
        bus.write(*addr, *val);
    }

    let mut cpu = CpuM68k::new(bus);
    cpu.regs = regs_initial.clone();
    cpu.prefetch = testcase.initial.prefetch.into();
    if let Err(e) = cpu.step() {
        dbg!(&testcase);
        panic!("Test {}: error: {:?}", testcase.name, e);
    }

    if cpu.prefetch.make_contiguous() != testcase.r#final.prefetch {
        dbg!(&testcase);
        panic!(
            "Test {}: prefetch: expected {:?}, saw {:?}",
            testcase.name,
            testcase.r#final.prefetch,
            cpu.prefetch.make_contiguous()
        );
    }

    if cpu.regs != regs_final {
        dbg!(&testcase);
        eprintln!("Initial: {}", &regs_initial);
        eprintln!("Final  : {}", &regs_final);
        eprintln!("Actual : {}", &cpu.regs);
        panic!("Test {}: Registers do not match", testcase.name);
    }

    for (addr, expected) in &testcase.r#final.ram {
        let actual = cpu.bus.read(*addr);
        if actual != *expected {
            panic!(
                "Test {}: bus address {:06X}: expected {}, saw {}",
                testcase.name, addr, *expected, actual
            );
        }
    }

    if cpu.cycles != testcase.length {
        dbg!(&testcase);
        panic!(
            "Test {}: expected {} cycles, saw {}",
            testcase.name, testcase.length, cpu.cycles
        );
    }

    // TODO transactions
}

//cpu_test!(abcd, "ABCD");
//cpu_test!(adda_l, "ADDA.l");
//cpu_test!(adda_w, "ADDA.w");
//cpu_test!(add_b, "ADD.b");
//cpu_test!(add_l, "ADD.l");
//cpu_test!(add_w, "ADD.w");
//cpu_test!(addx_b, "ADDX.b");
//cpu_test!(addx_l, "ADDX.l");
//cpu_test!(addx_w, "ADDX.w");
cpu_test!(and_b, "AND.b");
//cpu_test!(anditoccr, "ANDItoCCR");
//cpu_test!(anditosr, "ANDItoSR");
cpu_test!(and_l, "AND.l");
cpu_test!(and_w, "AND.w");
//cpu_test!(asl_b, "ASL.b");
//cpu_test!(asl_l, "ASL.l");
//cpu_test!(asl_w, "ASL.w");
//cpu_test!(asr_b, "ASR.b");
//cpu_test!(asr_l, "ASR.l");
//cpu_test!(asr_w, "ASR.w");
//cpu_test!(bcc, "Bcc");
//cpu_test!(bchg, "BCHG");
//cpu_test!(bclr, "BCLR");
//cpu_test!(bset, "BSET");
//cpu_test!(bsr, "BSR");
//cpu_test!(btst, "BTST");
//cpu_test!(chk, "CHK");
//cpu_test!(clr_b, "CLR.b");
//cpu_test!(clr_l, "CLR.l");
//cpu_test!(clr_w, "CLR.w");
//cpu_test!(cmpa_l, "CMPA.l");
//cpu_test!(cmpa_w, "CMPA.w");
//cpu_test!(cmp_b, "CMP.b");
//cpu_test!(cmp_l, "CMP.l");
//cpu_test!(cmp_w, "CMP.w");
//cpu_test!(dbcc, "DBcc");
//cpu_test!(divs, "DIVS");
//cpu_test!(divu, "DIVU");
//cpu_test!(eor_b, "EOR.b");
//cpu_test!(eoritoccr, "EORItoCCR");
//cpu_test!(eoritosr, "EORItoSR");
//cpu_test!(eor_l, "EOR.l");
//cpu_test!(eor_w, "EOR.w");
//cpu_test!(exg, "EXG");
//cpu_test!(ext_l, "EXT.l");
//cpu_test!(ext_w, "EXT.w");
//cpu_test!(jmp, "JMP");
//cpu_test!(jsr, "JSR");
//cpu_test!(lea, "LEA");
//cpu_test!(link, "LINK");
//cpu_test!(lsl_b, "LSL.b");
//cpu_test!(lsl_l, "LSL.l");
//cpu_test!(lsl_w, "LSL.w");
//cpu_test!(lsr_b, "LSR.b");
//cpu_test!(lsr_l, "LSR.l");
//cpu_test!(lsr_w, "LSR.w");
//cpu_test!(movea_l, "MOVEA.l");
//cpu_test!(movea_w, "MOVEA.w");
//cpu_test!(move_b, "MOVE.b");
//cpu_test!(movefromsr, "MOVEfromSR");
//cpu_test!(movefromusp, "MOVEfromUSP");
//cpu_test!(move_l, "MOVE.l");
//cpu_test!(movem_l, "MOVEM.l");
//cpu_test!(movem_w, "MOVEM.w");
//cpu_test!(movep_l, "MOVEP.l");
//cpu_test!(movep_w, "MOVEP.w");
//cpu_test!(move_q, "MOVE.q");
//cpu_test!(movetoccr, "MOVEtoCCR");
//cpu_test!(movetosr, "MOVEtoSR");
//cpu_test!(movetousp, "MOVEtoUSP");
//cpu_test!(move_w, "MOVE.w");
//cpu_test!(muls, "MULS");
//cpu_test!(mulu, "MULU");
//cpu_test!(nbcd, "NBCD");
//cpu_test!(neg_b, "NEG.b");
//cpu_test!(neg_l, "NEG.l");
//cpu_test!(neg_w, "NEG.w");
//cpu_test!(negx_b, "NEGX.b");
//cpu_test!(negx_l, "NEGX.l");
//cpu_test!(negx_w, "NEGX.w");
cpu_test!(nop, "NOP");
//cpu_test!(not_b, "NOT.b");
//cpu_test!(not_l, "NOT.l");
//cpu_test!(not_w, "NOT.w");
//cpu_test!(or_b, "OR.b");
//cpu_test!(oritoccr, "ORItoCCR");
//cpu_test!(oritosr, "ORItoSR");
//cpu_test!(or_l, "OR.l");
//cpu_test!(or_w, "OR.w");
//cpu_test!(pea, "PEA");
//cpu_test!(reset, "RESET");
//cpu_test!(rol_b, "ROL.b");
//cpu_test!(rol_l, "ROL.l");
//cpu_test!(rol_w, "ROL.w");
//cpu_test!(ror_b, "ROR.b");
//cpu_test!(ror_l, "ROR.l");
//cpu_test!(ror_w, "ROR.w");
//cpu_test!(roxl_b, "ROXL.b");
//cpu_test!(roxl_l, "ROXL.l");
//cpu_test!(roxl_w, "ROXL.w");
//cpu_test!(roxr_b, "ROXR.b");
//cpu_test!(roxr_l, "ROXR.l");
//cpu_test!(roxr_w, "ROXR.w");
//cpu_test!(rte, "RTE");
//cpu_test!(rtr, "RTR");
//cpu_test!(rts, "RTS");
//cpu_test!(sbcd, "SBCD");
//cpu_test!(scc, "Scc");
//cpu_test!(suba_l, "SUBA.l");
//cpu_test!(suba_w, "SUBA.w");
//cpu_test!(sub_b, "SUB.b");
//cpu_test!(sub_l, "SUB.l");
//cpu_test!(sub_w, "SUB.w");
//cpu_test!(subx_b, "SUBX.b");
//cpu_test!(subx_l, "SUBX.l");
//cpu_test!(subx_w, "SUBX.w");
cpu_test!(swap, "SWAP");
//cpu_test!(tas, "TAS");
//cpu_test!(trap, "TRAP");
//cpu_test!(trapv, "TRAPV");
//cpu_test!(tst_b, "TST.b");
//cpu_test!(tst_l, "TST.l");
//cpu_test!(tst_w, "TST.w");
//cpu_test!(unlink, "UNLINK");
