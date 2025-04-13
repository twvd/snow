use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs};

use anyhow::{Context, Result};
use chrono::prelude::*;
use clap::Parser;
use log::*;
use snow_core::mac::MacModel;
use snow_floppy::loaders::FloppyImageLoader;

use snow_floppy::Floppy;
use testrunner::{TestFailure, TestReport, TestReportTest, TestResult};

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[derive(Parser)]
struct Args {
    rom_dir: String,
    floppy_dir: String,
    output_dir: String,

    #[arg(short('j'), default_value_t = 1)]
    parallel: usize,
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
            "{}-{}-{}",
            self.rom.file_stem().unwrap().to_string_lossy(),
            self.floppy
                .as_ref()
                .unwrap()
                .file_stem()
                .unwrap()
                .to_string_lossy(),
            self.floppy
                .as_ref()
                .unwrap()
                .extension()
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

fn write_with_size_limit<P: AsRef<Path>>(
    file_path: P,
    data: &[u8],
    max_size: usize,
) -> io::Result<()> {
    const TRUNCATION_MARKER: &[u8] = b"[TRUNCATED]";

    let mut file = fs::File::create(file_path)?;

    if data.len() <= max_size {
        // If data is smaller than or equal to max_size, write it all
        file.write_all(data)?;
    } else {
        // Write only up to the max_size
        file.write_all(&data[..max_size])?;

        // Append truncation marker
        file.write_all(TRUNCATION_MARKER)?;
    }

    file.flush()?;

    Ok(())
}

pub fn version_string() -> String {
    format!(
        "{}-{}{}",
        built_info::PKG_VERSION,
        built_info::GIT_COMMIT_HASH_SHORT.expect("Git version unavailable"),
        if built_info::GIT_DIRTY.expect("Git version unavailable") {
            "-dirty"
        } else {
            ""
        }
    )
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

        for floppy in floppies
            .iter()
            .filter(|&f| !f.extension().unwrap().to_string_lossy().ends_with("_2"))
        {
            let imgdata = fs::read(floppy)?;
            let Ok(imgtype) = snow_floppy::loaders::Autodetect::detect(&imgdata) else {
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
                // Floppy image incompatible with drive
                continue;
            }

            let mut cycle_fn = floppy.to_path_buf();
            cycle_fn.set_extension("cycles");
            let cycles = fs::read_to_string(cycle_fn)
                .ok()
                .and_then(|i| i.trim().parse::<usize>().ok())
                .unwrap_or(40_000_000);
            tests.push(Test {
                name: floppy.file_stem().unwrap().to_string_lossy().to_string(),
                rom: rom.clone(),
                model,
                floppy: Some(floppy.clone()),
                cycles: match model {
                    MacModel::Early128K | MacModel::Early512K => 128_000_000,
                    MacModel::Plus => 12_000_000,
                    _ => 0,
                } + 172_000_000
                    + cycles,
                floppy_type: Some(imgtype.to_string()),
            });
        }
    }

    let single_bin = get_binary_path("single");
    assert!(single_bin.exists());

    let report = Arc::new(Mutex::new(TestReport {
        run_start: Local::now().to_rfc2822(),
        version: version_string(),
        run_jobs: args.parallel,
        run_cpus: num_cpus::get(),
        ..Default::default()
    }));

    let total_tests = tests.len();
    info!(
        "Collected {} tests, running {} tests in parallel",
        total_tests, args.parallel
    );
    let pool = rusty_pool::ThreadPool::new(args.parallel, args.parallel, Duration::from_secs(60));
    let start_time = Instant::now();

    for (current_test, test) in tests.into_iter().enumerate() {
        let t_report = Arc::clone(&report);
        let t_single_bin = single_bin.clone();
        let t_output_dir = args.output_dir.clone();

        pool.execute(move || {
            info!(
                "({}/{}) Running {} on {} ({}) for {} cycles...",
                current_test, total_tests, test.name, test.model, test, test.cycles
            );

            let mut frame_fn = test.floppy.clone().unwrap();
            frame_fn.set_extension("frame");
            let mut model_frame_fn = test.floppy.clone().unwrap();
            model_frame_fn.set_file_name(format!("{}.frame", test));

            let output = Command::new(&t_single_bin)
                .env("RUST_LOG_STYLE", "never")
                .args([
                    test.rom.to_string_lossy().to_string(),
                    test.floppy.as_ref().unwrap().to_string_lossy().to_string(),
                    test.cycles.to_string(),
                    test.to_string(),
                    if model_frame_fn.exists() {
                        model_frame_fn.to_string_lossy().to_string()
                    } else {
                        frame_fn.to_string_lossy().to_string()
                    },
                    t_output_dir.clone(),
                ])
                .output()
                .expect("Failed to execute command");

            // Save log
            write_with_size_limit(
                format!("{}/{}.log", t_output_dir, test),
                &output.stderr,
                10 * 1024 * 1024,
            )
            .unwrap();

            t_report.lock().unwrap().tests.push(TestReportTest {
                name: test.name.clone(),
                model: test.model.to_string(),
                img_type: test.floppy_type.as_ref().unwrap().to_string(),
                fn_prefix: test.to_string(),
                result: match output.status.code().unwrap_or(255) {
                    0 => {
                        // Success / Ok(())
                        TestResult::Pass
                    }
                    1 => {
                        // Error / Err()
                        TestResult::Failed(TestFailure::ExitCode(output.status.code().unwrap()))
                    }
                    2 => {
                        // Frame not found
                        TestResult::Inconclusive
                    }
                    101 => {
                        // Panic
                        TestResult::Failed(TestFailure::ExitCode(output.status.code().unwrap()))
                    }
                    _ => TestResult::Failed(TestFailure::ExitCode(output.status.code().unwrap())),
                },
            });
        });
    }
    pool.shutdown_join();

    let test_duration = Instant::now() - start_time;
    if let Ok(mut report) = report.as_ref().lock() {
        report.run_duration = format!("{:?}", test_duration);
        fs::write(
            format!("{}/report.json", args.output_dir),
            serde_json::to_string(&*report)?,
        )?;
    } else {
        unreachable!()
    }
    info!("Tests completed in {:?}", test_duration);

    Ok(())
}
