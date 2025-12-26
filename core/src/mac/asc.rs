use std::collections::VecDeque;

use crate::audio_filter::AudioFilter;
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
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

pub const AUDIO_QUEUE_LEN: usize = 2;
const FIFO_SIZE: usize = 0x400;

pub type AudioBuffer = Box<[f32]>;

/// Apple Sound Chip
#[derive(Serialize, Deserialize)]
pub struct Asc {
    #[serde(skip)]
    sender: Option<Sender<AudioBuffer>>,
    #[serde(skip)]
    pub receiver: Option<Receiver<AudioBuffer>>,

    buffer: Vec<f32>,
    silent: bool,

    mode: AscMode,
    ctrl: Control,
    channels: [AscChannel; 4],
    #[serde(with = "BigArray")]
    wavetables: [u8; 0x800],
    fifo_status: FifoStatus,
    irq: bool,
    fifo_l: VecDeque<u8>,
    fifo_r: VecDeque<u8>,

    /// Last sample played on left channel
    l_last: u8,

    /// Last sample played on right channel
    r_last: u8,

    /// Countdown to generate IRQs only every FIFO_SIZE (empty) samples when
    /// FIFO is empty
    empty_cycles: Ticks,

    filter: AudioFilter,
}

bitfield! {
    /// FIFO status register
    #[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct FifoStatus(pub Byte): Debug, FromStorage, IntoStorage, DerefStorage {
        pub l_half: bool @ 0,
        pub l_fullempty: bool @ 1,
        pub r_half: bool @ 2,
        pub r_fullempty: bool @ 3,
    }
}

bitfield! {
    /// Control register
    #[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct Control(pub Byte): Debug, FromStorage, IntoStorage, DerefStorage {
        pub stereo: bool @ 1,
    }
}

#[derive(Default, Serialize, Deserialize)]
struct AscChannel {
    freq: Field32,
    phase: Field32,
}

#[derive(
    Clone,
    Copy,
    Eq,
    PartialEq,
    FromPrimitive,
    ToPrimitive,
    strum::IntoStaticStr,
    Serialize,
    Deserialize,
)]
enum AscMode {
    Off = 0,
    Fifo = 1,
    Wavetable = 2,
}

impl Default for Asc {
    fn default() -> Self {
        let mut result = Self {
            sender: None,
            receiver: None,
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            silent: true,
            channels: Default::default(),
            wavetables: [0; 0x800],
            mode: AscMode::Off,
            fifo_l: VecDeque::with_capacity(FIFO_SIZE),
            fifo_r: VecDeque::with_capacity(FIFO_SIZE),
            fifo_status: Default::default(),
            irq: false,
            ctrl: Control(0),
            l_last: 0,
            r_last: 0,
            empty_cycles: 0,
            filter: AudioFilter::new(),
        };
        result.init_channels();
        result
    }
}

impl Asc {
    pub fn reset(&mut self) {
        self.irq = false;
        self.fifo_status.0 = 0;
        self.mode = AscMode::Off;
        self.fifo_l.clear();
        self.fifo_r.clear();
        self.wavetables.fill(0);
        self.empty_cycles = 0;
        self.filter.reset();
    }

    pub fn is_silent(&self) -> bool {
        self.silent
    }

    pub fn get_irq(&self) -> bool {
        self.irq
    }

    fn push(&mut self, l: u8, r: u8) -> Result<()> {
        if (l, r) != (0, 0) && (l, r) != (0xFF, 0xFF) {
            self.silent = false;
        }

        // Convert u8 (0-255) to f32 in standard audio range (-1.0 to 1.0)
        let sample_l = ((l as f32) - 128.0) / 128.0;
        let sample_r = ((r as f32) - 128.0) / 128.0;

        // Apply filters
        let (filtered_l, filtered_r) = self.filter.filter_stereo(sample_l, sample_r);

        self.buffer.push(filtered_l);
        self.buffer.push(filtered_r);
        // Assuming we're always aligned to 2 here
        if self.buffer.len() >= AUDIO_BUFFER_SIZE {
            let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(AUDIO_BUFFER_SIZE));
            self.silent = buffer.iter().all(|&s| s.abs() < 0.01);
            self.sender
                .as_ref()
                .unwrap()
                .send(buffer.into_boxed_slice())?;
        }
        Ok(())
    }

    /// Sample the ASC for wavetable mode
    fn sample_wavetable(&mut self) -> (u8, u8) {
        let mut sample = 0;
        for (i, c) in self.channels.iter_mut().enumerate() {
            c.phase.0 += c.freq.0;

            let table_offset = (c.phase.0 >> 15) & 0x1FF;
            sample += self.wavetables[i * 0x200 + table_offset as usize] as u16;
        }
        ((sample >> 2) as u8, (sample >> 2) as u8)
    }

    /// Sample the ASC for FIFO mode
    fn sample_fifo(&mut self) -> (u8, u8) {
        // If a FIFO is empty, the ASC will continue to output the last sample of that channel
        let l = self.fifo_l.pop_front().unwrap_or(self.l_last);
        let r = if self.ctrl.stereo() {
            self.fifo_r.pop_front().unwrap_or(self.r_last)
        } else {
            l
        };
        self.l_last = l;
        self.r_last = r;

        // Set FIFO status bits
        if self.fifo_l.len() == FIFO_SIZE / 2 {
            self.fifo_status.set_l_half(true);
            self.irq = true;
        }
        if self.fifo_r.len() == FIFO_SIZE / 2 {
            self.fifo_status.set_r_half(true);
            self.irq = true;
        }

        if self.fifo_l.is_empty() && self.fifo_r.is_empty() {
            // When outputting alert sounds, MacOS leaves the FIFO mode on and still expects IRQs,
            // but too many will freeze the system. 'empty_cycles' will generate an IRQ once for
            // every FIFO_SIZE (empty) samples.
            if self.empty_cycles == 0 {
                self.fifo_status = FifoStatus::default()
                    .with_l_half(true)
                    .with_r_half(true)
                    .with_l_fullempty(true)
                    .with_r_fullempty(true);
                self.irq = true;
                self.empty_cycles = FIFO_SIZE;
            } else {
                self.empty_cycles -= 1;
            }
        } else {
            self.empty_cycles = 0;
        }

        (l, r)
    }

    /// Ticks the ASC at the sample rate
    pub fn tick(&mut self, queue_sample: bool) -> Result<()> {
        let (l, r) = match self.mode {
            AscMode::Off => (0, 0),
            AscMode::Fifo => self.sample_fifo(),
            AscMode::Wavetable => self.sample_wavetable(),
        };
        if queue_sample {
            self.push(l, r)?;
        }

        Ok(())
    }

    pub const fn sample_rate(&self) -> Ticks {
        // TODO configurable sample rate
        22257
    }

    pub fn init_channels(&mut self) {
        let (sender, receiver) = crossbeam_channel::bounded(AUDIO_QUEUE_LEN);
        self.sender = Some(sender);
        self.receiver = Some(receiver);
    }

    pub fn after_deserialize(&mut self) {
        self.init_channels();
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
            // Control
            0x802 => Some(self.ctrl.0),
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
            // Control
            0x802 => Some(self.ctrl.0 = val),
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
            dbgprop_bool!("Stereo", self.ctrl.stereo()),
            dbgprop_header!("FIFO"),
            dbgprop_byte_bin!("FIFO status", self.fifo_status.0),
            dbgprop_udec!("FIFO L level", self.fifo_l.len()),
            dbgprop_udec!("FIFO R level", self.fifo_r.len()),
            dbgprop_udec!("Empty cycles", self.empty_cycles),
        ]
    }
}
