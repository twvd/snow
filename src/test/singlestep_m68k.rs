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

cpu_test!(nop, "NOP");
