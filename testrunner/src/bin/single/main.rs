use std::collections::VecDeque;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Result};
use clap::Parser;
use itertools::Itertools;
use log::*;

use snow_core::emulator::comm::{EmulatorCommand, EmulatorEvent, EmulatorSpeed};
use snow_core::emulator::Emulator;
use snow_core::mac::MacModel;
use snow_core::tickable::{Tickable, Ticks};

#[derive(Parser)]
struct Args {
    rom: String,
    floppy: Option<String>,
    cycles: Ticks,
    fn_prefix: String,
    control_frame: String,
    out_dir: String,
}

fn deduplicate_with_counts<T: Clone + Eq>(arr: &[T]) -> Vec<(T, usize)> {
    arr.iter()
        .chunk_by(|&x| x.clone())
        .into_iter()
        .map(|(key, group)| (key, group.count()))
        .collect()
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

    // Load control frame
    let control_frame = fs::read(&args.control_frame).ok();
    if control_frame.is_none() {
        warn!(
            "Could not load control frame: {}",
            PathBuf::from_str(&args.control_frame)?
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );
    }

    let mut fullgifencoder = gif::Encoder::new(
        File::create(format!("{}/{}-full.gif", args.out_dir, args.fn_prefix))?,
        model.display_width(),
        model.display_height(),
        &[],
    )?;
    fullgifencoder.set_repeat(gif::Repeat::Infinite)?;

    let mut frames = VecDeque::<Vec<u8>>::new();
    let mut last_delay = 0;
    let mut control_seen = false;
    info!("Starting");
    while emulator.get_cycles() < args.cycles {
        while let Ok(oframe) = frame_recv.try_recv() {
            let frame = oframe
                .iter()
                .map(|b| b.load(std::sync::atomic::Ordering::Relaxed))
                .collect::<Vec<_>>();

            if !frames.is_empty() && frame == *frames.back().unwrap() {
                last_delay += 1;
            } else {
                let mut fcopy = frame.clone();
                let mut gframe = gif::Frame::from_rgba(
                    model.display_width(),
                    model.display_height(),
                    &mut fcopy,
                );
                gframe.delay = last_delay;
                fullgifencoder.write_frame(&gframe)?;
                last_delay = 0;
            }

            if let Some(cf) = control_frame.as_ref() {
                control_seen |= *cf == frame;
            }
            while frames.len() >= 120 {
                frames.pop_front();
            }
            frames.push_back(frame);
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
        // Finish full recording
        if last_delay > 0 {
            let mut fcopy = frames.back().unwrap().clone();
            let mut gframe =
                gif::Frame::from_rgba(model.display_width(), model.display_height(), &mut fcopy);
            gframe.delay = last_delay;
            fullgifencoder.write_frame(&gframe)?;
        }

        // Write still screenshot
        let frame = frames.back().unwrap();
        let mut encoder = png::Encoder::new(
            File::create(format!("{}/{}.png", args.out_dir, args.fn_prefix))?,
            model.display_width() as u32,
            model.display_height() as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Best);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(frame)?;
        fs::write(format!("{}/{}.frame", args.out_dir, args.fn_prefix), frame)?;

        // Write animated short
        write_gif(
            format!("{}/{}.gif", args.out_dir, args.fn_prefix),
            model.display_width(),
            model.display_height(),
            &mut frames,
        )?;
    }

    if !control_seen {
        std::process::exit(2);
    }
    Ok(())
}

fn write_gif<P: AsRef<Path>>(
    file: P,
    width: u16,
    height: u16,
    frames: &mut VecDeque<Vec<u8>>,
) -> Result<(), anyhow::Error> {
    let gifout = File::create(file)?;
    let mut gifencoder = gif::Encoder::new(gifout, width, height, &[])?;
    gifencoder.set_repeat(gif::Repeat::Infinite)?;
    let mut dedup_frames = deduplicate_with_counts(frames.make_contiguous());
    for oframe in &mut dedup_frames {
        let mut gframe = gif::Frame::from_rgba(width, height, &mut oframe.0);
        gframe.delay = oframe.1 as u16;
        gifencoder.write_frame(&gframe)?;
    }
    info!(
        "deduplicated {} frames to {}",
        frames.len(),
        dedup_frames.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup_empty_array() {
        let arr: Vec<i32> = vec![];
        let result = deduplicate_with_counts(&arr);
        assert_eq!(result, vec![]);
    }

    #[test]
    fn test_dedup_single_element() {
        let arr = vec![42];
        let result = deduplicate_with_counts(&arr);
        assert_eq!(result, vec![(42, 1)]);
    }

    #[test]
    fn test_dedup_consecutive_duplicates() {
        let arr = vec![1, 1, 1];
        let result = deduplicate_with_counts(&arr);
        assert_eq!(result, vec![(1, 3)]);
    }

    #[test]
    fn test_dedup_mixed_elements() {
        let arr = vec![1, 2, 3, 4];
        let result = deduplicate_with_counts(&arr);
        assert_eq!(result, vec![(1, 1), (2, 1), (3, 1), (4, 1)]);
    }

    #[test]
    fn test_dedup_realistic() {
        let arr = vec![1, 1, 1, 5, 6, 7, 7, 1];
        let result = deduplicate_with_counts(&arr);
        assert_eq!(result, vec![(1, 3), (5, 1), (6, 1), (7, 2), (1, 1)]);
    }

    #[test]
    fn test_dedup_different_data_types() {
        let arr = vec!["a", "a", "b", "c", "c"];
        let result = deduplicate_with_counts(&arr);
        assert_eq!(result, vec![("a", 2), ("b", 1), ("c", 2)]);
    }

    #[test]
    fn test_dedup_alternating_elements() {
        let arr = vec![1, 2, 1, 2, 1];
        let result = deduplicate_with_counts(&arr);
        assert_eq!(result, vec![(1, 1), (2, 1), (1, 1), (2, 1), (1, 1)]);
    }
}
