use anyhow::Result;
use clap::Parser;
use hex_literal::hex;
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::mouse::MouseButton;
use sha2::{Digest, Sha256};
use snow::emulator::comm::EmulatorCommand;
use snow::emulator::{Emulator, MacModel};
use snow::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow::tickable::Tickable;

use std::{fs, thread};

use snow::frontend::sdl::{SDLEventPump, SDLRenderer};
use snow::frontend::Renderer;

#[derive(Parser)]
#[command(
    about = "Snow - Classic Macintosh emulator",
    author = "Thomas <thomas@thomasw.dev>",
    long_about = None)]
struct Args {
    /// ROM filename to load.
    rom_filename: String,

    /// Trace bus I/O activity
    #[arg(long)]
    trace: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize display
    let mut renderer = SDLRenderer::new(SCREEN_HEIGHT, SCREEN_WIDTH)?;
    let eventpump = SDLEventPump::new();

    // Initialize ROM
    let rom = fs::read(args.rom_filename)?;

    // Detect model
    // TODO Make this nicer
    let mut hash = Sha256::new();
    hash.update(&rom);
    let digest = hash.finalize();
    let model =
        if digest[..] == hex!("fe6a1ceff5b3eefe32f20efea967cdf8cd4cada291ede040600e7f6c9e2dfc0e") {
            // Macintosh 512K
            MacModel {
                name: "Macintosh 512K",
                ram_size: 512 * 1024,
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
            }
        } else {
            panic!("Cannot determine model from ROM file")
        };

    let (mut emulator, frame_recv) = Emulator::new(&rom, model)?;
    let cmd = emulator.create_cmd_sender();

    // Spin up emulator thread
    let emuthread = thread::spawn(move || loop {
        match emulator.tick(1) {
            Ok(0) => break,
            Ok(_) => (),
            Err(e) => panic!("Emulator error: {}", e),
        }
    });

    'mainloop: loop {
        let frame = frame_recv.recv()?;
        renderer.update_from(frame)?;

        while let Some(event) = eventpump.poll() {
            match event {
                Event::KeyDown {
                    keycode: Some(Keycode::I),
                    ..
                } => {
                    cmd.send(EmulatorCommand::InsertFloppy(Box::new([])))?;
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                }
                | Event::Quit { .. } => {
                    break 'mainloop;
                }
                Event::MouseMotion { xrel, yrel, .. } => {
                    cmd.send(EmulatorCommand::MouseUpdateRelative {
                        relx: xrel.try_into()?,
                        rely: yrel.try_into()?,
                        btn: None,
                    })?;
                }
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
    }

    cmd.send(EmulatorCommand::Quit)?;
    emuthread.join().unwrap();
    Ok(())
}
