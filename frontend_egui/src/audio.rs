use anyhow::{anyhow, Result};
use log::*;
use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use sdl2::Sdl;
use snow_core::renderer::{AudioReceiver, AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

struct SDLAudioCallback {
    recv: AudioReceiver,
    stop_delay: Instant,
    exch: Arc<SDLAudioExchange>,
}

/// Exchanges state between the SDLAudioCallback and SDLAudioProvider
#[derive(Default)]
struct SDLAudioExchange {
    mute: AtomicBool,
    slow: AtomicBool,
}

pub struct SDLAudioProvider {
    device: AudioDevice<SDLAudioCallback>,
    exch: Arc<SDLAudioExchange>,
}

impl AudioCallback for SDLAudioCallback {
    type Channel = f32;

    fn callback(&mut self, out: &mut [f32]) {
        // Audio samples are already in standard f32 range (-1.0 to 1.0)
        // The amplitude is reduced from theoretical 1.0 to 0.5 because
        // if the audio reaches max volume on MacOS hosts with CERTAIN audio outputs, the sound
        // in the OS will be distorted for as long as the emulator is running.
        // Reducing the maximum sample amplitude seems to not trigger this, so that
        // is the workaround for now.
        //
        // Last tested on MacOS Sequoia, SDL 2.32.8.

        let slow = self.stop_delay > Instant::now();
        self.exch.slow.store(slow, Ordering::Relaxed);

        if let Ok(buffer) = self.recv.try_recv() {
            if slow || self.exch.mute.load(Ordering::Relaxed) {
                out.fill(0.0);
            } else {
                for i in 0..out.len() {
                    out[i] = buffer[i].clamp(-1.0, 1.0) * 0.5;
                }
            }
        } else {
            // Audio is late. Disable audio for a certain period
            out.fill(0.0);
            self.stop_delay = Instant::now() + Duration::from_millis(500);
        }
    }
}

impl SDLAudioProvider {
    /// Creates a new audio provider
    pub fn new(audioch: AudioReceiver) -> Result<Self> {
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

            let exch = Arc::new(SDLAudioExchange::default());

            let device = audio_subsystem
                .open_playback(None, &spec, |spec| {
                    debug!("Audio spec: {:?}", spec);
                    SDLAudioCallback {
                        recv: audioch,
                        stop_delay: Instant::now(),
                        exch: exch.clone(),
                    }
                })
                .map_err(|e| anyhow!(e))?;
            device.resume();
            Ok(Self { device, exch })
        })
    }

    pub fn is_slow(&self) -> bool {
        self.exch.slow.load(Ordering::Relaxed)
    }

    pub fn is_muted(&self) -> bool {
        self.exch.mute.load(Ordering::Relaxed) || self.is_slow()
    }

    pub fn set_mute(&self, mute: bool) {
        self.exch.mute.store(mute, Ordering::Release);
    }
}
