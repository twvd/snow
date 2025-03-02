#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

mod app;
mod audio;
mod dialogs;
mod emulator;
mod keymap;
mod widgets;
mod workspace;

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
    /// ROM filename to load on start
    rom_filename: Option<String>,

    /// Disable audio
    #[arg(long, action)]
    no_audio: bool,
}

fn main() -> eframe::Result {
    let args = Args::parse();

    let logger_env = Box::new(
        env_logger::builder()
            .filter_level(LevelFilter::Debug)
            .build(),
    );
    let logger_egui = Box::new(egui_logger::builder().max_level(LevelFilter::Debug).build());
    multi_log::MultiLogger::init(vec![logger_env, logger_egui], log::Level::Debug).unwrap();

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
        Box::new(|cc| {
            Ok(Box::new(SnowGui::new(
                cc,
                r,
                args.rom_filename,
                !args.no_audio,
            )))
        }),
    )
}
