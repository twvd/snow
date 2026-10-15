//! Sound of the V8 and Sonora gate arrays
//!
//! The V8 has a cut down Apple Sound Chip: one mono FIFO, no wavetable mode,
//! and an interrupt which is a level, asserted for as long as the FIFO is
//! half full or less, so also when no sound is played. The system masks
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

use super::audiopath::AudioPath;
use super::fifo::Fifo;
use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::renderer::AudioSink;
use crate::tickable::Ticks;
use crate::types::Byte;

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

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

/// Size of a FIFO, and the level at or below which it is half full. Both
/// were measured with ASCTester on a LC II and a LC III: the half condition
/// sets 510 samples above empty and full is 1022, where the Apple Sound Chip
/// has 512 and 1024. The system refills 512 samples at the half interrupt,
/// which works either way as long as full is half + 512.
const FIFO_SIZE: usize = 1022;
const FIFO_HALF: usize = 510;

/// Registers of the channels of the Sonora, a block for each
const CHANNEL_REGS: Address = 0xF00;
const CHANNEL_REGS_SIZE: Address = 0x20;

/// Register of a channel which disables its interrupt
const CHANNEL_REG_IRQ: usize = 0x09;

bitfield! {
    /// FIFO status register
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct FifoStatus(pub Byte): Debug, FromStorage, IntoStorage, DerefStorage {
        /// FIFO half full or less (V8)
        pub half: bool @ 0,
        /// FIFO empty or full (V8), not recording (Sonora)
        pub fullempty: bool @ 1,
        /// FIFOs half full or less (Sonora)
        pub half_b: bool @ 2,
        /// FIFOs empty or full (Sonora)
        pub fullempty_b: bool @ 3,
    }
}

/// Gate array which the sound is part of
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum V8Variant {
    V8,
    Sonora,
}

impl V8Variant {
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
    fifo: Fifo,

    /// Level at or below which the FIFO is half full
    half: usize,

    /// Interrupt of the channel disabled
    irq_disable: bool,

    /// Registers which are only stored: FIFO pointers, sample rate, volume,
    /// FIFO control, and coefficients for CD-XA sound
    regs: [u8; CHANNEL_REGS_SIZE as usize],
}

impl Channel {
    fn new(size: usize, half: usize) -> Self {
        Self {
            fifo: Fifo::new(0x80, size),
            half,
            irq_disable: false,
            regs: [0; CHANNEL_REGS_SIZE as usize],
        }
    }

    fn half(&self) -> bool {
        self.fifo.len() <= self.half
    }

    fn empty(&self) -> bool {
        self.fifo.is_empty()
    }

