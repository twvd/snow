//! Audio from the machine to the host, in three parts:
//! - [`PacedAudioSink`] holds the emulator to the speed of the real machine by
//!   the wall clock, a few samples at a time.
//! - [`ChannelAudioSink`] queues the samples for the host.
//! - [`AudioPlayer`] plays them from the host's audio callback, keeping a steady
//!   amount queued: it resamples very slightly, to make up for the host's audio
//!   clock running a little faster or slower than the wall clock.
//!
//! Pacing the emulator by the host's audio callbacks instead lets it run in
//! bursts, a callback's worth at a time, which shows as uneven motion on screen.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};

use super::{
    AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS, AudioBuffer, AudioProvider, AudioSink, null_audio_sink,
};

/// Frames the machines hand over at a time. The emulator is held to the wall
/// clock at each, so they are few: 32 frames are 1.4 ms at 22 kHz.
pub const AUDIO_CHUNK_FRAMES: usize = 32;

/// Size of a chunk, of all channels
pub const AUDIO_CHUNK_SIZE: usize = AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS;

/// How much audio playback keeps queued, to ride out the emulator thread
/// falling behind for a moment
const TARGET_QUEUED: Duration = Duration::from_millis(40);

/// Audio beyond this many times the target is dropped, should playback stall
const MAX_QUEUED_TARGETS: usize = 4;

/// Most the playback speed changes to keep the target queued: 0.5%, too little
/// to hear
const MAX_RATE_CORRECTION: f64 = 0.005;

/// How far the emulator can fall behind the wall clock before it gives up
/// catching up, as after a pause or while it can't keep up
const MAX_BEHIND: Duration = Duration::from_millis(50);

/// Receives the audio of a [`ChannelAudioSink`]
#[derive(Clone)]
pub struct AudioReceiver {
    receiver: Receiver<AudioBuffer>,
    /// Samples queued, of all channels
    queued: Arc<AtomicUsize>,
}

impl AudioReceiver {
    pub fn try_recv(&self) -> Option<AudioBuffer> {
        let buffer = self.receiver.try_recv().ok()?;
        self.queued.fetch_sub(buffer.len(), Ordering::AcqRel);
        Some(buffer)
    }

    /// Samples queued, of all channels
    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }
}

/// Queues audio for the host
pub struct ChannelAudioSink {
    sender: Sender<AudioBuffer>,
    receiver: AudioReceiver,
    channels: usize,
    /// Samples to keep queued, of all channels
    target: usize,
}

impl ChannelAudioSink {
    pub fn new(freq: i32, channels: u8) -> Self {
        let (sender, receiver) = crossbeam_channel::unbounded();
        let channels = usize::from(channels.max(1));
        let target = (f64::from(freq) * TARGET_QUEUED.as_secs_f64()) as usize * channels;
        Self {
            sender,
            receiver: AudioReceiver {
                receiver,
                queued: Arc::default(),
            },
            channels,
            target,
        }
    }

    pub fn receiver(&self) -> AudioReceiver {
        self.receiver.clone()
    }

    /// Clears/flushes the entire current content in the sink
    pub fn clear(&self) {
        while self.receiver.try_recv().is_some() {}
    }
}

impl AudioSink for ChannelAudioSink {
    /// Queues the samples, or drops them if playback has stalled. Never blocks.
    fn send(&self, buffer: AudioBuffer) -> Result<()> {
        if self.receiver.queued() + buffer.len() > self.target * MAX_QUEUED_TARGETS {
            return Ok(());
        }
        self.receiver
            .queued
            .fetch_add(buffer.len(), Ordering::AcqRel);
        self.sender.send(buffer)?;
        Ok(())
    }

    /// Whether the target amount is queued, for sources that fill the queue
    /// as playback empties it, rather than at their own pace
    fn is_full(&self) -> bool {
        self.receiver.queued() >= self.target
    }

    fn is_empty(&self) -> bool {
        self.receiver.queued() == 0
    }
}

