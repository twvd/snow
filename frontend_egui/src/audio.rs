use anyhow::{anyhow, Result};
use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use sdl2::Sdl;
use snow_core::renderer::{AudioBuffer, AudioProvider, AudioReceiver, AudioSink, ChannelAudioSink};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    exch: Arc<SDLAudioExchange>,
    /// When true, play silence while waiting for the audio receiver to fill up.
    /// Prebuffering prevents audio interruptions if the emulator doesn't deliver
    /// samples in time for playback.
    prebuffering: bool,
    /// An extra layer of buffering to solve these problems:
    /// - SDL audio buffers must have a power-of-2 length
    /// - Users may submit audio buffers that exceed SDL's expected length
    active_samples: VecDeque<f32>,
}

/// Exchanges state between the SDLAudioCallback and SDLAudioProvider
#[derive(Default)]
struct SDLAudioExchange {
    mute: AtomicBool,
    underrun: AtomicBool,
}

/// Holds the SDL AudioDevice solely to prevent it from being dropped.
/// Do not access the device field, it may not be thread-safe!
#[allow(unused)]
#[allow(clippy::non_send_fields_in_send_ty)]
struct AudioDeviceHolder(AudioDevice<SDLAudioCallback>);

// SAFETY: It should be safe to declare AudioDeviceHolder Send and Sync as long
// as it is only being held, never accessed.
// FIXME: I'm not actually sure if this is safe at all. It seems to work though.
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

        if self.prebuffering {
            if self.recv.is_full() {
                self.prebuffering = false;
            } else {
                out.fill(0.0);
                return;
            }
        }

        // Collect audio samples into the active buffer
        while self.active_samples.len() < out.len() {
            if let Ok(new_samples) = self.recv.try_recv() {
                self.active_samples.extend(&new_samples);
            } else {
                break;
            }
        }

        if self.active_samples.len() >= out.len() {
            if self.exch.mute.load(Ordering::Relaxed) {
                out.fill(0.0);
            } else {
                // Feed active samples to the SDL output
                for out_sample in out.iter_mut() {
                    *out_sample = self.active_samples.pop_front().unwrap().clamp(-1.0, 1.0) * 0.5;
                }
            }

            self.exch.underrun.store(false, Ordering::Relaxed);
        } else {
            // log::warn!("Audio buffer underrun. Audio may skip.");

            // Audio is late. Play the last active samples and start prebuffering.
            let sample_count = self.active_samples.len();
            for out_sample in out.iter_mut().take(sample_count) {
                *out_sample = self.active_samples.pop_front().unwrap().clamp(-1.0, 1.0) * 0.5;
            }
            out[sample_count..].fill(0.0);
            self.active_samples.clear();

            self.prebuffering = true;
            self.exch.underrun.store(true, Ordering::Relaxed);
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
                    log::debug!("Audio spec: {:?}", spec);
                    SDLAudioCallback {
                        recv: channel_sink.receiver(),
                        exch: exch.clone(),
                        prebuffering: true,
                        active_samples: Default::default(),
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

    pub fn is_underrun(&self) -> bool {
        self.exch.underrun.load(Ordering::Relaxed)
    }

    pub fn is_muted(&self) -> bool {
        self.exch.mute.load(Ordering::Relaxed)
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

    fn is_empty(&self) -> bool {
        self.stream.channel_sink.is_empty()
    }
}

pub struct SDLAudioProvider {
    streams: Vec<Arc<SDLAudioStream>>,
}

impl SDLAudioProvider {
    pub fn new() -> Result<Self> {
        Ok(Self { streams: vec![] })
    }

    pub fn is_underrun(&self) -> bool {
        // Only report underrun for the first stream
        self.streams
            .first()
            .map(|stream| stream.is_underrun())
            .unwrap_or(false)
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
