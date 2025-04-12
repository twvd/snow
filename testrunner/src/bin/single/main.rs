use std::collections::VecDeque;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Result};
use clap::Parser;
use log::*;

use snow_core::emulator::comm::{EmulatorCommand, EmulatorEvent, EmulatorSpeed};
use snow_core::emulator::Emulator;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::mac::MacModel;
use snow_core::tickable::{Tickable, Ticks};

#[derive(Parser)]
struct Args {
    rom: String,
    floppy: Option<String>,
    cycles: Ticks,
    fn_prefix: String,
    out_dir: String,
}

fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Debug)
        .init();
    let args = Args::parse();

    // Initialize ROM
    let rom = fs::read(&args.rom)?;
    let model = MacModel::detect_from_rom(&rom).expect("Cannot detect model from ROM file");

    // Initialize emulator
    let (mut emulator, frame_recv) = Emulator::new(&rom, model)?;
    let cmd = emulator.create_cmd_sender();
    let event_recv = emulator.create_event_recv();
    if let Some(floppy_fn) = args.floppy {
        // Model-specific replay file
        let mut model_replay_fn = PathBuf::from_str(&floppy_fn)?;
        model_replay_fn.set_file_name(&args.fn_prefix);
        model_replay_fn.set_extension("snowr");
        // Generic replay file
        let mut replay_fn = PathBuf::from_str(&floppy_fn)?;
        replay_fn.set_extension("snowr");

        // Secondary floppy disk
        let mut secondary_fn = PathBuf::from_str(&floppy_fn)?;
        secondary_fn.set_extension(format!(
            "{}_2",
            secondary_fn.extension().unwrap().to_string_lossy()
        ));

        cmd.send(EmulatorCommand::InsertFloppy(0, floppy_fn))?;
        if secondary_fn.exists() {
            cmd.send(EmulatorCommand::InsertFloppy(
                1,
                secondary_fn.to_string_lossy().to_string(),
            ))?;
        }

        // See if there's a replay file
        let try_replay = |replay_fn: &Path| -> Result<bool> {
            if replay_fn.exists() {
                let recording = serde_json::from_reader(fs::File::open(replay_fn)?)?;
                cmd.send(EmulatorCommand::ReplayInputRecording(recording, false))?;
                info!("Loaded recording file '{}'", replay_fn.display());
                Ok(true)
            } else {
                Ok(false)
            }
        };

        #[allow(clippy::if_same_then_else)]
        if try_replay(&model_replay_fn)? {
        } else if try_replay(&replay_fn)? {
        } else {
            info!("No replay file found");
        }
    }
    cmd.send(EmulatorCommand::Run)?;
    cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Uncapped))?;

    let mut frames = VecDeque::<Vec<u8>>::new();
    info!("Starting");
    while emulator.get_cycles() < args.cycles {
        while let Ok(frame) = frame_recv.try_recv() {
            while frames.len() >= 30 {
                frames.pop_front();
            }
            frames.push_back(
                frame
                    .iter()
                    .map(|b| b.load(std::sync::atomic::Ordering::Relaxed))
                    .collect::<Vec<_>>(),
            );
        }
        while let Ok(event) = event_recv.try_recv() {
            match event {
                EmulatorEvent::Memory(_) => (),
                EmulatorEvent::Status(s) => {
                    info!("Event: Status: {:?}", s);
                    if !s.running && s.cycles > 100 {
                        bail!("Emulator stopped");
                    }
                }
                _ => info!("Event: {}", event),
            }
        }
        emulator.tick(1)?;
    }

    if !frames.is_empty() {
        // Write still screenshot
        let frame = frames.back().unwrap();
        let mut encoder = png::Encoder::new(
            File::create(format!("{}/{}.png", args.out_dir, args.fn_prefix))?,
            SCREEN_WIDTH as u32,
            SCREEN_HEIGHT as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Best);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(frame)?;
        fs::write(format!("{}/{}.frame", args.out_dir, args.fn_prefix), frame)?;

        // Write animated GIF
        let gifout = File::create(format!("{}/{}.gif", args.out_dir, args.fn_prefix))?;
        let mut gifencoder = gif::Encoder::new(
            gifout,
            SCREEN_WIDTH.try_into()?,
            SCREEN_HEIGHT.try_into()?,
            &[],
        )?;
        gifencoder.set_repeat(gif::Repeat::Infinite)?;
        while let Some(mut frame) = frames.pop_front() {
            let mut gframe = gif::Frame::from_rgba(
                SCREEN_WIDTH.try_into()?,
                SCREEN_HEIGHT.try_into()?,
                &mut frame,
            );
            gframe.delay = 1;
            gifencoder.write_frame(&gframe)?;
        }
    }

    Ok(())
}
