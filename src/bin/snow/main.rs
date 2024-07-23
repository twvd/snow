use anyhow::Result;
use clap::Parser;

use std::fs;

use snow::cpu_m68k::cpu::CpuM68k;
use snow::mac::bus::MacBus;

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

    // Initialize ROM
    let rom = fs::read(&args.rom_filename)?;

    // Initialize bus and CPU
    let bus = MacBus::new(&rom);
    let mut cpu = CpuM68k::new(bus);
    cpu.reset()?;

    loop {
        //println!("PC: {:08X}", cpu.regs.pc);
        cpu.step()?;
    }

    Ok(())
}
