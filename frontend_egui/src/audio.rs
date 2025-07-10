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
    type Channel = i16;

    fn callback(&mut self, out: &mut [i16]) {
        // The formula below SHOULD be ((s as i16) - 128) * 256, but for some reason
        // if the audio clips on MacOS hosts with CERTAIN audio outputs, the sound
        // in the OS will be distorted for as long as the emulator is running.
        // Reducing the maximum sample amplitude seems to not trigger this, so that
        // is the workaround for now.
        //
        // Last tested on MacOS Sequoia, SDL 2.32.8.

        if let Ok(buffer) = self.recv.try_recv() {
            self.last_sample = buffer.last().copied().unwrap();
            for i in 0..out.len() {
                out[i] = ((buffer[i] as i16) - 128) * 128;
            }
        } else {
            // Audio is late. Continue the last output sample to reduce
            // pops and other abrupt noises.
            out.fill(((self.last_sample as i16) - 128) * 128);
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
                // Round down to a safe commonly used value (22050), 0.9% off
                freq: Some(22050),
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
