use std::cell::RefCell;

use anyhow::{anyhow, Result};
use log::*;
use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use sdl2::Sdl;
use snow_core::renderer::{AudioReceiver, AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS};

pub struct SDLSingleton {
    context: Sdl,
}

thread_local! {
    static SDL: RefCell<SDLSingleton> = RefCell::new({
        let context = sdl2::init().unwrap();

        SDLSingleton {
            context,
        }
    });
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
                channels: Some(AUDIO_CHANNELS.try_into().unwrap()),
                samples: Some(AUDIO_BUFFER_SAMPLES.try_into().unwrap()),
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

    pub fn set_receiver(&mut self, recv: AudioReceiver) {
        self.recv = recv;
    }
}
