mod keymap_sdl;
mod renderer_sdl;
mod ui;

use anyhow::Result;
use clap::Parser;
use hex_literal::hex;
use keymap_sdl::map_sdl_keycode;
use log::*;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
use sdl2::event::{Event, WindowEvent};
use sdl2::mouse::MouseButton;
use sha2::{Digest, Sha256};
use snow_core::emulator::comm::EmulatorCommand;
use snow_core::emulator::Emulator;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::mac::MacModel;
use snow_core::tickable::Tickable;
use ui::UserInterface;

use std::panic::{set_hook, take_hook};
use std::{fs, thread};

use renderer_sdl::{SDLAudioSink, SDLEventPump, SDLRenderer};
use snow_core::renderer::Renderer;

#[derive(Eq, PartialEq, Clone, Copy, clap::ValueEnum)]
enum MouseControl {
    Absolute,
    Relative,
}

#[derive(Eq, PartialEq, Clone, Copy, clap::ValueEnum)]
/// Emulation speed
enum Speed {
    /// Actual speed accurate to the real hardware
    Accurate,
    /// Actual speed when sound is played, otherwise uncapped
    Dynamic,
    /// Uncapped at all times, sound disabled
    Uncapped,
    /// Sync to 60 fps video, sound disabled
    Video,
}

impl From<Speed> for snow_core::emulator::comm::EmulatorSpeed {
    fn from(value: Speed) -> Self {
        match value {
            Speed::Accurate => Self::Accurate,
            Speed::Dynamic => Self::Dynamic,
            Speed::Uncapped => Self::Uncapped,
            Speed::Video => Self::Video,
        }
    }
}

#[derive(Parser)]
#[command(
    about = "Snow - Classic Macintosh emulator",
    author = "Thomas <thomas@thomasw.dev>",
    long_about = None)]
struct Args {
    /// ROM filename to load.
    rom_filename: String,

    /// Initial floppy disk image to load
    floppy_filename: Option<String>,

    /// Trace bus I/O activity
    #[arg(long)]
    trace: bool,

    /// Do not run emulator on start
    #[arg(short, long)]
    stop: bool,

    /// Mouse motion control method
    #[arg(long, value_enum, default_value_t=MouseControl::Absolute)]
    mouse: MouseControl,

    /// Scaling factor for the display
    #[arg(long, default_value_t = 2)]
    scale: usize,

    /// Emulation speed
    #[arg(long, value_enum, default_value_t=Speed::Accurate)]
    speed: Speed,
}

/// Sets up a panic handler that restores the terminal back to the original state
/// so any panics are readable and the terminal is usable.
fn setup_panic_handler() {
    let original_hook = take_hook();
    set_hook(Box::new(move |panic_info| {
        // intentionally ignore errors here since we're already in a panic
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);

        // Exit the process in case this doesn't happen on the main thread
        std::process::exit(1);
    }));
}

