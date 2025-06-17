use crate::bus::{Address, BusMember};
use crate::renderer::AUDIO_BUFFER_SIZE;
use crate::tickable::Ticks;
use crate::types::Field32;
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};

pub const AUDIO_QUEUE_LEN: usize = 2;

pub type AudioBuffer = Box<[u8]>;

/// Apple Sound Chip
pub struct Asc {
    sender: Sender<AudioBuffer>,
    pub receiver: Receiver<AudioBuffer>,
    buffer: Vec<u8>,
    silent: bool,

    mode: AscMode,
    channels: [AscChannel; 4],
    wavetables: [u8; 0x800],
}

#[derive(Default)]
struct AscChannel {
    freq: Field32,
    phase: Field32,
}

#[derive(FromPrimitive, ToPrimitive)]
enum AscMode {
    Off = 0,
    Fifo = 1,
    Wavetable = 2,
}

impl Default for Asc {
    fn default() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(AUDIO_QUEUE_LEN);
        Self {
            sender,
            receiver,
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            silent: true,
            channels: Default::default(),
            wavetables: [0; 0x800],
            mode: AscMode::Off,
        }
    }
}

impl Asc {
    pub fn is_silent(&self) -> bool {
        self.silent
    }

    pub fn push(&mut self, val: u8) -> Result<()> {
        if val != 0 && val != 0xFF {
            self.silent = false;
        }

        self.buffer.push(val);
        if self.buffer.len() >= AUDIO_BUFFER_SIZE {
            let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(AUDIO_BUFFER_SIZE));
            self.silent = buffer.iter().all(|&s| s == buffer[0]);
            self.sender.send(buffer.into_boxed_slice())?;
        }
        Ok(())
    }

    /// Sample the ASC for wavetable mode
    fn sample_wavetable(&mut self) -> u8 {
        let mut sample = 0;
        for (i, c) in self.channels.iter_mut().enumerate() {
            c.phase.0 += c.freq.0;

            let table_offset = (c.phase.0 >> 15) & 0x1FF;
            sample += self.wavetables[i * 0x200 + table_offset as usize] as u16;
        }
        (sample >> 2) as u8
    }

    /// Ticks the ASC at the sample rate
    pub fn tick(&mut self, queue_sample: bool) -> Result<()> {
        let sample = match self.mode {
            AscMode::Off => 0,
            AscMode::Fifo => 0,
            AscMode::Wavetable => self.sample_wavetable(),
        };
        if queue_sample {
            self.push(sample)?;
        }

        Ok(())
    }

    pub const fn sample_rate(&self) -> Ticks {
        // TODO configurable sample rate
        22257
    }
}

impl BusMember<Address> for Asc {
    fn read(&mut self, addr: Address) -> Option<u8> {
        match addr {
            // Sample buffer
            0x000..=0x7FF => Some(self.wavetables[addr as usize]),
            // Version
            0x800 => Some(0), // ASC v1
            // Mode
            0x801 => Some(self.mode.to_u8().unwrap()),
            // FIFO status
            0x804 => Some(0xFF),
            // Channel configuration
            0x810..=0x82F => {
                let channel = (((addr - 0x810) >> 3) & 3) as usize;
                if addr & 4 == 0 {
                    Some(self.channels[channel].phase.be((addr & 3) as usize))
                } else {
                    Some(self.channels[channel].freq.be((addr & 3) as usize))
                }
            }
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        match addr {
            // Sample buffer
            0x000..=0x7FF => Some(self.wavetables[addr as usize] = val),
            // Mode
            0x801 => Some(self.mode = AscMode::from_u8(val).unwrap_or(AscMode::Off)),
            // FIFO status
            0x804 => Some(()),
            // Clock rate
            0x807 => {
                log::debug!("Clock rate = {}", val);
                Some(())
            }
            // Channel configuration
            0x810..=0x82F => {
                let channel = (((addr - 0x810) >> 3) & 3) as usize;
                if addr & 4 == 0 {
                    Some(
                        self.channels[channel]
                            .phase
                            .set_be((addr & 3) as usize, val),
                    )
                } else {
                    Some(self.channels[channel].freq.set_be((addr & 3) as usize, val))
                }
            }
            _ => None,
        }
    }
}
