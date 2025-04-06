use std::path::PathBuf;
use std::process::Command;
use std::{env, fs};

use anyhow::{Context, Result};
use clap::Parser;
use log::*;
use snow_core::mac::MacModel;
use snow_floppy::loaders::FloppyImageLoader;

use snow_floppy::Floppy;
use testrunner::{TestFailure, TestReport, TestReportTest, TestResult};

#[derive(Parser)]
struct Args {
    rom_dir: String,
    floppy_dir: String,
    output_dir: String,
}

struct Test {
    name: String,
    model: MacModel,
    rom: PathBuf,
    floppy: Option<PathBuf>,
    floppy_type: Option<String>,
    cycles: usize,
}

impl std::fmt::Display for Test {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}-{}",
            self.rom.file_stem().unwrap().to_string_lossy(),
            self.floppy
                .as_ref()
                .unwrap()
                .file_stem()
                .unwrap()
                .to_string_lossy()
        )
    }
}

fn get_binary_path(binary_name: &str) -> PathBuf {
    // First try using Cargo environment variable
    if let Ok(path) = env::var(format!("CARGO_BIN_EXE_{}", binary_name)) {
        return PathBuf::from(path);
    }

    // Otherwise find it relative to the current executable
    let current_exe = env::current_exe().expect("Failed to get current executable path");
    let bin_dir = current_exe
        .parent()
        .expect("Failed to get binary directory");
    bin_dir.join(binary_name)
}

fn main() -> Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .init();
    let args = Args::parse();
    let mut tests = vec![];

    // Collect tests
    let roms = fs::read_dir(args.rom_dir)?
        .map(|res| res.map(|e| e.path()))
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    let floppies = fs::read_dir(args.floppy_dir)?
        .map(|res| res.map(|e| e.path()))
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    for rom in &roms {
        let model = MacModel::detect_from_rom(&fs::read(rom)?).context("Cannot detect ROM")?;

        for floppy in &floppies {
            let imgdata = fs::read(floppy)?;
            let Ok(imgtype) = snow_floppy::loaders::Autodetect::detect(&imgdata) else {
                error!("Cannot load floppy: {}", floppy.to_string_lossy());
                continue;
            };
            let Ok(img) = snow_floppy::loaders::Autodetect::load(
                &imgdata,
                Some(&floppy.file_name().unwrap().to_string_lossy()),
            ) else {
                error!("Cannot load floppy: {}", floppy.to_string_lossy());
                continue;
            };

            if !model.fdd_drives()[0]
                .compatible_floppies()
                .contains(&img.get_type())
            {
                continue;
            }

            tests.push(Test {
                name: img.get_title().to_string(),
                rom: rom.clone(),
                model,
                floppy: Some(floppy.clone()),
                cycles: match model {
                    MacModel::Early128K | MacModel::Early512K => 320_000_000,
                    _ => 800_000_000,
                },
                floppy_type: Some(imgtype.to_string()),
            });
        }
    }

    let single_bin = get_binary_path("single");
    assert!(single_bin.exists());

    let mut report = TestReport::default();

    info!("Collected {} tests", tests.len());
    for test in tests {
        info!("Running {} on {} ({})...", test.name, test.model, test);

        let output = Command::new(&single_bin)
            .env("RUST_LOG_STYLE", "never")
            .args([
                test.rom.to_string_lossy().to_string(),
                test.floppy.as_ref().unwrap().to_string_lossy().to_string(),
                test.cycles.to_string(),
                format!("{}/{}.png", args.output_dir, test),
            ])
            .output()
            .expect("Failed to execute command");
        fs::write(format!("{}/{}.log", args.output_dir, test), output.stderr)?;

        report.tests.push(TestReportTest {
            name: test.name.clone(),
            model: test.model.to_string(),
            img_type: test.floppy_type.as_ref().unwrap().to_string(),
            fn_prefix: test.to_string(),
            result: if output.status.success() {
                TestResult::Inconclusive
            } else {
                TestResult::Failed(TestFailure::ExitCode(output.status.code().unwrap()))
            },
        });
    }

    fs::write(
        format!("{}/report.json", args.output_dir),
        serde_json::to_string(&report)?,
    )?;

    Ok(())
}
