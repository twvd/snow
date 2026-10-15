//! Sample rate converter: upsamples to the output rate by a 12-point
//! convolution of the input with a sinc in a Blackman window, 6 lobes on
//! each side
//!
//! Each output sample is interpolated between the 6th and 7th of the last
//! 12 input samples, so the output lags the input by 6 samples: a sound
//! starts with 6 samples of silence, and its last 6 samples are only played
//! as more input comes in.

use std::f64::consts::PI;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

const TAPS: usize = 12;

/// Positions between two input samples the weights are computed for
const PHASES: usize = 256;

static WEIGHTS: LazyLock<[[f32; TAPS]; PHASES]> = LazyLock::new(weights);

fn weights() -> [[f32; TAPS]; PHASES] {
    std::array::from_fn(|phase| {
        let position = phase as f64 / PHASES as f64;
        let taps: [f64; TAPS] = std::array::from_fn(|k| windowed_sinc(k as f64 - 5.0 - position));
        let sum: f64 = taps.iter().sum();
        taps.map(|t| (t / sum) as f32)
    })
}

/// Sinc in a Blackman window from -6 to 6, 0.42 - 0.5 cos(2πx) + 0.08 cos(4πx)
fn windowed_sinc(d: f64) -> f64 {
    let sinc = if d == 0.0 {
        1.0
    } else {
        (PI * d).sin() / (PI * d)
    };
    let x = (d + 6.0) / 12.0;
    let window = 0.08f64.mul_add(
        (4.0 * PI * x).cos(),
        0.5f64.mul_add(-(2.0 * PI * x).cos(), 0.42),
    );
    sinc * window
}

#[derive(Default, Serialize, Deserialize)]
pub struct RateConverter {
    /// Last input samples, oldest first
    history: [i16; TAPS],

    /// Position between the 6th and 7th input samples, a 16-bit fraction
    position: u32,
}

impl RateConverter {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Computes an output sample, moving on by `step`, the input rate
    /// divided by the output rate as a 16-bit fraction. Takes an input
    /// sample with `input` when it passes one, and returns None without
    /// moving if there is none.
    pub fn convert(&mut self, step: u16, input: impl FnOnce() -> Option<i16>) -> Option<i16> {
        let position = self.position + u32::from(step);
        if position > 0xFFFF {
            let sample = input()?;
            self.history.rotate_left(1);
            self.history[TAPS - 1] = sample;
        }
        self.position = position & 0xFFFF;

        let weights = &WEIGHTS[(self.position as usize * PHASES) >> 16];
        let sum: f32 = self
            .history
            .iter()
            .zip(weights)
            .map(|(&s, &w)| f32::from(s) * w)
            .sum();
        Some(sum.round().clamp(-32768.0, 32767.0) as i16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant() {
        let mut converter = RateConverter::default();
        for i in 0..1000 {
            let sample = converter.convert(0x812F, || Some(10000)).unwrap();
            if i > 30 {
                assert_eq!(sample, 10000);
            }
        }
    }

    #[test]
    fn latency() {
        // Half the output rate: on an input sample every other output, which
        // is then the input sample 6 before
        let mut converter = RateConverter::default();
        let mut inputs = (1..).map(|n| n * 100);
        let mut outputs = vec![];
        for _ in 0..40 {
            outputs.push(converter.convert(0x8000, || inputs.next()).unwrap());
        }
        for n in 0..20 {
            let expected = if n < 6 { 0 } else { (n - 5) * 100 };
            assert_eq!(outputs[n * 2 + 1], expected as i16);
        }
    }

    #[test]
    fn no_input() {
        let mut converter = RateConverter::default();
        assert_eq!(converter.convert(0x8000, || None), Some(0));
        converter.convert(0x8000, || Some(100));
        converter.convert(0x8000, || None);

        // Without the input sample it needs, it stays where it is
        assert_eq!(converter.convert(0x8000, || None), None);
        assert_eq!(converter.position, 0x8000);
        assert_eq!(converter.history[TAPS - 1], 100);
        converter.convert(0x8000, || Some(200)).unwrap();
        assert_eq!(converter.history[TAPS - 2..], [100, 200]);
    }

    #[test]
    fn sine() {
        // 1 kHz at the Mac rate, no more than 0.5% off once past the start
        let mut converter = RateConverter::default();
        let rate = 22254.5;
        let mut n = 0;
        let mut input = || {
            n += 1;
            Some((20000.0 * (2.0 * PI * 1000.0 * f64::from(n) / rate).sin()) as i16)
        };
        let step = 0x812F;
        let mut position = 0.0;
        for i in 0..4000 {
            let sample = converter.convert(step, &mut input).unwrap();
            position += f64::from(step) / 65536.0;
            if i > 100 {
                // The sample 6 before the last one taken, plus the position
                let t = position.floor() - 6.0 + position.fract();
                let expected = 20000.0 * (2.0 * PI * 1000.0 * t / rate).sin();
                assert!(
                    (f64::from(sample) - expected).abs() < 100.0,
                    "{i}: {sample} {expected}"
                );
            }
        }
    }
}
