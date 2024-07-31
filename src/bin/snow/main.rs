use anyhow::Result;
use clap::Parser;
use hex_literal::hex;
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::mouse::MouseButton;
use sha2::{Digest, Sha256};

use std::fs;
use std::sync::atomic::Ordering;

use snow::cpu_m68k::cpu::CpuM68k;
use snow::frontend::sdl::{SDLEventPump, SDLRenderer};
use snow::frontend::Renderer;
use snow::mac::bus::MacBus;

const SCREEN_HEIGHT: usize = 512;
const SCREEN_WIDTH: usize = 342;

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

struct MacModel {
    ram_size: usize,
    framebuffer: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();

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
    let model =
        if digest[..] == hex!("fe6a1ceff5b3eefe32f20efea967cdf8cd4cada291ede040600e7f6c9e2dfc0e") {
            // Macintosh 512K
            MacModel {
                ram_size: 512 * 1024,
                framebuffer: 0x7A700,
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
                ram_size: 4096 * 1024,
                framebuffer: 0x3FA700,
            }
        } else {
            panic!("Cannot determine model from ROM file")
        };

    // Initialize bus and CPU
    let mut bus = MacBus::new(&rom, model.ram_size);
    bus.trace = args.trace;

    let mut cpu = CpuM68k::new(bus);
    cpu.reset()?;

    'mainloop: for i in 0.. {
        cpu.step()?;

        // TODO do less frequent/move emulator to its own thread
        while let Some(event) = eventpump.poll() {
            match event {
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                }
                | Event::Quit { .. } => break 'mainloop,
                Event::MouseMotion { xrel, yrel, .. } => {
                    cpu.bus
                        .mouse_update(xrel.try_into()?, yrel.try_into()?, None)
                }
                Event::MouseButtonDown {
                    mouse_btn: MouseButton::Left,
                    ..
                } => cpu.bus.mouse_update(0, 0, Some(true)),
                Event::MouseButtonUp {
                    mouse_btn: MouseButton::Left,
                    ..
                } => cpu.bus.mouse_update(0, 0, Some(false)),
                _ => (),
            }
        }

        if i % 10000 == 0 {
            let buf = renderer.get_buffer();
            for idx in 0..(SCREEN_WIDTH * SCREEN_HEIGHT) {
                let byte = idx / 8;
                let bit = idx % 8;
                if cpu.bus.ram[model.framebuffer + byte] & (1 << (7 - bit)) == 0 {
                    buf[idx * 4 + 0].store(0xC7, Ordering::Release);
                    buf[idx * 4 + 1].store(0xF1, Ordering::Release);
                    buf[idx * 4 + 2].store(0xFB, Ordering::Release);
                } else {
                    buf[idx * 4 + 0].store(0x22, Ordering::Release);
                    buf[idx * 4 + 1].store(0x22, Ordering::Release);
                    buf[idx * 4 + 2].store(0x22, Ordering::Release);
                }
            }
            renderer.update()?;
        }
    }

    Ok(())
}
