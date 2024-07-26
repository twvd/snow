use anyhow::Result;
use clap::Parser;

use std::fs;
use std::sync::atomic::Ordering;

use snow::cpu_m68k::cpu::CpuM68k;
use snow::frontend::sdl::SDLRenderer;
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
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize display
    let mut renderer = SDLRenderer::new(SCREEN_HEIGHT, SCREEN_WIDTH)?;

    // Initialize ROM
    let rom = fs::read(&args.rom_filename)?;

    // Initialize bus and CPU
    let bus = MacBus::new(&rom);
    let mut cpu = CpuM68k::new(bus);
    cpu.reset()?;

    for i in 0.. {
        //println!("PC: {:08X}", cpu.regs.pc);
        cpu.step()?;

        if cpu.bus.via.irq_flag.0 != 0 {
            cpu.trigger_irq_autovector(1)?;
        }

        if i % 10000 == 0 {
            let buf = renderer.get_buffer();
            for idx in 0..(SCREEN_WIDTH * SCREEN_HEIGHT) {
                let byte = idx / 8;
                let bit = idx % 8;
                if cpu.bus.ram[0x7A700 + byte] & (1 << bit) == 0 {
                    for i in 0..4 {
                        buf[idx * 4 + i].store(0, Ordering::Release);
                    }
                } else {
                    for i in 0..4 {
                        buf[idx * 4 + i].store(0xFF, Ordering::Release);
                    }
                }
            }
            renderer.update()?;
        }
    }

    Ok(())
}
