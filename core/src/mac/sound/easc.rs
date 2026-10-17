//! Enhanced Apple Sound Chip (EASC, "Batman"), of the Quadra 700, 900 and
//! 950
//!
//! Two channels, left and right, each with its FIFO and a block of
//! registers. There is no wavetable mode. As on the V8 and Sonora, the
//! status follows the FIFO levels, a read clears nothing, and the interrupt
//! is a level, asserted for as long as a FIFO whose interrupt is enabled is
//! less than half full. It is meant for an edge-triggered input (VIA2 CB1).
//!
//! The chip outputs at 44.1 kHz. A FIFO with sample rate conversion enabled
//! is drained at the rate in its registers, otherwise one sample per output
//! sample.
//!
//! CD-XA decoding, the left and right scales of the channels and sound input
//! are not implemented. Not tested on a machine yet.

use super::audiopath::AudioPath;
use super::channel::{Channel, channel_reg};
use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::renderer::AudioSink;
use crate::tickable::Ticks;
use crate::types::Byte;

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

const VERSION: u8 = 0xB0;

const REG_VERSION: Address = 0x800;
const REG_MODE: Address = 0x801;
const REG_CONTROL: Address = 0x802;
const REG_FIFO_MODE: Address = 0x803;
const REG_FIFO_STATUS: Address = 0x804;
const REG_CLOCK: Address = 0x807;

/// Registers at 0x800 which are only stored
const REGS_SIZE: usize = 0x40;

/// Registers of a channel: sample rate conversion step, 16.16 minus 1
const CHANNEL_REG_SRC_H: usize = 0x04;
const CHANNEL_REG_SRC_L: usize = 0x05;
/// FIFO control: bit 7 enables the sample rate conversion, bits 0-1 select
/// a CD-XA mode
const CHANNEL_REG_FIFO_CTRL: usize = 0x08;

bitfield! {
    /// FIFO status register
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct FifoStatus(pub Byte): Debug, FromStorage, IntoStorage, DerefStorage {
        /// FIFO A less than half full
        pub a_half: bool @ 0,
        /// FIFO A empty or full
        pub a_fullempty: bool @ 1,
        /// FIFO B less than half full
        pub b_half: bool @ 2,
        /// FIFO B empty or full
        pub b_fullempty: bool @ 3,
    }
}

#[derive(Serialize, Deserialize)]
pub struct Easc {
    out: AudioPath,

    /// Left and right
    channels: [Channel; 2],

    /// Sample rate conversion accumulators, 16.16
    src: [u32; 2],

    /// FIFO mode on
    fifo_mode: bool,

    #[serde(with = "BigArray")]
    regs: [u8; REGS_SIZE],
}

impl Default for Easc {
    fn default() -> Self {
        Self {
            out: AudioPath::default(),
            channels: [Self::new_channel(), Self::new_channel()],
            src: [0; 2],
            fifo_mode: false,
            regs: [0; REGS_SIZE],
        }
    }
}

impl Easc {
    /// The interrupts of the channels are disabled at reset
    fn new_channel() -> Channel {
        let mut channel = Channel::new();
        channel.irq_disable = true;
        channel
    }

    pub fn reset(&mut self) {
        self.channels = [Self::new_channel(), Self::new_channel()];
        self.src = [0; 2];
        self.fifo_mode = false;
        self.regs = [0; REGS_SIZE];
        self.out.reset();
    }

    fn fifo_status(&self) -> FifoStatus {
        let [a, b] = &self.channels;
        FifoStatus(0)
            .with_a_half(a.half())
            .with_a_fullempty(a.empty() || a.full())
            .with_b_half(b.half())
            .with_b_fullempty(b.empty() || b.full())
    }

    /// Level of the interrupt output
    pub fn irq(&self) -> bool {
        self.channels.iter().any(|c| c.half() && !c.irq_disable)
    }

    fn clear_fifos(&mut self) {
        for channel in &mut self.channels {
            channel.fifo.clear();
        }
    }

    /// Takes the sample of a channel for one output sample
    fn sample(&mut self, ch: usize) -> u8 {
        let channel = &mut self.channels[ch];
        let samples = if channel.read_reg(CHANNEL_REG_FIFO_CTRL) & 0x80 != 0 {
            let step = u32::from(channel.read_reg(CHANNEL_REG_SRC_H)) << 8
                | u32::from(channel.read_reg(CHANNEL_REG_SRC_L));
            self.src[ch] += step + 1;
            let samples = self.src[ch] >> 16;
            self.src[ch] &= 0xFFFF;
            samples
        } else {
            1
        };
        // Without a sample to take, the last one goes on
        let mut sample = channel.fifo.last();
        for _ in 0..samples {
            sample = channel.fifo.pop();
        }
        sample
    }

    /// Plays a sample, at the sample rate
    pub fn tick(&mut self, queue_sample: bool) -> Result<()> {
        let (l, r) = if self.fifo_mode {
            (self.sample(0), self.sample(1))
        } else {
            (self.channels[0].fifo.last(), self.channels[1].fifo.last())
        };
        if queue_sample {
            self.out.push(l, r)?;
        }
        Ok(())
    }

    /// The audio stream is to be opened at this rate
    pub const fn sample_rate(&self) -> Ticks {
        44100
    }

