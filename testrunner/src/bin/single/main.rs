use std::fs::{self, File};

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
    final_screenshot: String,
    frame_file: String,
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
        cmd.send(EmulatorCommand::InsertFloppy(0, floppy_fn))?;
    }
    cmd.send(EmulatorCommand::Run)?;
    cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Uncapped))?;

    let mut last_frame = None;
    info!("Starting");
    while emulator.get_cycles() < args.cycles {
        while let Ok(frame) = frame_recv.try_recv() {
            last_frame = Some(frame);
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

    let frame = &last_frame
        .as_ref()
        .unwrap()
        .iter()
        .map(|b| b.load(std::sync::atomic::Ordering::Relaxed))
        .collect::<Vec<_>>();

    let mut encoder = png::Encoder::new(
        File::create(args.final_screenshot)?,
        SCREEN_WIDTH as u32,
        SCREEN_HEIGHT as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Best);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(frame)?;
    fs::write(args.frame_file, frame)?;

    Ok(())
}
