use std::path::PathBuf;
use std::process::Command;
use std::{env, fs};

use anyhow::Result;
use clap::Parser;
use log::*;

#[derive(Parser)]
struct Args {
    rom_dir: String,
    floppy_dir: String,
    output_dir: String,
}

struct Test {
    rom: PathBuf,
    floppy: Option<PathBuf>,
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
        for floppy in &floppies {
            tests.push(Test {
                rom: rom.clone(),
                floppy: Some(floppy.clone()),
                cycles: 20_000_000,
            });
        }
    }

    let single_bin = get_binary_path("single");
    assert!(single_bin.exists());

    info!("Collected {} tests", tests.len());
    for test in tests {
        info!("Running {}...", test);

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
    }

    Ok(())
}
