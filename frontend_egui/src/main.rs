#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

pub mod app;
pub mod emulator;
pub mod keymap;
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

    // The egui frontend uses a patched version of egui-winit that allows hooking
    // into the winit WindowEvent stream in order to capture raw keyboard events.
    // egui/eframe does not expose all the keys we need, currently.
    // See also https://github.com/emilk/egui/issues/3653
    let (s, r) = crossbeam_channel::unbounded();
    egui_winit::install_windowevent_hook(s);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default(),
        ..Default::default()
    };
    eframe::run_native(
        "Snow",
        options,
        Box::new(|cc| Ok(Box::new(SnowGui::new(cc, r)))),
    )
}
