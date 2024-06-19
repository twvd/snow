use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::Value;

use std::fs;

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
    prefetch: [u32; 2],
    ram: Vec<(u32, u32)>,
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
    length: u64,
}

macro_rules! cpu_test {
    ($testfn:ident, $instr:expr) => {
        #[test]
        fn $testfn() {
            let filename = format!("testdata/680x0/68000/v1/{}.json.gz", $instr);
            let testcases: Vec<Testcase> =
                serde_json::from_reader(GzDecoder::new(fs::File::open(filename).unwrap())).unwrap();

            for testcase in testcases {
                run_testcase(testcase);
            }
        }
    };
}

fn run_testcase(testcase: Testcase) {
    dbg!(&testcase);
    panic!("");
}

cpu_test!(abcd, "ABCD");
cpu_test!(add_b, "ADD.b");
