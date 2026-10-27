//! Headless runner for the ATalkTest guest.
//!
//! Usage: atalk-runner <rom> <cycles> <screenshot.png> <scsi id>:<image>...
//!
//! Boots the ROM with the given SCSI disks and the built-in AppleTalk
//! node on SCC channel B (the printer port), sharing SNOW_AFP_ROOT as a
//! read-only AFP volume. Runs the given number of cycles, uncapped, then
//! saves the last frame.

use std::fs::{self, File};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use snow_core::emulator::Emulator;
use snow_core::emulator::comm::{EmulatorCommand, EmulatorEvent, EmulatorSpeed};
use snow_core::mac::MacModel;
use snow_core::mac::scc::SccCh;
use snow_core::mac::serial_bridge::SerialBridgeConfig;
use snow_core::tickable::{Tickable, Ticks};

fn save_png(path: &str, width: u16, height: u16, rgba: &[u8]) -> Result<()> {
    let mut encoder = png::Encoder::new(File::create(path)?, width.into(), height.into());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}

fn main() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        bail!("usage: atalk-runner <rom> <cycles> <screenshot.png> <scsi id>:<image>...");
    }
    let rom = fs::read(&args[1]).with_context(|| format!("reading {}", args[1]))?;
    let cycles: Ticks = args[2].parse().context("cycle count")?;
    let screenshot = &args[3];
    let Some(model) = MacModel::detect_from_rom(&rom) else {
        bail!("unknown ROM {}", args[1]);
    };

    let (mut emu, frame_recv) = Emulator::new(&rom, &[], model)?;
    let cmd = emu.create_cmd_sender();
    let events = emu.create_event_recv();
    for disk in &args[4..] {
        let Some((id, path)) = disk.split_once(':') else {
            bail!("expected <scsi id>:<image>, got {disk}");
        };
        cmd.send(EmulatorCommand::ScsiAttachHdd(
            id.parse()?,
            PathBuf::from(path),
        ))?;
    }
    // The server shares SNOW_AFP_ROOT
    cmd.send(EmulatorCommand::SerialBridgeEnable(
        SccCh::B,
        SerialBridgeConfig::AppleTalk,
    ))?;
    cmd.send(EmulatorCommand::Run)?;
    cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Uncapped))?;

    let mut last_frame = None;
    let mut next_report = cycles / 8;
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
            next_report += cycles / 8;
        }
        emu.tick(1, ())?;
    }

    if let Some(frame) = last_frame {
        let (width, height) = (frame.width(), frame.height());
        save_png(screenshot, width, height, &frame.into_inner())?;
    }
    Ok(())
}
