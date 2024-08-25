#![allow(clippy::iter_nth_zero)]

pub mod ui;

use anyhow::Result;
use clap::Parser;
use hex_literal::hex;
use log::*;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::mouse::MouseButton;
use sha2::{Digest, Sha256};
use snow_core::emulator::comm::EmulatorCommand;
use snow_core::emulator::{Emulator, MacModel};
use snow_core::mac::keyboard::{self, Keyboard};
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::tickable::Tickable;
use ui::UserInterface;

use std::panic::{set_hook, take_hook};
use std::time::{Duration, Instant};
use std::{fs, thread};

use snow_core::frontend::sdl::{SDLEventPump, SDLRenderer};
use snow_core::frontend::Renderer;

#[derive(Eq, PartialEq, Clone, Copy, clap::ValueEnum)]
enum MouseControl {
    Absolute,
    Relative,
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

    /// Run emulator on start
    #[arg(short, long)]
    run: bool,

    /// Mouse motion control method
    #[arg(long, value_enum, default_value_t=MouseControl::Absolute)]
    mouse: MouseControl,
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
    }));
}

fn map_sdl_keycode(kc: Keycode) -> Option<u8> {
    match kc {
        Keycode::BACKSPACE => Some(keyboard::SC_BACKSPACE),
        Keycode::TAB => Some(keyboard::SC_TAB),
        Keycode::CAPSLOCK => Some(keyboard::SC_CAPSLOCK),
        Keycode::RETURN | Keycode::RETURN2 => Some(keyboard::SC_RETURN),
        Keycode::LSHIFT | Keycode::RSHIFT => Some(keyboard::SC_SHIFT),
        Keycode::LALT | Keycode::RALT => Some(keyboard::SC_OPTION),
        Keycode::LCTRL | Keycode::RCTRL => Some(keyboard::SC_APPLE),
        Keycode::SPACE => Some(keyboard::SC_SPACE),
        _ => {
            let name = kc.name();
            if name.len() == 1 {
                let sdl_char = name.chars().nth(0)?;
                Keyboard::char_to_scancode(sdl_char)
            } else {
                None
            }
        }
    }
}

fn main() -> Result<()> {
    setup_panic_handler();
    let args = Args::parse();

    // Initialize logging
    tui_logger::init_logger(log::LevelFilter::Trace).unwrap();
    tui_logger::set_default_level(log::LevelFilter::Trace);

    // Initialize display
    let mut renderer = SDLRenderer::new(SCREEN_HEIGHT, SCREEN_WIDTH)?;
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
        MacModel {
            name: "Macintosh 128K",
            ram_size: 128 * 1024,
            fd_double: false,
        }
    } else if digest[..] == hex!("fe6a1ceff5b3eefe32f20efea967cdf8cd4cada291ede040600e7f6c9e2dfc0e")
    {
        // Macintosh 512K
        MacModel {
            name: "Macintosh 512K",
            ram_size: 512 * 1024,
            fd_double: false,
        }
    } else if
    // Macintosh Plus v1
    digest[..] == hex!("c5d862605867381af6200dd52f5004cc00304a36ab996531f15e0b1f8a80bc01") ||
        // Macintosh Plus v2
    digest[..] == hex!("06f598ff0f64c944e7c347ba55ae60c792824c09c74f4a55a32c0141bf91b8b3") ||
        // Macintosh Plus v3
    digest[..] == hex!("dd908e2b65772a6b1f0c859c24e9a0d3dcde17b1c6a24f4abd8955846d7895e7")
    {
        MacModel {
            name: "Macintosh Plus",
            ram_size: 4096 * 1024,
            fd_double: true,
        }
    } else {
        panic!("Cannot determine model from ROM file")
    };

    // Initialize emulator
    let (mut emulator, frame_recv) = Emulator::new(&rom, model)?;
    let cmd = emulator.create_cmd_sender();
    if let Some(floppy_fn) = args.floppy_filename {
        cmd.send(EmulatorCommand::InsertFloppy(
            std::fs::read(floppy_fn)?.into_boxed_slice(),
        ))?;
    }
    if args.run {
        cmd.send(EmulatorCommand::Run)?;
    }

    // Initialize user interface
    let mut terminal = UserInterface::init_terminal()?;
    let mut ui = UserInterface::new(
        &args.rom_filename,
        model.name,
        emulator.create_event_recv(),
        emulator.create_cmd_sender(),
    )?;

    // Spin up emulator thread
    let emuthread = thread::spawn(move || loop {
        match emulator.tick(1) {
            Ok(0) => break,
            Ok(_) => (),
            Err(e) => panic!("Emulator error: {}", e),
        }
    });

    'mainloop: loop {
        let t_start = Instant::now();

        // Render frame to SDL window
        if let Ok(frame) = frame_recv.try_recv() {
            renderer.update_from(&frame)?;
        }

        // Draw TUI
        if !ui.run(&mut terminal)? {
            break 'mainloop;
        }

        // Process SDL events
        while let Some(event) = eventpump.poll() {
            match event {
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                }
                | Event::Quit { .. } => {
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
                        snow_core::mac::keyboard::KeyEvent::KeyDown(mac_keycode),
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
                        snow_core::mac::keyboard::KeyEvent::KeyUp(mac_keycode),
                    ))?;
                }
                Event::MouseMotion {
                    x, y, xrel, yrel, ..
                } => match args.mouse {
                    MouseControl::Absolute => cmd.send(EmulatorCommand::MouseUpdateAbsolute {
                        x: x as u16,
                        y: y as u16,
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
                _ => (),
            }
        }

        // Sync to roughly 120 iterations per second
        thread::sleep(Duration::from_millis(10).saturating_sub(t_start.elapsed()));
    }

    // Terminate emulator
    cmd.send(EmulatorCommand::Quit)?;
    emuthread.join().unwrap();

    // Clean up terminal
    UserInterface::shutdown_terminal(&mut terminal)?;
    Ok(())
}
