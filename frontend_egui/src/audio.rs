use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, Stream, StreamConfig};
use snow_core::renderer::{AudioBuffer, AudioProvider, AudioReceiver, AudioSink, ChannelAudioSink};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct CpalAudioCallback {
    recv: AudioReceiver,
    exch: Arc<CpalAudioExchange>,
    /// When true, play silence while waiting for the audio receiver to fill up.
    /// Prebuffering prevents audio interruptions if the emulator doesn't deliver
    /// samples in time for playback.
    prebuffering: bool,
    /// An extra layer of buffering to solve the problem that users may submit audio
    /// buffers that don't line up with the host's expected callback length.
    active_samples: VecDeque<f32>,
}

impl CpalAudioCallback {
    fn fill(&mut self, out: &mut [f32]) {
        // Audio samples are already in standard f32 range (-1.0 to 1.0)
        // The amplitude is reduced from theoretical 1.0 to 0.5 because
        // if the audio reaches max volume on MacOS hosts with CERTAIN audio outputs, the sound
        // in the OS will be distorted for as long as the emulator is running.
        // Reducing the maximum sample amplitude seems to not trigger this, so that
        // is the workaround for now.

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
                self.active_samples.extend(new_samples.iter().copied());
            } else {
                break;
            }
        }

        let submit_samples = |out: &mut [f32], active_samples: &mut VecDeque<f32>| {
            if self.exch.mute.load(Ordering::Relaxed) {
                // Discard active samples and play silence
                for out_sample in out.iter_mut() {
                    *out_sample = 0.0;
                    active_samples.pop_front();
                }
            } else {
                // Feed active samples to the output
                for out_sample in out.iter_mut() {
                    *out_sample = active_samples.pop_front().unwrap().clamp(-1.0, 1.0) * 0.5;
                }
            }
        };

        if self.active_samples.len() >= out.len() {
            submit_samples(out, &mut self.active_samples);
            self.exch.underrun.store(false, Ordering::Relaxed);
        } else {
            // log::warn!("Audio buffer underrun. Audio may skip.");

            // Audio is late. Submit any remaining active samples and enter prebuffering mode.
            let sample_count = self.active_samples.len();
            submit_samples(&mut out[..sample_count], &mut self.active_samples);
            out[sample_count..].fill(0.0);
            self.active_samples.clear();

            self.prebuffering = true;
            self.exch.underrun.store(true, Ordering::Relaxed);
        }
    }
}

/// Exchanges state between the audio callback and the provider
#[derive(Default)]
struct CpalAudioExchange {
    mute: AtomicBool,
    underrun: AtomicBool,
}

/// Holds the cpal Stream solely to prevent it from being dropped.
/// Do not access the stream field, it may not be thread-safe!
#[allow(unused)]
struct StreamHolder(Stream);

pub struct CpalAudioStream {
    #[allow(unused)]
    stream: StreamHolder,
    channel_sink: ChannelAudioSink,
    exch: Arc<CpalAudioExchange>,
}

impl CpalAudioStream {
    pub fn new(freq: i32, channels: u8, samples: u16) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("No default audio output device"))?;

        let config = StreamConfig {
            channels: channels as u16,
            sample_rate: freq as u32,
            buffer_size: BufferSize::Fixed(samples as u32),
        };

        log::debug!(
            "Audio: device={:?} freq={} channels={} buffer={}",
            device.description().ok(),
            freq,
            channels,
            samples
        );

        let channel_sink = ChannelAudioSink::new();
        let exch = Arc::new(CpalAudioExchange::default());

        let mut cb = CpalAudioCallback {
            recv: channel_sink.receiver(),
            exch: exch.clone(),
            prebuffering: true,
            active_samples: VecDeque::new(),
        };

        let err_fn = |err| log::error!("Audio stream error: {}", err);

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| cb.fill(data),
                err_fn,
                None,
            )
            .map_err(|e| anyhow!("Failed to build audio output stream: {}", e))?;

        // Sanity check the supported format. cpal will try to honor the config
        // regardless; we only log a warning if it looks off.
        if let Ok(supported) = device.default_output_config() {
            if supported.sample_format() != SampleFormat::F32 {
                log::debug!(
                    "Default device sample format is {:?}; requesting F32 anyway.",
                    supported.sample_format()
                );
            }
        }

        stream
            .play()
            .map_err(|e| anyhow!("Failed to start audio stream: {}", e))?;

        Ok(Self {
            stream: StreamHolder(stream),
            channel_sink,
            exch,
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

struct CpalAudioStreamSink {
    stream: Arc<CpalAudioStream>,
}

impl AudioSink for CpalAudioStreamSink {
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

pub struct CpalAudioProvider {
    streams: Vec<Arc<CpalAudioStream>>,
}

impl CpalAudioProvider {
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

impl AudioProvider for CpalAudioProvider {
    fn create_stream(
        &mut self,
        freq: i32,
        channels: u8,
        samples: u16,
    ) -> Result<Box<dyn AudioSink>> {
        let stream = Arc::new(CpalAudioStream::new(freq, channels, samples)?);
        self.streams.push(stream.clone());
        let stream_sink = CpalAudioStreamSink { stream };
        Ok(Box::new(stream_sink))
    }
}
