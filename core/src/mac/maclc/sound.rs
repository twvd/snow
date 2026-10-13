//! Sound of the V8 and Sonora gate arrays
//!
//! The V8 has a cut down Apple Sound Chip: one mono FIFO, no wavetable mode,
//! and an interrupt which is a level, asserted for as long as the FIFO is
//! less than half full, so also when no sound is played. The system masks
//! it in the V8.
//!
//! The Sonora has two channels, for stereo, each with its FIFO and a block
//! of registers, among which one disables its interrupt. The registers at
//! 0x800 and the interrupt output are common to both.
//!
//! Sound input is not implemented.
//!
//! The last sample played is held when a FIFO is empty, so that the output
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

/// Registers of the channels of the Sonora, a block for each
const CHANNEL_REGS: Address = 0xF00;
const CHANNEL_REGS_SIZE: Address = 0x20;

/// Register of a channel which disables its interrupt
const CHANNEL_REG_IRQ: usize = 0x09;

bitfield! {
    /// FIFO status register
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct FifoStatus(pub Byte): Debug, FromStorage, IntoStorage, DerefStorage {
        /// First FIFO less than half full (V8)
        pub half: bool @ 0,
        /// First FIFO empty or full
        pub fullempty: bool @ 1,
        /// One of the FIFOs less than half full (Sonora)
        pub half_b: bool @ 2,
        /// One of the FIFOs empty (Sonora)
        pub empty_b: bool @ 3,
    }
}

/// Gate array which the sound is part of
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SoundKind {
    V8,
    Sonora,
}

impl SoundKind {
    /// Version of the sound chip
    const fn version(self) -> u8 {
        match self {
            Self::V8 => 0xE8,
            Self::Sonora => 0xBC,
        }
    }

    const fn channels(self) -> usize {
        match self {
            Self::V8 => 1,
            Self::Sonora => 2,
        }
    }

    /// The channels have a block of registers
    const fn channel_regs(self) -> bool {
        matches!(self, Self::Sonora)
    }

    /// Value of the control and FIFO mode registers, which are fixed
    const fn control(self) -> u8 {
        match self {
            Self::V8 => 1,
            Self::Sonora => 0,
        }
    }
}

/// A channel: a FIFO and, in the Sonora, its registers
#[derive(Serialize, Deserialize)]
struct Channel {
    fifo: VecDeque<u8>,

    /// Last sample played, which goes on when the FIFO is empty
    last: u8,

    /// Interrupt of the channel disabled
    irq_disable: bool,

    /// Registers which are only stored: FIFO pointers, sample rate, volume,
    /// FIFO control, and coefficients for CD-XA sound
    regs: [u8; CHANNEL_REGS_SIZE as usize],
}

impl Channel {
    fn new() -> Self {
        Self {
            fifo: VecDeque::with_capacity(FIFO_SIZE),
            last: 0x80,
            irq_disable: false,
            regs: [0; CHANNEL_REGS_SIZE as usize],
        }
    }

    fn write_fifo(&mut self, val: Byte) {
        if self.fifo.len() < FIFO_SIZE {
            self.fifo.push_back(val);
        }
    }

    /// Takes the sample to play out of the FIFO
    fn tick(&mut self) -> u8 {
        self.last = self.fifo.pop_front().unwrap_or(self.last);
        self.last
    }

    fn half(&self) -> bool {
        self.fifo.len() < FIFO_SIZE / 2
    }

    fn empty(&self) -> bool {
        self.fifo.is_empty()
    }

    fn full(&self) -> bool {
        self.fifo.len() == FIFO_SIZE
    }

    fn read_reg(&self, reg: usize) -> Byte {
        match reg {
            CHANNEL_REG_IRQ => self.irq_disable.into(),
            _ => self.regs[reg],
        }
    }

