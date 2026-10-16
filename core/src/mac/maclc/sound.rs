//! Sound of the V8 gate array
//!
//! This is a cut down Apple Sound Chip: one mono FIFO, no wavetable mode,
//! and an interrupt which is a level, asserted for as long as the FIFO is
//! less than half full, so also when no sound is played. The system masks
//! it in the V8. Sound input is not implemented.
//!
//! The last sample played is held when the FIFO is empty, so that the output
//! does not jump when a sound starts or ends.

use std::collections::VecDeque;

use crate::audio_filter::AudioFilter;
use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::renderer::{AUDIO_BUFFER_SIZE, AudioSink, null_audio_sink};
use crate::tickable::Ticks;
use crate::types::Byte;

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

const FIFO_SIZE: usize = 0x400;

/// Version of the sound chip of the V8
const VERSION: u8 = 0xE8;

const REG_VERSION: Address = 0x800;
const REG_MODE: Address = 0x801;
const REG_CONTROL: Address = 0x802;
const REG_FIFO_MODE: Address = 0x803;
const REG_FIFO_STATUS: Address = 0x804;
const REG_WAVETABLE_CONTROL: Address = 0x805;
const REG_VOLUME: Address = 0x806;
const REG_CLOCK: Address = 0x807;
const REG_PLAY_RECORD: Address = 0x80A;
const REG_TEST: Address = 0x80F;

bitfield! {
    /// FIFO status register
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct FifoStatus(pub Byte): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Less than half full
        pub half: bool @ 0,
        /// Empty or full
        pub fullempty: bool @ 1,
    }
}

#[derive(Serialize, Deserialize)]
pub struct Sound {
    #[serde(skip, default = "null_audio_sink")]
    sink: Box<dyn AudioSink>,

    buffer: Vec<f32>,
    fifo: VecDeque<u8>,

    /// Last sample played, which goes on when the FIFO is empty
    last: u8,

    /// Volume register. The volume is not applied.
    volume: u8,

    /// Play and record register, for the sound input
    play_record: u8,

    filter: AudioFilter,
}

impl Default for Sound {
    fn default() -> Self {
        Self {
            sink: null_audio_sink(),
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            fifo: VecDeque::with_capacity(FIFO_SIZE),
            last: 0x80,
            volume: 0,
            play_record: 0,
            filter: AudioFilter::new(),
        }
    }
}

impl Sound {
    pub fn reset(&mut self) {
        self.fifo.clear();
        self.last = 0x80;
        self.volume = 0;
        self.play_record = 0;
        self.filter.reset();
    }

    fn fifo_status(&self) -> FifoStatus {
        FifoStatus(0)
            .with_half(self.fifo.len() < FIFO_SIZE / 2)
            .with_fullempty(self.fifo.is_empty() || self.fifo.len() == FIFO_SIZE)
    }

    /// Level of the interrupt output
    pub fn irq(&self) -> bool {
        self.fifo_status().half()
    }

    fn push(&mut self, sample: u8) -> Result<()> {
        // Convert u8 (0-255) to f32 in standard audio range (-1.0 to 1.0)
        let sample = self.filter.filter_mono((f32::from(sample) - 128.0) / 128.0);

        self.buffer.push(sample);
        self.buffer.push(sample);
        if self.buffer.len() >= AUDIO_BUFFER_SIZE {
            let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(AUDIO_BUFFER_SIZE));
            self.sink.send(buffer.into_boxed_slice())?;
        }
        Ok(())
    }

    /// Plays a sample, at the sample rate
    pub fn tick(&mut self, queue_sample: bool) -> Result<()> {
        self.last = self.fifo.pop_front().unwrap_or(self.last);
        if queue_sample {
            self.push(self.last)?;
        }
        Ok(())
    }

    pub const fn sample_rate(&self) -> Ticks {
        22257
    }

    pub fn set_sink(&mut self, sink: Box<dyn AudioSink>) {
        self.sink = sink;
    }

    pub fn after_deserialize(&mut self) {
        self.set_sink(null_audio_sink());
    }
}

impl BusMember<Address> for Sound {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            // What a read of the FIFO returns is not known
            0x000..=0x7FF => Some(0),
            REG_VERSION => Some(VERSION),
            // The chip is always in FIFO mode
            REG_MODE | REG_CONTROL | REG_FIFO_MODE => Some(1),
            REG_FIFO_STATUS => Some(self.fifo_status().0),
            REG_WAVETABLE_CONTROL | REG_CLOCK | REG_TEST => Some(0),
            REG_VOLUME => Some(self.volume),
            REG_PLAY_RECORD => Some(self.play_record),
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            0x000..=0x3FF => {
                if self.fifo.len() < FIFO_SIZE {
                    self.fifo.push_back(val);
                }
            }
            // There is no second FIFO
            0x400..=0x7FF => (),
            REG_FIFO_MODE => {
                if val & 0x80 != 0 {
                    self.fifo.clear();
                }
            }
            REG_VOLUME => self.volume = val,
            REG_PLAY_RECORD => self.play_record = val,
            // The status follows the FIFO, and the other registers are fixed
            REG_MODE
            | REG_CONTROL
            | REG_FIFO_STATUS
            | REG_WAVETABLE_CONTROL
            | REG_CLOCK
            | REG_TEST => (),
            _ => return None,
        }
        Some(())
    }
}

impl Debuggable for Sound {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_byte, dbgprop_byte_bin, dbgprop_udec};

        vec![
            dbgprop_udec!("FIFO level", self.fifo.len()),
            dbgprop_byte_bin!("FIFO status", self.fifo_status().0),
            dbgprop_bool!("IRQ", self.irq()),
            dbgprop_byte!("Volume", self.volume),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers() {
        let mut sound = Sound::default();
        assert_eq!(sound.read(REG_VERSION), Some(0xE8));
        assert_eq!(sound.read(REG_MODE), Some(1));
        sound.write(REG_MODE, 0);
        assert_eq!(sound.read(REG_MODE), Some(1));
        sound.write(REG_VOLUME, 0xE0);
        assert_eq!(sound.read(REG_VOLUME), Some(0xE0));
    }

    #[test]
    fn fifo_status_and_irq() {
        let mut sound = Sound::default();

        // Empty: the interrupt stays asserted, a read does not clear it
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x03));
        assert!(sound.irq());
        sound.write(REG_FIFO_STATUS, 0xFF);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x03));

        for _ in 0..FIFO_SIZE / 2 - 1 {
            sound.write(0x000, 0x80);
        }
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x01));
        sound.write(0x000, 0x80);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x00));
        assert!(!sound.irq());

        // The second FIFO does not exist
        sound.write(0x400, 0x80);
        assert_eq!(sound.fifo.len(), FIFO_SIZE / 2);

        for _ in 0..FIFO_SIZE {
            sound.write(0x000, 0x80);
        }
        assert_eq!(sound.fifo.len(), FIFO_SIZE);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x02));

        // Playing a sample brings the level below full
        sound.tick(false).unwrap();
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x00));

        sound.write(REG_FIFO_MODE, 0x80);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x03));
        assert!(sound.irq());
    }

    #[test]
    fn last_sample_held() {
        let mut sound = Sound::default();
        sound.write(0x000, 0x12);
        sound.tick(false).unwrap();
        sound.tick(false).unwrap();
        assert_eq!(sound.last, 0x12);
    }
}
