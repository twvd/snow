use std::collections::VecDeque;

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::renderer::AUDIO_BUFFER_SIZE;
use crate::tickable::Ticks;
use crate::types::{Byte, Field32};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use proc_bitfield::bitfield;

pub const AUDIO_QUEUE_LEN: usize = 2;
const FIFO_SIZE: usize = 0x400;

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
    fifo_status: FifoStatus,
    irq: bool,
    fifo_l: VecDeque<u8>,
    fifo_r: VecDeque<u8>,
}

bitfield! {
    /// FIFO status register
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct FifoStatus(pub Byte): Debug, FromStorage, IntoStorage, DerefStorage {
        pub l_half: bool @ 0,
        pub l_fullempty: bool @ 1,
        pub r_half: bool @ 2,
        pub r_fullempty: bool @ 3,
    }
}

#[derive(Default)]
struct AscChannel {
    freq: Field32,
    phase: Field32,
}

#[derive(Clone, Copy, Eq, PartialEq, FromPrimitive, ToPrimitive, strum::IntoStaticStr)]
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
            fifo_l: VecDeque::with_capacity(FIFO_SIZE),
            fifo_r: VecDeque::with_capacity(FIFO_SIZE),
            fifo_status: Default::default(),
            irq: false,
        }
    }
}

impl Asc {
    pub fn is_silent(&self) -> bool {
        self.silent
    }

    pub fn get_irq(&self) -> bool {
        self.irq
    }

    fn push(&mut self, val: u8) -> Result<()> {
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

    /// Sample the ASC for FIFO mode
    fn sample_fifo(&mut self) -> u8 {
        let l = self.fifo_l.pop_front().unwrap_or(0);
        let _r = self.fifo_r.pop_front().unwrap_or(0);

        // Set FIFO status bits
        if self.fifo_l.len() == FIFO_SIZE / 2 {
            self.fifo_status.set_l_half(true);
            self.irq = true;
        }
        if self.fifo_r.len() == FIFO_SIZE / 2 {
            self.fifo_status.set_r_half(true);
            self.irq = true;
        }
        if self.fifo_l.len() == 1 {
            self.fifo_status.set_l_fullempty(true);
            self.irq = true;
        }
        if self.fifo_r.len() == 1 {
            self.fifo_status.set_r_fullempty(true);
            self.irq = true;
        }

        // TODO stereo
        l
    }

    /// Ticks the ASC at the sample rate
    pub fn tick(&mut self, queue_sample: bool) -> Result<()> {
        let sample = match self.mode {
            AscMode::Off => 0,
            AscMode::Fifo => self.sample_fifo(),
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
            0x000..=0x7FF if self.mode == AscMode::Wavetable => {
                Some(self.wavetables[addr as usize])
            }
            // Version
            0x800 => Some(0), // ASC v1
            // Mode
            0x801 => Some(self.mode.to_u8().unwrap()),
            // FIFO status
            0x804 => {
                self.irq = false;
                Some(*std::mem::take(&mut self.fifo_status))
            }
            // Clock rate
            0x807 => Some(0),
            // Wavetable channel configuration
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
            0x000..=0x3FF if self.mode == AscMode::Fifo => {
                if self.fifo_l.len() < FIFO_SIZE {
                    self.fifo_l.push_back(val);
                }
                if self.fifo_l.len() == FIFO_SIZE {
                    self.fifo_status.set_l_fullempty(true);
                }
                Some(())
            }
            0x400..=0x7FF if self.mode == AscMode::Fifo => {
                if self.fifo_r.len() < FIFO_SIZE {
                    self.fifo_r.push_back(val);
                }
                if self.fifo_r.len() == FIFO_SIZE {
                    self.fifo_status.set_r_fullempty(true);
                }
                Some(())
            }
            // Wave tables
            0x000..=0x7FF if self.mode == AscMode::Wavetable => {
                Some(self.wavetables[addr as usize] = val)
            }
            // Off..
            0x000..=0x7FF => Some(()),
            // Mode
            0x801 => Some(self.mode = AscMode::from_u8(val).unwrap_or(AscMode::Off)),
            // FIFO control
            0x803 => {
                if val & 0x80 != 0 {
                    // Clear FIFOs
                    self.fifo_l.clear();
                    self.fifo_r.clear();
                    self.fifo_status.set_l_fullempty(true);
                    self.fifo_status.set_r_fullempty(true);
                }
                Some(())
            }
            // FIFO status
            0x804 => Some(self.fifo_status.0 = val),
            // Clock rate
            0x807 => {
                if val != 0 {
                    log::warn!("TODO Clock rate = {}", val);
                }
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

impl Debuggable for Asc {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_byte_bin, dbgprop_enum, dbgprop_header, dbgprop_udec};

        vec![
            dbgprop_enum!("Mode", self.mode),
            dbgprop_bool!("IRQ", self.irq),
            dbgprop_header!("FIFO"),
            dbgprop_byte_bin!("FIFO status", self.fifo_status.0),
            dbgprop_udec!("FIFO L level", self.fifo_l.len()),
            dbgprop_udec!("FIFO R level", self.fifo_r.len()),
        ]
    }
}