    fn write_reg(&mut self, reg: usize, val: Byte) {
        match reg {
            CHANNEL_REG_IRQ => self.irq_disable = val & 1 != 0,
            _ => self.regs[reg] = val,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Sound {
    kind: SoundKind,

    #[serde(skip, default = "null_audio_sink")]
    sink: Box<dyn AudioSink>,

    buffer: Vec<f32>,

    /// One channel in the V8, left and right in the Sonora
    channels: Vec<Channel>,

    /// Volume register. The volume is not applied.
    volume: u8,

    /// Play and record register, for the sound input
    play_record: u8,

    filter: AudioFilter,
}

impl Sound {
    pub fn new(kind: SoundKind) -> Self {
        Self {
            kind,
            sink: null_audio_sink(),
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            channels: (0..kind.channels()).map(|_| Channel::new()).collect(),
            volume: 0,
            play_record: 0,
            filter: AudioFilter::new(),
        }
    }

    pub fn reset(&mut self) {
        for channel in &mut self.channels {
            *channel = Channel::new();
        }
        self.volume = 0;
        self.play_record = 0;
        self.filter.reset();
    }

    fn fifo_status(&self) -> FifoStatus {
        let ch = &self.channels;
        match self.kind {
            SoundKind::V8 => FifoStatus(0)
                .with_half(ch[0].half())
                .with_fullempty(ch[0].empty() || ch[0].full()),
            // The Sonora does not report each FIFO in its two bits as the
            // Apple Sound Chip does: bit 1 is set for as long as it is not
            // recording, and bits 2 and 3 are for both FIFOs. This follows
            // what the ROM expects and other emulators, and has not been
            // verified on hardware.
            SoundKind::Sonora => FifoStatus(0)
                .with_fullempty(self.play_record & 1 == 0)
                .with_half_b(ch.iter().any(Channel::half))
                .with_empty_b(ch.iter().any(Channel::empty)),
        }
    }

    /// Level of the interrupt output
    pub fn irq(&self) -> bool {
        let status = self.fifo_status();
        match self.kind {
            SoundKind::V8 => status.half(),
            // As for the status: the interrupt of the first channel follows
            // bit 1, that of the second the level of both FIFOs
            SoundKind::Sonora => {
                (status.fullempty() && !self.channels[0].irq_disable)
                    || (status.half_b() && !self.channels[1].irq_disable)
            }
        }
    }

    fn push(&mut self, l: u8, r: u8) -> Result<()> {
        // Convert u8 (0-255) to f32 in standard audio range (-1.0 to 1.0)
        let sample = |s: u8| (f32::from(s) - 128.0) / 128.0;
        let (l, r) = self.filter.filter_stereo(sample(l), sample(r));

        self.buffer.push(l);
        self.buffer.push(r);
        if self.buffer.len() >= AUDIO_BUFFER_SIZE {
            let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(AUDIO_BUFFER_SIZE));
            self.sink.send(buffer.into_boxed_slice())?;
        }
        Ok(())
    }

    /// Plays a sample, at the sample rate
    pub fn tick(&mut self, queue_sample: bool) -> Result<()> {
        let mut samples = [0; 2];
        for (sample, channel) in samples.iter_mut().zip(&mut self.channels) {
            *sample = channel.tick();
        }
        if queue_sample {
            // The only channel of the V8 goes to both sides
            let right = samples[self.channels.len() - 1];
            self.push(samples[0], right)?;
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

    /// Channel and number of the register at an address in the registers
    /// of the channels
    fn channel_reg(addr: Address) -> (usize, usize) {
        let offset = addr - CHANNEL_REGS;
        (
            (offset / CHANNEL_REGS_SIZE) as usize,
            (offset % CHANNEL_REGS_SIZE) as usize,
        )
    }
}

impl BusMember<Address> for Sound {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            // What a read of the FIFO returns is not known
            0x000..=0x7FF => Some(0),
            REG_VERSION => Some(self.kind.version()),
            // The chip is always in FIFO mode
            REG_MODE => Some(1),
            REG_CONTROL | REG_FIFO_MODE => Some(self.kind.control()),
            REG_FIFO_STATUS => Some(self.fifo_status().0),
            REG_WAVETABLE_CONTROL | REG_CLOCK | REG_TEST => Some(0),
            REG_VOLUME => Some(self.volume),
            REG_PLAY_RECORD => Some(self.play_record),
            0xF00..=0xF3F if self.kind.channel_regs() => {
                let (channel, reg) = Self::channel_reg(addr);
                Some(self.channels[channel].read_reg(reg))
            }
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            // The V8 has no second FIFO
            0x000..=0x7FF => {
                if let Some(channel) = self.channels.get_mut((addr / 0x400) as usize) {
                    channel.write_fifo(val);
                }
            }
            REG_FIFO_MODE => {
                if val & 0x80 != 0 {
                    for channel in &mut self.channels {
                        channel.fifo.clear();
                    }
                }
            }
            REG_VOLUME => self.volume = val,
            REG_PLAY_RECORD => self.play_record = val,
            0xF00..=0xF3F if self.kind.channel_regs() => {
                let (channel, reg) = Self::channel_reg(addr);
                self.channels[channel].write_reg(reg, val);
            }
            // The status follows the FIFOs, and the other registers are fixed
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

        let mut props = vec![
            dbgprop_byte_bin!("FIFO status", self.fifo_status().0),
            dbgprop_bool!("IRQ", self.irq()),
            dbgprop_byte!("Volume", self.volume),
        ];
        for (n, channel) in self.channels.iter().enumerate() {
            props.push(dbgprop_udec!(
                format!("FIFO level (channel {})", n),
                channel.fifo.len()
            ));
        }
        props
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REG_FIFO_A_IRQ: Address = 0xF09;
    const REG_FIFO_B_IRQ: Address = 0xF29;

    #[test]
    fn registers() {
        let mut sound = Sound::new(SoundKind::V8);
        assert_eq!(sound.read(REG_VERSION), Some(0xE8));
        assert_eq!(sound.read(REG_MODE), Some(1));
        sound.write(REG_MODE, 0);
        assert_eq!(sound.read(REG_MODE), Some(1));
        sound.write(REG_VOLUME, 0xE0);
        assert_eq!(sound.read(REG_VOLUME), Some(0xE0));
    }

    #[test]
    fn fifo_status_and_irq() {
        let mut sound = Sound::new(SoundKind::V8);

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
        assert_eq!(sound.channels.len(), 1);
        assert_eq!(sound.channels[0].fifo.len(), FIFO_SIZE / 2);

        for _ in 0..FIFO_SIZE {
            sound.write(0x000, 0x80);
        }
        assert_eq!(sound.channels[0].fifo.len(), FIFO_SIZE);
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
        let mut sound = Sound::new(SoundKind::V8);
        sound.write(0x000, 0x12);
        sound.tick(false).unwrap();
        sound.tick(false).unwrap();
        assert_eq!(sound.channels[0].last, 0x12);
    }

    #[test]
    fn sonora_registers() {
        let mut sound = Sound::new(SoundKind::Sonora);
        assert_eq!(sound.read(REG_VERSION), Some(0xBC));
        assert_eq!(sound.read(REG_MODE), Some(1));
        assert_eq!(sound.read(REG_CONTROL), Some(0));
        assert_eq!(sound.read(REG_FIFO_A_IRQ), Some(0));
        sound.write(REG_FIFO_A_IRQ, 0x01);
        assert_eq!(sound.read(REG_FIFO_A_IRQ), Some(1));
        sound.write(0xF04, 0x81);
        assert_eq!(sound.read(0xF04), Some(0x81));
        assert_eq!(sound.read(0xF24), Some(0x00));

        // The V8 has no such register
        let mut sound = Sound::new(SoundKind::V8);
        assert_eq!(sound.read(REG_FIFO_A_IRQ), None);
    }

    #[test]
    fn sonora_fifo_status_and_irq() {
        let mut sound = Sound::new(SoundKind::Sonora);

        // Both interrupts are asserted with the FIFOs empty
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x0E));
        assert!(sound.irq());
        sound.write(REG_FIFO_A_IRQ, 0x01);
        assert!(sound.irq());
        sound.write(REG_FIFO_B_IRQ, 0x01);
        assert!(!sound.irq());
        sound.write(REG_FIFO_B_IRQ, 0x00);

        // Both FIFOs have to be half full for the interrupt to be negated
        for _ in 0..FIFO_SIZE / 2 {
            sound.write(0x000, 0x80);
        }
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x0E));
        sound.write(0x400, 0x80);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x06));
        for _ in 1..FIFO_SIZE / 2 {
            sound.write(0x400, 0x80);
        }
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x02));
        assert!(!sound.irq());

        sound.tick(false).unwrap();
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x06));
        assert!(sound.irq());

        sound.write(REG_FIFO_MODE, 0x80);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x0E));
    }

    #[test]
    fn sonora_stereo() {
        let mut sound = Sound::new(SoundKind::Sonora);
        sound.write(0x000, 0x12);
        sound.write(0x400, 0x34);
        sound.tick(false).unwrap();
        sound.tick(false).unwrap();
        assert_eq!(sound.channels[0].last, 0x12);
        assert_eq!(sound.channels[1].last, 0x34);
    }
}
