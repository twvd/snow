mod keymap_sdl;
mod renderer_sdl;

use anyhow::Result;
use keymap_sdl::map_sdl_keycode;
use log::*;
use sdl2::event::{Event, WindowEvent};
use sdl2::mouse::MouseButton;
use snow_core::emulator::comm::EmulatorCommand;
use snow_core::emulator::Emulator;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::mac::MacModel;
use snow_core::tickable::Tickable;

use std::thread;

use renderer_sdl::{SDLAudioSink, SDLEventPump, SDLRenderer};
use snow_core::renderer::Renderer;

fn main() -> Result<()> {
    // Initialize logging
    env_logger::init();

    // Initialize display
    let scale = 1;
    let mut disp_win_width = SCREEN_WIDTH * scale;
    let mut disp_win_height = SCREEN_HEIGHT * scale;
    let mut renderer = SDLRenderer::new(SCREEN_WIDTH, SCREEN_HEIGHT)?;
    renderer.set_window_size(disp_win_width, disp_win_height)?;
    let eventpump = SDLEventPump::new();

    // Initialize ROM
    let rom = include_bytes!("../../plus3.rom");

    // Initialize emulator
    let (mut emulator, frame_recv) = Emulator::new(rom, MacModel::Plus)?;
    let cmd = emulator.create_cmd_sender();
    cmd.send(EmulatorCommand::Run)?;

    // Initialize audio
    let _audiodev = SDLAudioSink::new(emulator.get_audio())?;

    // Spin up emulator thread
    let emuthread = thread::spawn(move || loop {
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
                Event::MouseMotion { x, y, .. } => {
                    cmd.send(EmulatorCommand::MouseUpdateAbsolute {
                        x: (x as f32 / (disp_win_width as f32 / SCREEN_WIDTH as f32)) as u16,
                        y: (y as f32 / (disp_win_height as f32 / SCREEN_HEIGHT as f32)) as u16,
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

    Ok(())
}
