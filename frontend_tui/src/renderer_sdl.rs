use std::cell::RefCell;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use log::*;
use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use sdl2::event::Event;
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Canvas, Texture};
use sdl2::video::Window;
use sdl2::{EventPump, Sdl};

use snow_core::mac::audio::{AudioReceiver, AUDIO_BUFFER_SIZE};
use snow_core::renderer::{new_displaybuffer, DisplayBuffer, Renderer};

pub struct SDLSingleton {
    context: Sdl,
    pump: EventPump,
}

thread_local! {
    static SDL: RefCell<SDLSingleton> = RefCell::new({
        let context = sdl2::init().unwrap();
        let pump = context.event_pump().unwrap();

        SDLSingleton {
            context,
            pump
        }
    });
}

pub struct SDLRenderer {
    canvas: Canvas<Window>,
    texture: Texture,
    displaybuffer: DisplayBuffer,
    width: usize,
    #[allow(dead_code)]
    height: usize,

    fps_count: u64,
    fps_time: Instant,
}

impl SDLRenderer {
    const BPP: usize = 4;

    pub fn update_from(&mut self, buffer: &DisplayBuffer) -> Result<()> {
        // This is safe because SDL will only read from the transmuted
        // buffer. Worst case is a garbled display.
        let sdl_displaybuffer = unsafe { std::mem::transmute::<&[AtomicU8], &[u8]>(buffer) };
        self.texture
            .update(None, sdl_displaybuffer, self.width * Self::BPP)?;
        self.canvas.clear();
        self.canvas
            .copy(&self.texture, None, None)
            .map_err(|e| anyhow!(e))?;
        self.canvas.present();

        self.fps_count += 1;

        if self.fps_time.elapsed().as_secs() >= 2 {
            self.fps_count = 0;
            self.fps_time = Instant::now();
        }

        Ok(())
    }

    pub fn set_window_size(&mut self, width: usize, height: usize) -> Result<()> {
        self.canvas
            .window_mut()
            .set_size(width as u32, height as u32)?;
        self.canvas.window_mut().set_position(
            sdl2::video::WindowPos::Centered,
            sdl2::video::WindowPos::Centered,
        );
        Ok(())
    }
}

impl Renderer for SDLRenderer {
    /// Creates a new renderer with a screen of the given size
    fn new(width: usize, height: usize) -> Result<Self> {
        SDL.with(|cell| {
            let sdls = cell.borrow_mut();

            let video_subsystem = sdls.context.video().map_err(|e| anyhow!(e))?;
            let window = video_subsystem
                .window("Snow", width.try_into()?, height.try_into()?)
                .position_centered()
                .build()?;

            let canvas = window.into_canvas().accelerated().build()?;
            info!("Rendering driver: {:?}", canvas.info().name);

            sdls.context.mouse().show_cursor(false);

            let texture_creator = canvas.texture_creator();
            let texture = texture_creator.create_texture_streaming(
                PixelFormatEnum::RGB888,
                width.try_into()?,
                height.try_into()?,
            )?;

            Ok(Self {
                canvas,
                texture,
                displaybuffer: new_displaybuffer(width, height),
                width,
                height,
                fps_count: 0,
                fps_time: Instant::now(),
            })
        })
    }

    fn get_buffer(&mut self) -> DisplayBuffer {
        Arc::clone(&self.displaybuffer)
    }

    /// Renders changes to screen
    fn update(&mut self) -> Result<()> {
        self.update_from(&Arc::clone(&self.displaybuffer))
    }
}

pub struct SDLEventPump {}
impl SDLEventPump {
    pub fn new() -> Self {
        Self {}
    }

    #[allow(dead_code)]
    pub fn poll(&self) -> Option<Event> {
        SDL.with(|cell| {
            let mut sdls = cell.borrow_mut();
            sdls.pump.poll_event()
        })
    }

    #[allow(dead_code)]
    pub fn wait(&self, ms: u32) -> Option<Event> {
        SDL.with(|cell| {
            let mut sdls = cell.borrow_mut();
            sdls.pump.wait_event_timeout(ms)
        })
    }
}

pub struct SDLAudioSink {
    recv: AudioReceiver,
    last_sample: u8,
}

impl AudioCallback for SDLAudioSink {
    type Channel = u8;

    fn callback(&mut self, out: &mut [u8]) {
        if let Ok(buffer) = self.recv.try_recv() {
            self.last_sample = buffer.last().copied().unwrap();
            out.copy_from_slice(&buffer);
        } else {
            // Audio is late. Continue the last output sample to reduce
            // pops and other abrupt noises.
            out.fill(self.last_sample);
        }
    }
}

impl SDLAudioSink {
    /// Creates a new audiosink
    pub fn new(audioch: AudioReceiver) -> Result<AudioDevice<Self>> {
        SDL.with(|cell| {
            let sdls = cell.borrow_mut();
            let audio_subsystem = sdls.context.audio().map_err(|e| anyhow!(e))?;
            let spec = AudioSpecDesired {
                // Audio sample frequency is tied to monitor's horizontal sync
                // 370 horizontal lines * 60.147 frames/sec = 22.254 KHz
                freq: Some(22254),
                channels: Some(1),
                samples: Some(AUDIO_BUFFER_SIZE.try_into().unwrap()),
            };

            let device = audio_subsystem
                .open_playback(None, &spec, |spec| {
                    debug!("Audio spec: {:?}", spec);
                    Self {
                        recv: audioch,
                        last_sample: 0,
                    }
                })
                .map_err(|e| anyhow!(e))?;
            device.resume();
            Ok(device)
        })
    }
}