    fn full(&self) -> bool {
        self.fifo.len() == self.fifo.size()
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
pub struct V8Sound {
    kind: V8Variant,

    out: AudioPath,

    /// One channel in the V8, left and right in the Sonora
    channels: Vec<Channel>,

    /// Volume register. The volume is not applied.
    volume: u8,

    /// Play and record register, for the sound input
    play_record: u8,
}

impl V8Sound {
    pub fn new(kind: V8Variant) -> Self {
        Self {
            kind,
            out: AudioPath::default(),
            channels: (0..kind.channels())
                .map(|_| Channel::new(FIFO_SIZE, FIFO_HALF))
                .collect(),
            volume: 0,
            play_record: 0,
        }
    }

    pub fn reset(&mut self) {
        for channel in &mut self.channels {
            *channel = Channel::new(FIFO_SIZE, FIFO_HALF);
        }
        self.volume = 0;
        self.play_record = 0;
        self.out.reset();
    }

    fn fifo_status(&self) -> FifoStatus {
        // The status follows the fuller FIFO
        let ch = &self.channels;
        let half = ch.iter().all(Channel::half);
        let fullempty = ch.iter().any(Channel::full) || ch.iter().all(Channel::empty);
        match self.kind {
            V8Variant::V8 => FifoStatus(0).with_half(half).with_fullempty(fullempty),
            // The Sonora has the bits of the V8 in bits 2 and 3, and bit 1 is
            // set for as long as it is not recording. ASCTester on a LC III
            // reads $0E empty, $0A full, $02 above half and $06 half full or
            // less, whether FIFO A, FIFO B or both are filled. A LC 475
            // reads the same.
            V8Variant::Sonora => FifoStatus(0)
                .with_fullempty(self.play_record & 1 == 0)
                .with_half_b(half)
                .with_fullempty_b(fullempty),
        }
    }

    /// Level of the interrupt output
    pub fn irq(&self) -> bool {
        let status = self.fifo_status();
        match self.kind {
            V8Variant::V8 => status.half(),
            // As for the status: the interrupt of the first channel follows
            // bit 1, that of the second the level of both FIFOs
            V8Variant::Sonora => {
                (status.fullempty() && !self.channels[0].irq_disable)
                    || (status.half_b() && !self.channels[1].irq_disable)
            }
        }
    }

    /// Plays a sample, at the sample rate
    pub fn tick(&mut self, queue_sample: bool) -> Result<()> {
        let mut samples = [0; 2];
        for (sample, channel) in samples.iter_mut().zip(&mut self.channels) {
            *sample = channel.fifo.pop();
        }
        if queue_sample {
            // The only channel of the V8 goes to both sides
            let right = samples[self.channels.len() - 1];
            self.out.push(samples[0], right)?;
        }
        Ok(())
    }

    /// Time between two samples, in cycles of C16M (15.6672 MHz): 22254.5
    /// Hz, 35.2 VIA clock cycles, as ASCTester measures on a LC II and a
    /// LC III
    pub const fn sample_period(&self) -> Ticks {
        704
    }

    pub fn set_sink(&mut self, sink: Box<dyn AudioSink>) {
        self.out.set_sink(sink);
    }

    pub fn after_deserialize(&mut self) {
        self.out.after_deserialize();
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

impl BusMember<Address> for V8Sound {
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
            // ASCTester on a LC II and a LC III reads the other registers
            // as 0, as $808, and $F09 and $F29 on the V8
            _ => Some(0),
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            // The V8 has no second FIFO
            0x000..=0x7FF => {
                if let Some(channel) = self.channels.get_mut((addr / 0x400) as usize) {
                    channel.fifo.push(val);
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

impl Debuggable for V8Sound {
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
        let mut sound = V8Sound::new(V8Variant::V8);
        assert_eq!(sound.read(REG_VERSION), Some(0xE8));
        assert_eq!(sound.read(REG_MODE), Some(1));
        sound.write(REG_MODE, 0);
        assert_eq!(sound.read(REG_MODE), Some(1));
        sound.write(REG_VOLUME, 0xE0);
        assert_eq!(sound.read(REG_VOLUME), Some(0xE0));
    }

    #[test]
    fn fifo_status_and_irq() {
        let mut sound = V8Sound::new(V8Variant::V8);

        // Empty: the interrupt stays asserted, a read does not clear it
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x03));
        assert!(sound.irq());
        sound.write(REG_FIFO_STATUS, 0xFF);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x03));

        for _ in 0..FIFO_HALF {
            sound.write(0x000, 0x80);
        }
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x01));
        sound.write(0x000, 0x80);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x00));
        assert!(!sound.irq());

        // The second FIFO does not exist
        sound.write(0x400, 0x80);
        assert_eq!(sound.channels.len(), 1);
        assert_eq!(sound.channels[0].fifo.len(), FIFO_HALF + 1);

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
        let mut sound = V8Sound::new(V8Variant::V8);
        sound.write(0x000, 0x12);
        sound.tick(false).unwrap();
        sound.tick(false).unwrap();
        assert_eq!(sound.channels[0].fifo.pop(), 0x12);
    }

    #[test]
    fn sonora_registers() {
        let mut sound = V8Sound::new(V8Variant::Sonora);
        assert_eq!(sound.read(REG_VERSION), Some(0xBC));
        assert_eq!(sound.read(REG_MODE), Some(1));
        assert_eq!(sound.read(REG_CONTROL), Some(0));
        assert_eq!(sound.read(REG_FIFO_A_IRQ), Some(0));
        sound.write(REG_FIFO_A_IRQ, 0x01);
        assert_eq!(sound.read(REG_FIFO_A_IRQ), Some(1));
        sound.write(0xF04, 0x81);
        assert_eq!(sound.read(0xF04), Some(0x81));
        assert_eq!(sound.read(0xF24), Some(0x00));

        // The V8 has no such register, and it reads 0 as the others
        let mut sound = V8Sound::new(V8Variant::V8);
        assert_eq!(sound.read(REG_FIFO_A_IRQ), Some(0));
        assert_eq!(sound.read(0x808), Some(0));
    }

    #[test]
    fn sonora_fifo_status_and_irq() {
        let mut sound = V8Sound::new(V8Variant::Sonora);

        // Both interrupts are asserted with the FIFOs empty
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x0E));
        assert!(sound.irq());
        sound.write(REG_FIFO_A_IRQ, 0x01);
        assert!(sound.irq());
        sound.write(REG_FIFO_B_IRQ, 0x01);
        assert!(!sound.irq());
        sound.write(REG_FIFO_B_IRQ, 0x00);

        // The status follows the fuller FIFO, whichever it is
        sound.write(0x400, 0x80);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x06));
        for _ in 0..FIFO_HALF {
            sound.write(0x000, 0x80);
        }
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x06));
        sound.write(0x000, 0x80);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x02));
        assert!(!sound.irq());

        sound.tick(false).unwrap();
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x06));
        assert!(sound.irq());

        for _ in 0..FIFO_SIZE {
            sound.write(0x400, 0x80);
        }
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x0A));

        sound.write(REG_FIFO_MODE, 0x80);
        assert_eq!(sound.read(REG_FIFO_STATUS), Some(0x0E));
    }

    #[test]
    fn sonora_stereo() {
        let mut sound = V8Sound::new(V8Variant::Sonora);
        sound.write(0x000, 0x12);
        sound.write(0x400, 0x34);
        sound.tick(false).unwrap();
        sound.tick(false).unwrap();
        assert_eq!(sound.channels[0].fifo.pop(), 0x12);
        assert_eq!(sound.channels[1].fifo.pop(), 0x34);
    }
}
