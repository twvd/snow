//! Headless runner for the guest testers.
//!
//! Usage: mac-tests-runner <rom> <cycles> <screenshot.png> [<scsi id>:<image>...]
//!
//! Boots the ROM with the given SCSI disks, runs the given number of cycles, uncapped, then
//! saves the last frame. It stops early, with an error, if the emulator stops.
//!
//! Environment:
//! - `RUNNER_MODEL`: the model, as Snow names it (default: the one the ROM is for)
//! - `RUNNER_RAM`: the RAM size in MB (default: the model's)
//! - `RUNNER_PMMU`: set to enable the 68851 PMMU of a Mac II
//! - `RUNNER_FPU`: set to enable the FPU where it is optional
//! - `RUNNER_PRAM`: a file to keep the PRAM in
//! - `RUNNER_DISPLAY_ROM`: the ROM of a Macintosh Display Card 8-24, or of the SE/30's video
//! - `RUNNER_FLOPPY`: a floppy image inserted at start, write-protected
//! - `RUNNER_OVERCLOCK`: the CPU clock in Hz
//! - `RUNNER_SHOTS`: set to also save a frame every 1/8 of the run, as
//!   `<screenshot>-<n>.png`
//! - `RUNNER_UNTIL`: a text the guest writes when it is done: the run stops once the image of
//!   the disk at SCSI ID 0 holds it more times than at the start (checked every 1G cycles).
//!   Snow's disks are files mapped in memory, so what the guest writes is in the file at once.
//!
//! Built with the `afp` feature, it puts the built-in AppleTalk node on SCC channel B (the
//! printer port) when `SNOW_AFP_ROOT` is set, sharing that folder as a read-only AFP volume.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use snow_core::emulator::Emulator;
use snow_core::emulator::comm::{EmulatorCommand, EmulatorEvent, EmulatorSpeed};
use snow_core::mac::{ExtraROMs, MacModel};
use snow_core::tickable::{Tickable, Ticks};

fn save_png(path: &str, width: u16, height: u16, rgba: &[u8]) -> Result<()> {
    let mut encoder = png::Encoder::new(File::create(path)?, width.into(), height.into());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}

/// The value of an environment variable, if it is set
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Cycles between two looks for `RUNNER_UNTIL`
const UNTIL_PERIOD: Ticks = 1_000_000_000;

/// How many times a text is in a file
fn count_in_file(path: &Path, text: &[u8]) -> Result<usize> {
    let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(data.windows(text.len()).filter(|w| *w == text).count())
}

#[cfg(feature = "afp")]
fn enable_appletalk(cmd: &snow_core::emulator::comm::EmulatorCommandSender) -> Result<()> {
    use snow_core::mac::scc::SccCh;
    use snow_core::mac::serial_bridge::SerialBridgeConfig;

    let Some(root) = std::env::var_os("SNOW_AFP_ROOT") else {
        return Ok(());
    };
    cmd.send(EmulatorCommand::SerialBridgeEnable(
        SccCh::B,
        SerialBridgeConfig::AppleTalk {
            root: PathBuf::from(root),
            volume_name: None,
            max_version: Default::default(),
            settings: Default::default(),
        },
    ))?;
    Ok(())
}