fn main() -> Result<()> {
    setup_panic_handler();
    let args = Args::parse();

    // Initialize logging
    tui_logger::init_logger(log::LevelFilter::Trace).unwrap();
    tui_logger::set_default_level(log::LevelFilter::Trace);

    // Initialize display
    let mut disp_win_width = SCREEN_WIDTH * args.scale;
    let mut disp_win_height = SCREEN_HEIGHT * args.scale;
    let mut renderer = SDLRenderer::new(SCREEN_WIDTH, SCREEN_HEIGHT)?;
    renderer.set_window_size(disp_win_width, disp_win_height)?;
    let eventpump = SDLEventPump::new();

    // Initialize ROM
    let rom = fs::read(&args.rom_filename)?;

    // Detect model
    // TODO Make this nicer
    let mut hash = Sha256::new();
    hash.update(&rom);
    let digest = hash.finalize();
    let model = if digest[..]
        == hex!("13fe8312cf6167a2bb4351297b48cc1ee29c523b788e58270434742bfeda864c")
    {
        // Macintosh 128K
        MacModel::Early128K
    } else if digest[..] == hex!("fe6a1ceff5b3eefe32f20efea967cdf8cd4cada291ede040600e7f6c9e2dfc0e")
    {
        // Macintosh 512K
        MacModel::Early512K
    } else if
    // Macintosh Plus v1
    digest[..] == hex!("c5d862605867381af6200dd52f5004cc00304a36ab996531f15e0b1f8a80bc01") ||
        // Macintosh Plus v2
    digest[..] == hex!("06f598ff0f64c944e7c347ba55ae60c792824c09c74f4a55a32c0141bf91b8b3") ||
        // Macintosh Plus v3
    digest[..] == hex!("dd908e2b65772a6b1f0c859c24e9a0d3dcde17b1c6a24f4abd8955846d7895e7")
    {
        MacModel::Plus
    } else if digest[..] == hex!("0dea05180e66fddb5f5577c89418de31b97e2d9dc6affe84871b031df8245487")
    {
        MacModel::SE
    } else if digest[..] == hex!("c1c47260bacac2473e21849925fbfdf48e5ab584aaef7c6d54569d0cb6b41cce")
    {
        MacModel::Classic
    } else {
        panic!("Cannot determine model from ROM file")
    };

    // Initialize emulator
    let (mut emulator, frame_recv) = Emulator::new(&rom, model)?;
    let cmd = emulator.create_cmd_sender();
    if let Some(floppy_fn) = args.floppy_filename {
        cmd.send(EmulatorCommand::InsertFloppy(0, floppy_fn))?;
    }
    if !args.stop {
        cmd.send(EmulatorCommand::Run)?;
    }
    cmd.send(EmulatorCommand::SetSpeed(args.speed.into()))?;

    // Initialize audio
    let _audiodev = SDLAudioSink::new(emulator.get_audio())?;

    // Initialize user interface
    let mut terminal = UserInterface::init_terminal()?;
    let mut ui = UserInterface::new(
        &args.rom_filename,
        &model.to_string(),
        emulator.create_event_recv(),
        emulator.create_cmd_sender(),
    )?;

    // Spin up emulator thread
    let emuthread = thread::spawn(move || loop {
        setup_panic_handler();

        match emulator.tick(1) {
            Ok(0) => break,
            Ok(_) => (),
            Err(e) => panic!("Emulator error: {}", e),
        }
    });

    'mainloop: loop {
        // Render frame to SDL window
        if let Ok(frame) = frame_recv.try_recv() {
            renderer.update_from(&frame)?;
        }

        // Draw TUI
        if !ui.run(&mut terminal)? {
            break 'mainloop;
        }

        // Process SDL events
        while let Some(event) = eventpump.wait(10) {
            match event {
                Event::Quit { .. } => {
                    break 'mainloop;
                }
                Event::KeyDown {
                    keycode: Some(k), ..
                } => {
                    let Some(mac_keycode) = map_sdl_keycode(k) else {
                        warn!("Unknown SDL keycode: {:?} ({})", k, k.name());
                        continue;
                    };

                    cmd.send(EmulatorCommand::KeyEvent(
                        snow_core::keymap::KeyEvent::KeyDown(mac_keycode),
                    ))?;
                }
                Event::KeyUp {
                    keycode: Some(k), ..
                } => {
                    let Some(mac_keycode) = map_sdl_keycode(k) else {
                        warn!("Unknown SDL keycode: {:?} ({})", k, k.name());
                        continue;
                    };

                    cmd.send(EmulatorCommand::KeyEvent(
                        snow_core::keymap::KeyEvent::KeyUp(mac_keycode),
                    ))?;
                }
                Event::MouseMotion {
                    x, y, xrel, yrel, ..
                } => match args.mouse {
                    MouseControl::Absolute => cmd.send(EmulatorCommand::MouseUpdateAbsolute {
                        x: (x as f32 / (disp_win_width as f32 / SCREEN_WIDTH as f32)) as u16,
                        y: (y as f32 / (disp_win_height as f32 / SCREEN_HEIGHT as f32)) as u16,
                    })?,
                    MouseControl::Relative => cmd.send(EmulatorCommand::MouseUpdateRelative {
                        relx: xrel.try_into()?,
                        rely: yrel.try_into()?,
                        btn: None,
                    })?,
                },
                Event::MouseButtonDown {
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    cmd.send(EmulatorCommand::MouseUpdateRelative {
                        relx: 0,
                        rely: 0,
                        btn: Some(true),
                    })?;
                }
                Event::MouseButtonUp {
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    cmd.send(EmulatorCommand::MouseUpdateRelative {
                        relx: 0,
                        rely: 0,
                        btn: Some(false),
                    })?;
                }
                Event::Window {
                    win_event: WindowEvent::Resized(w, h),
                    ..
                } => {
                    disp_win_width = w as usize;
                    disp_win_height = h as usize;
                }
                _ => (),
            }
        }
    }

    // Terminate emulator
    cmd.send(EmulatorCommand::Quit)?;
    emuthread.join().unwrap();

    // Clean up terminal
    UserInterface::shutdown_terminal(&mut terminal)?;
    Ok(())
}
