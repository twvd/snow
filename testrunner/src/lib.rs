use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum TestResult {
    Pass,
    Inconclusive,
    Failed(TestFailure),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum TestFailure {
    ExitCode(i32),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TestReportTest {
    pub name: String,
    pub img_type: String,
    pub model: String,
    pub fn_prefix: String,
    pub result: TestResult,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct TestReport {
    pub tests: Vec<TestReportTest>,
}