fn main() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        bail!("usage: mac-tests-runner <rom> <cycles> <screenshot.png> [<scsi id>:<image>...]");
    }
    let rom = fs::read(&args[1]).with_context(|| format!("reading {}", args[1]))?;
    let cycles: Ticks = args[2].parse().context("cycle count")?;
    let screenshot = &args[3];
    let model = match env("RUNNER_MODEL") {
        Some(name) => name
            .parse::<MacModel>()
            .map_err(|_| anyhow!("unknown model {name}"))?,
        None => MacModel::detect_from_rom(&rom).ok_or_else(|| anyhow!("unknown ROM {}", args[1]))?,
    };
    let ram = match env("RUNNER_RAM") {
        Some(mb) => Some(mb.parse::<usize>().context("RUNNER_RAM")? * 1024 * 1024),
        None => None,
    };
    let display_rom = match env("RUNNER_DISPLAY_ROM") {
        Some(path) => Some(fs::read(&path).with_context(|| format!("reading {path}"))?),
        None => None,
    };
    let extra: Vec<ExtraROMs> = display_rom
        .as_deref()
        .map(|rom| {
            if model == MacModel::SE30 {
                ExtraROMs::SE30Video(rom)
            } else {
                ExtraROMs::MDC12(rom)
            }
        })
        .into_iter()
        .collect();

    let (mut emu, frame_recv) = Emulator::new_with_extra(
        &rom,
        &extra,
        model,
        None,
        Default::default(),
        ram,
        None,
        env("RUNNER_PMMU").is_some(),
        env("RUNNER_FPU").is_some(),
        None,
        None,
        None,
    )?;
    if let Some(path) = env("RUNNER_PRAM") {
        emu.persist_pram(Path::new(&path));
    }
    let cmd = emu.create_cmd_sender();
    let events = emu.create_event_recv();
    let mut boot_disk = None;
    for disk in &args[4..] {
        let Some((id, path)) = disk.split_once(':') else {
            bail!("expected <scsi id>:<image>, got {disk}");
        };
        if id == "0" {
            boot_disk = Some(PathBuf::from(path));
        }
        cmd.send(EmulatorCommand::ScsiAttachHdd(
            id.parse()?,
            PathBuf::from(path),
        ))?;
    }
    if let Some(path) = env("RUNNER_FLOPPY") {
        cmd.send(EmulatorCommand::InsertFloppy(0, path, true))?;
    }
    if let Some(hz) = env("RUNNER_OVERCLOCK") {
        cmd.send(EmulatorCommand::SetOverclock(Some(
            hz.parse().context("RUNNER_OVERCLOCK")?,
        )))?;
    }
    #[cfg(feature = "afp")]
    enable_appletalk(&cmd)?;
    cmd.send(EmulatorCommand::Run)?;
    cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Uncapped))?;

    let until = match env("RUNNER_UNTIL") {
        Some(text) if !text.is_empty() => {
            let Some(disk) = boot_disk else {
                bail!("RUNNER_UNTIL needs a disk at SCSI ID 0");
            };
            let before = count_in_file(&disk, text.as_bytes())?;
            Some((disk, text, before))
        }
        _ => None,
    };
    let mut next_until = UNTIL_PERIOD;

    let shots = env("RUNNER_SHOTS").is_some();
    let mut last_frame = None;
    let mut next_report = cycles / 8;
    let mut report = 0;
    // Status reports from before the Run command say stopped too
    let mut started = false;
    while emu.get_cycles() < cycles {
        if let Some(frame) = frame_recv.lock().unwrap().take() {
            last_frame = Some(frame);
        }
        while let Ok(event) = events.try_recv() {
            match event {
                EmulatorEvent::Status(status) => {
                    if status.running {
                        started = true;
                    } else if started {
                        bail!("the emulator stopped at cycle {}", status.cycles);
                    }
                }
                EmulatorEvent::UserMessage(_, message) => log::info!("{message}"),
                _ => (),
            }
        }
        if emu.get_cycles() >= next_report {
            log::info!("cycle {}", emu.get_cycles());
            if shots && let Some(frame) = &last_frame {
                let path = format!("{}-{report}.png", screenshot.trim_end_matches(".png"));
                save_png(&path, frame.width(), frame.height(), frame)?;
            }
            next_report += cycles / 8;
            report += 1;
        }
        if emu.get_cycles() >= next_until {
            next_until += UNTIL_PERIOD;
            if let Some((disk, text, before)) = &until
                && count_in_file(disk, text.as_bytes())? > *before
            {
                log::info!("\"{text}\" written at cycle {}: done", emu.get_cycles());
                break;
            }
        }
        emu.tick(1, ())?;
    }

    if let Some(frame) = last_frame {
        let (width, height) = (frame.width(), frame.height());
        save_png(screenshot, width, height, &frame.into_inner())?;
    }
    Ok(())
}