    pub fn set_sink(&mut self, sink: Box<dyn AudioSink>) {
        self.out.set_sink(sink);
    }

    pub fn after_deserialize(&mut self) {
        self.out.after_deserialize();
    }
}

impl BusMember<Address> for Easc {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            // What a read of the FIFO returns is not known
            0x000..=0x7FF => Some(0),
            REG_VERSION => Some(VERSION),
            REG_MODE => Some(self.fifo_mode.into()),
            REG_CONTROL => Some(0),
            REG_FIFO_STATUS => Some(self.fifo_status().0),
            // Would be 44.1 kHz on the original ASC
            REG_CLOCK => Some(3),
            REG_FIFO_MODE | 0x805..=0x806 | 0x808..=0x83F => {
                Some(self.regs[(addr - 0x800) as usize])
            }
            0xF00..=0xF3F => {
                let (channel, reg) = channel_reg(addr);
                Some(self.channels[channel].read_reg(reg))
            }
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            0x000..=0x7FF => {
                if self.fifo_mode {
                    self.channels[(addr / 0x400) as usize].fifo.push(val);
                }
            }
            REG_MODE => {
                let fifo_mode = val & 1 != 0;
                if fifo_mode != self.fifo_mode {
                    self.clear_fifos();
                }
                self.fifo_mode = fifo_mode;
            }
            REG_FIFO_MODE => {
                if val & 0x80 != 0 {
                    self.clear_fifos();
                }
                self.regs[(addr - 0x800) as usize] = val & 0x7F;
            }
            // Fixed, or following the FIFOs
            REG_VERSION | REG_CONTROL | REG_FIFO_STATUS | REG_CLOCK => (),
            0x805..=0x806 | 0x808..=0x83F => self.regs[(addr - 0x800) as usize] = val,
            0xF00..=0xF3F => {
                let (channel, reg) = channel_reg(addr);
                if reg == CHANNEL_REG_FIFO_CTRL && val & 3 != 0 {
                    log::warn!(
                        "EASC channel {}: CD-XA mode {} not implemented",
                        channel,
                        val & 3
                    );
                }
                self.channels[channel].write_reg(reg, val);
            }
            _ => return None,
        }
        Some(())
    }
}

impl Debuggable for Easc {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_byte_bin, dbgprop_udec};

        vec![
            dbgprop_bool!("FIFO mode", self.fifo_mode),
            dbgprop_byte_bin!("FIFO status", self.fifo_status().0),
            dbgprop_bool!("IRQ", self.irq()),
            dbgprop_udec!("FIFO A level", self.channels[0].fifo.len()),
            dbgprop_udec!("FIFO B level", self.channels[1].fifo.len()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::sound::fifo::FIFO_SIZE;

    const REG_FIFO_A_IRQ: Address = 0xF09;
    const REG_FIFO_B_IRQ: Address = 0xF29;

    fn fill(easc: &mut Easc, addr: Address, samples: usize) {
        for _ in 0..samples {
            easc.write(addr, 0x80);
        }
    }

    #[test]
    fn status_and_irq() {
        let mut easc = Easc::default();
        assert_eq!(easc.read(REG_VERSION), Some(0xB0));
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x0F));
        assert_eq!(easc.read(REG_FIFO_A_IRQ), Some(1));

        // Disabled at reset, then a level while a FIFO is less than half full
        assert!(!easc.irq());
        easc.write(REG_FIFO_B_IRQ, 0);
        assert!(easc.irq());
        easc.read(REG_FIFO_STATUS);
        assert!(easc.irq());

        easc.write(REG_MODE, 1);
        fill(&mut easc, 0x400, FIFO_SIZE / 2);
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x03));
        assert!(!easc.irq());
        fill(&mut easc, 0x000, FIFO_SIZE);
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x02));

        easc.tick(false).unwrap();
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x04));
        assert!(easc.irq());

        // Leaving the FIFO mode clears the FIFOs
        easc.write(REG_MODE, 0);
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x0F));
    }

    #[test]
    fn sample_rate_conversion() {
        let mut easc = Easc::default();
        easc.write(REG_MODE, 1);
        fill(&mut easc, 0x000, FIFO_SIZE);

        // One sample per output sample without the conversion
        for _ in 0..100 {
            easc.tick(false).unwrap();
        }
        assert_eq!(easc.channels[0].fifo.len(), FIFO_SIZE - 100);

        // The rate the ROM sets, 22254.5 Hz
        easc.write(0xF04, 0x81);
        easc.write(0xF05, 0x2F);
        easc.write(0xF08, 0x80);
        for _ in 0..100 {
            easc.tick(false).unwrap();
        }
        assert_eq!(easc.channels[0].fifo.len(), FIFO_SIZE - 150);
    }

    #[test]
    fn stereo() {
        let mut easc = Easc::default();
        easc.write(REG_MODE, 1);
        easc.write(0x000, 0x12);
        easc.write(0x400, 0x34);
        easc.tick(false).unwrap();
        easc.tick(false).unwrap();
        assert_eq!(easc.channels[0].fifo.last(), 0x12);
        assert_eq!(easc.channels[1].fifo.last(), 0x34);
    }
}
