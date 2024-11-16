mod keymap_sdl;
mod renderer_sdl;

use anyhow::Result;
use crossbeam_channel::Receiver;
use log::*;
use sdl2::event::{Event, WindowEvent};
use sdl2::mouse::MouseButton;

use snow_core::emulator::comm::{EmulatorCommand, EmulatorCommandSender};
use snow_core::emulator::Emulator;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::mac::MacModel;
use snow_core::renderer::DisplayBuffer;
use snow_core::renderer::Renderer;
use snow_core::tickable::Tickable;

use std::thread;

use keymap_sdl::map_sdl_keycode;
use renderer_sdl::{SDLAudioSink, SDLEventPump, SDLRenderer};

struct EmulatorMain {
    emulator: Emulator,
    frame_recv: Receiver<DisplayBuffer>,
    renderer: SDLRenderer,
    cmd: EmulatorCommandSender,
    eventpump: SDLEventPump,
    disp_win_width: usize,
    disp_win_height: usize,
}

impl EmulatorMain {
    fn tick(&mut self) -> Result<bool> {
        // Run emulator until there's a frame available
        while self.frame_recv.is_empty() {
            self.emulator.tick(1)?;
        }

        // Render frame to SDL canvas
        self.renderer.update_from(&self.frame_recv.try_recv()?)?;

        // Process SDL events
        while let Some(event) = self.eventpump.wait(10) {
            match event {
                Event::Quit { .. } => {
                    return Ok(false);
                }
                Event::KeyDown {
                    keycode: Some(k), ..
                } => {
                    let Some(mac_keycode) = map_sdl_keycode(k) else {
                        warn!("Unknown SDL keycode: {:?} ({})", k, k.name());
                        continue;
                    };

                    self.cmd.send(EmulatorCommand::KeyEvent(
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

                    self.cmd.send(EmulatorCommand::KeyEvent(
                        snow_core::keymap::KeyEvent::KeyUp(mac_keycode),
                    ))?;
                }
                Event::MouseMotion { x, y, .. } => {
                    self.cmd.send(EmulatorCommand::MouseUpdateAbsolute {
                        x: (x as f32 / (self.disp_win_width as f32 / SCREEN_WIDTH as f32)) as u16,
                        y: (y as f32 / (self.disp_win_height as f32 / SCREEN_HEIGHT as f32)) as u16,
                    })?;
                }
                Event::MouseButtonDown {
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    self.cmd.send(EmulatorCommand::MouseUpdateRelative {
                        relx: 0,
                        rely: 0,
                        btn: Some(true),
                    })?;
                }
                Event::MouseButtonUp {
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    self.cmd.send(EmulatorCommand::MouseUpdateRelative {
                        relx: 0,
                        rely: 0,
                        btn: Some(false),
                    })?;
                }
                Event::Window {
                    win_event: WindowEvent::Resized(w, h),
                    ..
                } => {
                    self.disp_win_width = w as usize;
                    self.disp_win_height = h as usize;
                }
                _ => (),
            }
        }

        Ok(true)
    }
}

impl emscripten_main_loop::MainLoop for EmulatorMain {
    fn main_loop(&mut self) -> emscripten_main_loop::MainLoopEvent {
        if let Ok(true) = self.tick() {
            emscripten_main_loop::MainLoopEvent::Continue
        } else {
            emscripten_main_loop::MainLoopEvent::Terminate
        }
    }
}

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

    // Set up emscripten main loop
    let emuloop = EmulatorMain {
        emulator,
        frame_recv,
        renderer,
        eventpump,
        cmd,
        disp_win_width,
        disp_win_height,
    };

    //'mainloop: loop {
    //    emuloop.tick();
    //}

    emscripten_main_loop::run(emuloop);

    Ok(())
}
