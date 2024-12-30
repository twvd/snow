#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

pub mod app;
pub mod emulator;
pub mod widgets;

use crate::app::SnowGui;
use clap::Parser;
use eframe::egui;
use log::LevelFilter;

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
}

fn main() -> eframe::Result {
    env_logger::builder()
        .filter_level(LevelFilter::Debug)
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default(),
        ..Default::default()
    };
    eframe::run_native(
        "Snow",
        options,
        Box::new(|cc| Ok(Box::new(SnowGui::new(cc)))),
    )
}
