//! Audio output of the sound chips: the output stage, from the samples of
//! the channels to the audio sink

use crate::audio_filter::AudioFilter;
use crate::renderer::{AUDIO_BUFFER_SIZE, AudioSink, null_audio_sink};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Scales of the channels A and B for each output side, in 128ths: $80
/// passes a channel at its full level
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Panning {
    pub left: [u8; 2],
    pub right: [u8; 2],
}

impl Panning {
    /// A on the left, B on the right
    pub const STEREO: Self = Self {
        left: [0x80, 0],
        right: [0, 0x80],
    };

    /// A on the right, B on the left
    pub const SWAPPED: Self = Self {
        left: [0, 0x80],
        right: [0x80, 0],
    };

    /// A on both sides
    pub const MONO: Self = Self {
        left: [0x80, 0],
        right: [0x80, 0],
    };
}

/// An 8-bit binary offset sample as a 16-bit two's complement one
pub const fn sample16(sample: u8) -> i16 {
    ((sample ^ 0x80) as i8 as i16) << 8
}

/// One output side: the samples of A and B mixed by their scales for that
/// side, and clipped
pub fn mix(samples: [i16; 2], scales: [u8; 2]) -> i16 {
    let [a, b] = samples;
    let [sa, sb] = scales;
    let sum = i32::from(a) * i32::from(sa) + i32::from(b) * i32::from(sb);
    (sum >> 7).clamp(-0x8000, 0x7FFF) as i16
}

/// Gains of the eight volume settings, 0 to 7
///
/// The system passes the setting to the hardware unchanged, and doesn't
/// scale the samples it plays:
///
/// - The compact Macs take it on VIA port A bits 0-2.
/// - The Apple Sound Chip and its successors take it in bits 5-7 of their
///   volume register.
/// - The LC III takes it in the DFAC, through Egret.
///
/// The hardware then attenuates the sound: a resistor network on the
/// Macintosh Plus analog board, the Sony sound chip, Sporty, the DFAC, or the
/// sound chip itself. Only the Plus network is known: its resistors, R12,
/// R13, R16 and R17 switched by a CD4016, give -21.6, -15.6, -9.3, -7.4, -3.6,
/// -2.6, -0.7 and 0 dB relative to level 7, as MAME computes them. The same
/// curve is used for all the machines, with level 0 muted.
const VOLUME_GAINS: [f32; 8] = [0.0, 0.166, 0.3428, 0.4266, 0.6607, 0.7413, 0.9226, 1.0];

/// Gain of a volume setting, 0 to 7
pub const fn volume_gain(volume: u8) -> f32 {
    VOLUME_GAINS[(volume & 7) as usize]
}

/// Gain of 1, in the 1/32768ths of [`AudioPath::gain`]
const GAIN_ONE: i32 = 0x8000;

/// Takes the samples of the channels of a sound chip to the audio sink:
/// mixes them by their scales, applies the volume, adds the analog input
/// and filters the result
#[derive(Serialize, Deserialize)]
pub struct AudioPath {
    #[serde(skip, default = "null_audio_sink")]
    sink: Box<dyn AudioSink>,

    /// Gain of the volume, in 1/32768ths, applied to the samples of the
    /// channels but not to the analog input
    gain: i32,

    /// Last samples pushed, to tell when the sound starts
    last: [i16; 2],

    /// The samples changed during the buffer being filled
    changed: bool,

    buffer: Vec<f32>,
    silent: bool,
    filter: AudioFilter,
}

impl Default for AudioPath {
    fn default() -> Self {
        Self {
            sink: null_audio_sink(),
            gain: GAIN_ONE,
            last: [0; 2],
            changed: false,
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            silent: true,
            filter: AudioFilter::new(),
        }
    }
}

impl AudioPath {
    pub fn reset(&mut self) {
        self.filter.reset();
    }

    /// No sound plays: the samples didn't change during the last buffer sent,
    /// nor since. The volume doesn't count, so that a sound keeps the pace of
    /// the emulation at any volume.
    pub fn is_silent(&self) -> bool {
        self.silent
    }

    /// Sets the gain of the volume, 0.0 to 1.0
    pub fn set_gain(&mut self, gain: f32) {
        self.gain = (gain.clamp(0.0, 1.0) * GAIN_ONE as f32) as i32;
    }

    /// Pushes the output for one 16-bit sample of each channel, and the
    /// analog input (-1.0 to 1.0) on both sides
    pub fn push(&mut self, samples: [i16; 2], panning: Panning, input: f32) -> Result<()> {
        if samples != self.last {
            self.last = samples;
            self.silent = false;
            self.changed = true;
        }
        let side = |scales| {
            let level = (i32::from(mix(samples, scales)) * self.gain) >> 15;
            (level as f32 / 32768.0 + input).clamp(-1.0, 1.0)
        };
        self.queue(side(panning.left), side(panning.right))
    }

    fn queue(&mut self, sample_l: f32, sample_r: f32) -> Result<()> {
        // Apply filters
        let (filtered_l, filtered_r) = self.filter.filter_stereo(sample_l, sample_r);

        self.buffer.push(filtered_l);
        self.buffer.push(filtered_r);
        // Assuming we're always aligned to 2 here
        if self.buffer.len() >= AUDIO_BUFFER_SIZE {
            let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(AUDIO_BUFFER_SIZE));
            self.silent = !self.changed;
            self.changed = false;
            self.sink.send(buffer.into_boxed_slice())?;
        }
        Ok(())
    }

    pub fn set_sink(&mut self, sink: Box<dyn AudioSink>) {
        self.sink = sink;
    }

    pub fn after_deserialize(&mut self) {
        self.set_sink(null_audio_sink());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples() {
        assert_eq!(sample16(0x80), 0);
        assert_eq!(sample16(0xFF), 0x7F00);
        assert_eq!(sample16(0x00), -0x8000);
    }

    #[test]
    fn panning() {
        let samples = [0x1000, -0x2000];
        let side = |p: Panning| (mix(samples, p.left), mix(samples, p.right));
        assert_eq!(side(Panning::STEREO), (0x1000, -0x2000));
        assert_eq!(side(Panning::SWAPPED), (-0x2000, 0x1000));
        assert_eq!(side(Panning::MONO), (0x1000, 0x1000));
    }

    #[test]
    fn volume_levels() {
        assert_eq!(volume_gain(0), 0.0);
        assert_eq!(volume_gain(7), 1.0);
        assert!((volume_gain(4) - 0.6607).abs() < 0.0001);
        assert!((1..7).all(|v| volume_gain(v) < volume_gain(v + 1)));
    }
}