/// What [`AudioPlayer::fill`] played
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFill {
    /// Silence, until the target amount is queued
    Prebuffering,
    Played,
    /// Ran out of audio, and played silence for the rest
    Underrun,
}

/// Plays the audio of a [`ChannelAudioSink`], from the host's audio callback
pub struct AudioPlayer {
    receiver: AudioReceiver,
    channels: usize,
    target: usize,
    prebuffering: bool,
    /// Samples taken from the queue
    active: VecDeque<f32>,
    /// The frame before the front of `active`, and how far playback is from it
    /// to the next, in frames
    previous: Vec<f32>,
    position: f64,
}

impl AudioPlayer {
    pub fn new(sink: &ChannelAudioSink) -> Self {
        Self {
            receiver: sink.receiver(),
            channels: sink.channels,
            target: sink.target,
            prebuffering: true,
            active: VecDeque::new(),
            previous: vec![0.0; sink.channels],
            position: 0.0,
        }
    }

    /// Samples queued, of all channels
    fn queued(&self) -> usize {
        self.active.len() + self.receiver.queued()
    }

    /// Takes queued samples until a frame is ready, if there is one
    fn frame_ready(&mut self) -> bool {
        while self.active.len() < self.channels {
            let Some(buffer) = self.receiver.try_recv() else {
                return false;
            };
            self.active.extend(buffer.iter());
        }
        true
    }

    /// Fills `out` with interleaved samples
    pub fn fill(&mut self, out: &mut [f32]) -> AudioFill {
        if self.prebuffering {
            if self.queued() < self.target {
                out.fill(0.0);
                return AudioFill::Prebuffering;
            }
            self.prebuffering = false;
        }

        // Play a little faster with more than the target queued, slower with less
        let deviation = (self.queued() as f64 - self.target as f64) / self.target as f64;
        let rate = 1.0 + (deviation * 0.01).clamp(-MAX_RATE_CORRECTION, MAX_RATE_CORRECTION);

        let channels = self.channels;
        for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
            if !self.frame_ready() {
                out[i * channels..].fill(0.0);
                self.active.clear();
                self.prebuffering = true;
                return AudioFill::Underrun;
            }
            // Between the previous frame and the next, linearly
            let t = self.position as f32;
            for (c, sample) in frame.iter_mut().enumerate() {
                *sample = (self.active[c] - self.previous[c]).mul_add(t, self.previous[c]);
            }

            self.position += rate;
            while self.position >= 1.0 && self.frame_ready() {
                for previous in &mut self.previous {
                    *previous = self.active.pop_front().unwrap_or_default();
                }
                self.position -= 1.0;
            }
        }
        AudioFill::Played
    }
}

/// How fast the machine runs relative to the real one, while paced by the wall
/// clock.
///
/// It is 1.0, unless a frontend locks the machine's frames to the display's
/// refresh, which takes a speed slightly off (see [`super::DisplayLock`]).
#[derive(Clone)]
pub struct PaceControl(Arc<AtomicU64>);

impl Default for PaceControl {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(1.0f64.to_bits())))
    }
}

