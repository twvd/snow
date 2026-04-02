use anyhow::{anyhow, Result};
use log::*;
use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use sdl2::Sdl;
use snow_core::renderer::{AudioBuffer, AudioProvider, AudioReceiver, AudioSink, ChannelAudioSink};
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
    /// Another layer of buffering to solve these problems:
    /// - SDL audio buffers must have a power-of-2 sample count
    /// - The user may submit audio buffers that exceed SDL's buffer size
    active_samples: Vec<f32>,
}

/// Exchanges state between the SDLAudioCallback and SDLAudioProvider
#[derive(Default)]
struct SDLAudioExchange {
    mute: AtomicBool,
    slow: AtomicBool,
}

/// Holds the SDL AudioDevice solely to prevent it from being dropped.
/// Do not access the device field, it may not be thread-safe!
#[allow(unused)]
#[allow(clippy::non_send_fields_in_send_ty)]
struct AudioDeviceHolder(AudioDevice<SDLAudioCallback>);

// SAFETY: It should be safe to declare AudioDeviceHolder Send and Sync as long
// as it is only being held, never accessed.
// FIXME: I'm not actually sure if this is safe at all. Seems to work though.
unsafe impl Send for AudioDeviceHolder {}
unsafe impl Sync for AudioDeviceHolder {}

pub struct SDLAudioStream {
    #[allow(unused)]
    device: AudioDeviceHolder,
    channel_sink: ChannelAudioSink,
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

        // Collect audio samples into the active samples buffer
        while self.active_samples.len() < out.len() {
            if let Ok(new_samples) = self.recv.try_recv() {
                self.active_samples.extend_from_slice(&new_samples);
            } else {
                break;
            }
        }

        if self.active_samples.len() >= out.len() {
            if slow || self.exch.mute.load(Ordering::Relaxed) {
                out.fill(0.0);
            } else {
                // Feed active samples to the SDL output
                for (i, out_sample) in out.iter_mut().enumerate() {
                    *out_sample = self.active_samples[i].clamp(-1.0, 1.0) * 0.5;
                }
                // TODO: use a circular queue or something
                self.active_samples.copy_within(out.len().., 0);
                self.active_samples
                    .truncate(self.active_samples.len() - out.len());
            }
        } else {
            // Audio is late. Play the last active samples and disable audio for a certain period.
            for (i, out_sample) in out.iter_mut().enumerate().take(self.active_samples.len()) {
                *out_sample = self.active_samples[i].clamp(-1.0, 1.0) * 0.5;
            }
            out[self.active_samples.len()..].fill(0.0);
            self.active_samples.clear();
            self.stop_delay = Instant::now() + Duration::from_millis(250);
        }
    }
}

impl SDLAudioStream {
    /// Creates a new audio provider
    pub fn new(freq: i32, channels: u8, samples: u16) -> Result<Self> {
        SDL.with(|cell| {
            let channel_sink = ChannelAudioSink::new();

            let sdls = cell.borrow_mut();
            let audio_subsystem = sdls.context.audio().map_err(|e| anyhow!(e))?;
            let spec = AudioSpecDesired {
                freq: Some(freq),
                channels: Some(channels),
                samples: Some(samples),
            };

            let exch = Arc::new(SDLAudioExchange::default());

            let device = audio_subsystem
                .open_playback(None, &spec, |spec| {
                    debug!("Audio spec: {:?}", spec);
                    SDLAudioCallback {
                        recv: channel_sink.receiver(),
                        stop_delay: Instant::now(),
                        exch: exch.clone(),
                        active_samples: vec![],
                    }
                })
                .map_err(|e| anyhow!(e))?;
            device.resume();
            Ok(Self {
                device: AudioDeviceHolder(device),
                channel_sink,
                exch,
            })
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

struct SDLAudioStreamSink {
    stream: Arc<SDLAudioStream>,
}

impl AudioSink for SDLAudioStreamSink {
    fn send(&self, buffer: AudioBuffer) -> Result<()> {
        self.stream.channel_sink.send(buffer)
    }

    fn is_full(&self) -> bool {
        self.stream.channel_sink.is_full()
    }
}

pub struct SDLAudioProvider {
    streams: Vec<Arc<SDLAudioStream>>,
}

impl SDLAudioProvider {
    pub fn new() -> Result<Self> {
        Ok(Self { streams: vec![] })
    }

    pub fn is_slow(&self) -> bool {
        self.streams.iter().any(|stream| stream.is_slow())
    }

    pub fn is_muted(&self) -> bool {
        self.streams.iter().all(|stream| stream.is_muted())
    }

    pub fn set_mute(&self, mute: bool) {
        for stream in &self.streams {
            stream.set_mute(mute);
        }
    }
}

impl AudioProvider for SDLAudioProvider {
    fn create_stream(
        &mut self,
        freq: i32,
        channels: u8,
        samples: u16,
    ) -> Result<Box<dyn AudioSink>> {
        let stream = Arc::new(SDLAudioStream::new(freq, channels, samples)?);
        self.streams.push(stream.clone());
        let stream_sink = SDLAudioStreamSink { stream };
        Ok(Box::new(stream_sink))
    }
}
