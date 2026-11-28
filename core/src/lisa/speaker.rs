//! The speaker of the Lisa: the shift register of the keyboard VIA, rotating out
//! on CB2, and a D/A ladder for its volume on port B

use serde::{Deserialize, Serialize};

use crate::tickable::Ticks;

/// Shift register mode: out, free running at the rate of timer 2
const SR_MODE_FREE_RUN: u8 = 0b100;
/// VIA clocks per bit are 2 (T2 + this)
const T2_EXTRA_CLOCKS: Ticks = 2;
const BITS_IN_SR: Ticks = 8;

const SILENCE: u8 = 0x80;
/// Sample swing per step of the volume
const VOLUME_STEP: u8 = 16;

/// What the VIA has set up
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpeakerInputs {
    pub sr_mode: u8,
    pub sr: u8,
    pub t2_latch_low: u8,
    pub volume: u8,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Speaker {
    /// VIA clocks into the rotation
    clock: Ticks,
}

impl Speaker {
    /// Advances by VIA clocks and gives the sample at that point
    pub fn sample(&mut self, via_clocks: Ticks, inputs: SpeakerInputs) -> u8 {
        if inputs.sr_mode != SR_MODE_FREE_RUN {
            self.clock = 0;
            return SILENCE;
        }
        let per_bit = 2 * (Ticks::from(inputs.t2_latch_low) + T2_EXTRA_CLOCKS);
        self.clock = (self.clock + via_clocks) % (per_bit * BITS_IN_SR);
        let bit = (self.clock / per_bit) as u32;
        let swing = inputs.volume * VOLUME_STEP;
        if inputs.sr & (0x80 >> bit) != 0 {
            SILENCE + swing
        } else {
            SILENCE - swing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(t2_latch_low: u8, volume: u8) -> SpeakerInputs {
        SpeakerInputs {
            sr_mode: SR_MODE_FREE_RUN,
            sr: 0x0F,
            t2_latch_low,
            volume,
        }
    }

    #[test]
    fn silent_unless_rotating() {
        let mut speaker = Speaker::default();
        let off = SpeakerInputs {
            sr_mode: 0,
            ..tone(0x60, 7)
        };
        assert_eq!(speaker.sample(100, off), SILENCE);
    }

    /// The boot ROM's click on a 2/5: E / 16 (T2 + 2)
    #[test]
    fn square_wave_period() {
        let mut speaker = Speaker::default();
        let inputs = tone(0x60, 4);
        let period = 16 * (0x60 + 2);
        let mut rises = 0;
        let mut last = speaker.sample(0, inputs);
        for _ in 0..period * 10 {
            let now = speaker.sample(1, inputs);
            if now > last {
                rises += 1;
            }
            last = now;
        }
        assert_eq!(rises, 10);
    }

    #[test]
    fn volume_sets_the_swing() {
        let mut low = Speaker::default();
        let mut high = Speaker::default();
        assert_eq!(low.sample(0, tone(0x60, 1)), SILENCE - VOLUME_STEP);
        assert_eq!(high.sample(0, tone(0x60, 7)), SILENCE - 7 * VOLUME_STEP);
    }
}
