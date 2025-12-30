#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

mod app;
mod audio;
mod consts;
mod dialogs;
mod emulator;
mod helpers;
mod keymap;
mod settings;
mod shader_pipeline;
mod uniform;
mod util;
mod widgets;
mod workspace;

use crate::app::SnowGui;

use clap::Parser;
use eframe::egui;
use log::LevelFilter;

const SNOW_ICON: &[u8] = include_bytes!("../../docs/images/snow_icon.png");

#[derive(Parser)]
#[command(
    about = "Snow - Classic Macintosh emulator",
    author = "Thomas <thomas@thomasw.dev>",
    long_about = None)]
struct Args {
    /// Filename to load on start (ROM or workspace)
    filename: Option<String>,

    /// UI scale
    #[arg(long, default_value_t = 1.0)]
    ui_scale: f32,

    /// Start in fullscreen (specify ROM or workspace)
    #[arg(long, short)]
    fullscreen: bool,

    /// Start in Zen mode (specify ROM or workspace)
    #[arg(long)]
    zen: bool,

    /// Enable serial bridge on SCC channel A (modem port).
    /// Values: "pty" for PTY mode (Unix only), "tcp:PORT" for TCP mode
    #[arg(long, value_name = "MODE")]
    serial_bridge_a: Option<String>,

    /// Enable serial bridge on SCC channel B (printer port).
    /// Values: "pty" for PTY mode (Unix only), "tcp:PORT" for TCP mode
    #[arg(long, value_name = "MODE")]
    serial_bridge_b: Option<String>,
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

    log::info!(
        "Snow v{} ({} {})",
        snow_core::build_version(),
        snow_core::built_info::TARGET,
        snow_core::built_info::PROFILE,
    );

    // The egui frontend uses a patched version of egui-winit that allows hooking
    // into the winit WindowEvent stream in order to capture raw keyboard events.
    // egui/eframe does not expose all the keys we need, currently.
    // See also https://github.com/emilk/egui/issues/3653
    let (s, r) = crossbeam_channel::unbounded();
    egui_winit::install_windowevent_hook(s);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(eframe::icon_data::from_png_bytes(SNOW_ICON).expect("Icon is not valid PNG"))
            .with_inner_size([1000.0, 750.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Snow",
        options,
        Box::new(|cc| {
            // Force dark theme as UI elements and colors are not light-friendly (yet)
            cc.egui_ctx.set_theme(egui::Theme::Dark);

            Ok(Box::new(SnowGui::new(
                cc,
                r,
                args.filename,
                args.ui_scale,
                args.fullscreen,
                args.zen,
                args.serial_bridge_a.as_deref(),
                args.serial_bridge_b.as_deref(),
            )))
        }),
    )
}
