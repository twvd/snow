use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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

fn compare_frames(one: &Path, two: &Path) -> bool {
    let Ok(a) = fs::read(one) else {
        error!("Cannot read {}", one.display());
        return false;
    };
    let Ok(b) = fs::read(two) else {
        error!("Cannot read {}", two.display());
        return false;
    };
    a == b
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
                name: img.get_title().to_string(),
                rom: rom.clone(),
                model,
                floppy: Some(floppy.clone()),
                cycles: match model {
                    MacModel::Early128K | MacModel::Early512K => 128_000_000,
                    MacModel::Plus => 12_000_000,
                    _ => 0,
                } + 156_000_000
                    + cycles,
                floppy_type: Some(imgtype.to_string()),
            });
        }
    }

    let single_bin = get_binary_path("single");
    assert!(single_bin.exists());

    let report = Arc::new(Mutex::new(TestReport::default()));

    info!(
        "Collected {} tests, running {} tests in parallel",
        tests.len(),
        args.parallel
    );
    let pool = rusty_pool::ThreadPool::new(args.parallel, args.parallel, Duration::from_secs(60));
    let start_time = Instant::now();

    for test in tests {
        let t_report = Arc::clone(&report);
        let t_single_bin = single_bin.clone();
        let t_output_dir = args.output_dir.clone();

        pool.execute(move || {
            info!(
                "Running {} on {} ({}) for {} cycles...",
                test.name, test.model, test, test.cycles
            );

            let out_frame_fn = PathBuf::from(format!("{}/{}.frame", t_output_dir, test));
            let output = Command::new(&t_single_bin)
                .env("RUST_LOG_STYLE", "never")
                .args([
                    test.rom.to_string_lossy().to_string(),
                    test.floppy.as_ref().unwrap().to_string_lossy().to_string(),
                    test.cycles.to_string(),
                    test.to_string(),
                    t_output_dir.clone(),
                ])
                .output()
                .expect("Failed to execute command");
            fs::write(format!("{}/{}.log", t_output_dir, test), output.stderr).unwrap();

            t_report.lock().unwrap().tests.push(TestReportTest {
                name: test.name.clone(),
                model: test.model.to_string(),
                img_type: test.floppy_type.as_ref().unwrap().to_string(),
                fn_prefix: test.to_string(),
                result: if output.status.success() {
                    let mut frame_fn = test.floppy.clone().unwrap();
                    frame_fn.set_extension("frame");
                    let mut model_frame_fn = test.floppy.clone().unwrap();
                    model_frame_fn.set_file_name(format!("{}.frame", test));
                    if compare_frames(&model_frame_fn, &out_frame_fn)
                        || compare_frames(&frame_fn, &out_frame_fn)
                    {
                        TestResult::Pass
                    } else {
                        TestResult::Inconclusive
                    }
                } else {
                    TestResult::Failed(TestFailure::ExitCode(output.status.code().unwrap()))
                },
            });
        });
    }
    pool.shutdown_join();

    fs::write(
        format!("{}/report.json", args.output_dir),
        serde_json::to_string(&*report.as_ref().lock().unwrap())?,
    )?;
    info!("Tests completed in {:?}", Instant::now() - start_time);

    Ok(())
}