impl PaceControl {
    pub fn set(&self, speed: f64) {
        self.0.store(speed.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Holds the emulator to the speed of the real machine by the wall clock: each
/// chunk of audio waits until the time it is for has come
pub struct PacedAudioSink {
    inner: Box<dyn AudioSink>,
    /// Frames per second of the machine's sound
    rate: f64,
    pace: Option<PaceControl>,
    /// When the frames sent so far are due
    due: Cell<Option<Instant>>,
}

impl PacedAudioSink {
    pub fn new(inner: Box<dyn AudioSink>, rate: f64, pace: Option<PaceControl>) -> Self {
        Self {
            inner,
            rate,
            pace,
            due: Cell::new(None),
        }
    }
}

impl AudioSink for PacedAudioSink {
    fn send(&self, buffer: AudioBuffer) -> Result<()> {
        let speed = self.pace.as_ref().map_or(1.0, PaceControl::get);
        let frames = (buffer.len() / AUDIO_CHANNELS) as f64;
        let now = Instant::now();
        let mut due =
            self.due.get().unwrap_or(now) + Duration::from_secs_f64(frames / (self.rate * speed));

        if due > now {
            thread::sleep(due - now);
        } else if now - due > MAX_BEHIND {
            // Start again from now, rather than run flat out to catch up
            due = now;
        }
        self.due.set(Some(due));
        self.inner.send(buffer)
    }

    fn is_full(&self) -> bool {
        self.inner.is_full()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Opens the audio stream of a machine's sound, which paces the emulator. `rate`
/// is the frames per second the sound hardware makes.
pub fn machine_audio_sink(provider: &mut dyn AudioProvider, rate: f64) -> Box<dyn AudioSink> {
    match provider.create_stream(
        rate.round() as i32,
        AUDIO_CHANNELS as u8,
        AUDIO_BUFFER_SAMPLES as u16,
    ) {
        Ok(sink) => Box::new(PacedAudioSink::new(sink, rate, provider.pace_control())),
        Err(e) => {
            log::warn!("Cannot open an audio stream: {:#}", e);
            null_audio_sink()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(value: f32, frames: usize) -> AudioBuffer {
        vec![value; frames * AUDIO_CHANNELS].into_boxed_slice()
    }

    #[test]
    fn player_prebuffers_then_plays() {
        let sink = ChannelAudioSink::new(22050, 2);
        let mut player = AudioPlayer::new(&sink);
        let mut out = vec![1.0; 256];
        assert_eq!(player.fill(&mut out), AudioFill::Prebuffering);
        assert!(out.iter().all(|&s| s == 0.0));

        while !sink.is_full() {
            sink.send(chunk(0.5, AUDIO_CHUNK_FRAMES)).unwrap();
        }
        assert_eq!(player.fill(&mut out), AudioFill::Played);
        // After the first frame, which comes from silence
        assert!(out[2..].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn player_underruns() {
        let sink = ChannelAudioSink::new(22050, 2);
        let mut player = AudioPlayer::new(&sink);
        while !sink.is_full() {
            sink.send(chunk(0.5, AUDIO_CHUNK_FRAMES)).unwrap();
        }
        let mut out = vec![1.0; sink.target * 2];
        assert_eq!(player.fill(&mut out), AudioFill::Underrun);
        assert_eq!(*out.last().unwrap(), 0.0);
        assert_eq!(player.fill(&mut out), AudioFill::Prebuffering);
    }

    #[test]
    fn player_corrects_rate() {
        let sink = ChannelAudioSink::new(22050, 2);
        let mut player = AudioPlayer::new(&sink);
        // Twice the target queued: plays faster, so takes more than it gives
        while sink.receiver.queued() < sink.target * 2 {
            sink.send(chunk(0.5, AUDIO_CHUNK_FRAMES)).unwrap();
        }
        let before = player.queued();
        let mut out = vec![0.0; 1000 * AUDIO_CHANNELS];
        assert_eq!(player.fill(&mut out), AudioFill::Played);
        let taken = (before - player.queued()) / AUDIO_CHANNELS;
        // 0.5% faster, give or take the rounding
        assert!((1004..=1005).contains(&taken), "{taken}");
    }

    #[test]
    fn sink_drops_when_playback_stalls() {
        let sink = ChannelAudioSink::new(22050, 2);
        for _ in 0..1000 {
            sink.send(chunk(0.5, AUDIO_CHUNK_FRAMES)).unwrap();
        }
        assert!(sink.receiver.queued() <= sink.target * MAX_QUEUED_TARGETS);
    }

    #[test]
    fn paced_sink_keeps_to_the_clock() {
        let sink = PacedAudioSink::new(null_audio_sink(), 22050.0, None);
        let start = Instant::now();
        // 50 ms of audio
        for _ in 0..(22050 / 20 / AUDIO_CHUNK_FRAMES) {
            sink.send(chunk(0.0, AUDIO_CHUNK_FRAMES)).unwrap();
        }
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(45), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(100), "{elapsed:?}");
    }
}
