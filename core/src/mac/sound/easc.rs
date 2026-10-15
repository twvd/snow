//! Enhanced Apple Sound Chip (EASC, "Batman"), of the Quadra 700, 900 and
//! 950
//!
//! Two channels, left and right, each with its FIFO and a block of
//! registers. There is no wavetable mode. As on the V8 and Sonora, the
//! status follows the FIFO levels, a read clears nothing, and the interrupt
//! is a level, asserted for as long as a FIFO whose interrupt is enabled is
//! half full or less. It is meant for an edge-triggered input (VIA2 CB1).
//!
//! The chip outputs at 44.1 kHz. A FIFO with sample rate conversion enabled
//! is converted from the rate in its registers (rate_converter.rs),
//! otherwise it gives one sample per output sample. Samples are 16-bit from
//! there on.
//!
//! Each output side mixes both channels, each by its scale for that side.
//! The ROM sets $7F for the side a channel plays on and 0 for the other.
//!
//! A FIFO can hold CD-XA data instead of samples (cdxa.rs), decoded before
//! the sample rate conversion.
//!
//! In record mode, FIFO A is read by the CPU and fills with silence at the
//! record rate, as there is no sound input. Its interrupt is then asserted
//! while it is more than half full. Not tested on a machine yet.

use super::audiopath::AudioPath;
use super::cdxa::CdxaDecoder;
use super::channel::{Channel, channel_reg};
use super::rate_converter::RateConverter;
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
/// Volume, in bits 5-7
const REG_VOLUME: Address = 0x806;
const REG_CLOCK: Address = 0x807;
/// Play or record, and the record rate. Bit 1 (22 kHz) is set at power-up.
const REG_PLAY_RECORD: Address = 0x80A;

/// Cycles of the chip clock for each output sample, and for each recorded
/// sample at 22 kHz (22245 Hz) and 11 kHz
const CYCLES_PER_SAMPLE: u32 = 512;
const RECORD_PERIOD_22K: u32 = 1015;
const RECORD_PERIOD_11K: u32 = 2030;

/// Registers at 0x800 which are only stored
const REGS_SIZE: usize = 0x40;

/// Size of a FIFO (1K in the Enhanced ASC documentation), and the level at
/// or below which it is half full, as on the Apple Sound Chip and the
/// LC 475 ($BB). Not measured on an EASC: the V8 and the Sonora have 1022
/// and 510 (v8_sonora.rs), and the system refills 512 samples at the half
/// interrupt, which works either way as long as full is half + 512.
const FIFO_SIZE: usize = 0x400;
const FIFO_HALF: usize = FIFO_SIZE / 2;

/// Registers of a channel: sample rate conversion step, the input rate
/// divided by the output rate as a 16-bit fraction. The Quadra 700 ROM sets
/// $812F, 22254 Hz.
const CHANNEL_REG_SRC_H: usize = 0x04;
const CHANNEL_REG_SRC_L: usize = 0x05;
/// Scales of a channel for the left and right outputs, taken as fractions
/// of $80
const CHANNEL_REG_SCALE_L: usize = 0x06;
const CHANNEL_REG_SCALE_R: usize = 0x07;
/// FIFO control: bit 7 enables the sample rate conversion, bits 0-1 select
/// a CD-XA mode: 0 off, 1 8-bit, 2 4-bit, 3 2-bit samples. Writing it with
/// bit 7 clear resets the conversion, with bits 0-1 clear the decoder.
const CHANNEL_REG_FIFO_CTRL: usize = 0x08;
/// CD-XA coefficients, K1 and K0 for each filter
const CHANNEL_REG_CDXA: usize = 0x10;

bitfield! {
    /// FIFO status register
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct FifoStatus(pub Byte): Debug, FromStorage, IntoStorage, DerefStorage {
        /// FIFO A half full or less
        pub a_half: bool @ 0,
        /// FIFO A empty or full
        pub a_fullempty: bool @ 1,
        /// FIFO B half full or less
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

    src: [RateConverter; 2],
    cdxa: [CdxaDecoder; 2],

    /// Last sample of each channel, played on when its FIFO is empty
    last: [i16; 2],

    /// Chip clock cycles towards the next recorded sample
    record_clock: u32,

    /// FIFO mode on
    fifo_mode: bool,

    #[serde(with = "BigArray")]
    regs: [u8; REGS_SIZE],
}

impl Default for Easc {
    fn default() -> Self {
        Self {
            out: AudioPath::default(),
            channels: [
                Channel::new(FIFO_SIZE, FIFO_HALF),
                Channel::new(FIFO_SIZE, FIFO_HALF),
            ],
            src: Default::default(),
            cdxa: Default::default(),
            last: [0; 2],
            record_clock: 0,
            fifo_mode: false,
            regs: Self::power_up_regs(),
        }
    }
}

impl Easc {
    // The Quadra 700 ROM masks both interrupts and writes $02 to $80A itself
    // before using the chip
    fn power_up_regs() -> [u8; REGS_SIZE] {
        let mut regs = [0; REGS_SIZE];
        regs[(REG_PLAY_RECORD - 0x800) as usize] = 0x02;
        regs
    }

    pub fn reset(&mut self) {
        self.channels = [
            Channel::new(FIFO_SIZE, FIFO_HALF),
            Channel::new(FIFO_SIZE, FIFO_HALF),
        ];
        self.src = Default::default();
        self.cdxa = Default::default();
        self.last = [0; 2];
        self.record_clock = 0;
        self.fifo_mode = false;
        self.regs = Self::power_up_regs();
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
        let [a, b] = &self.channels;
        let a_irq = if self.recording() {
            !a.half()
        } else {
            a.half()
        };
        (a_irq && !a.irq_disable) || (b.half() && !b.irq_disable)
    }

    fn recording(&self) -> bool {
        self.regs[(REG_PLAY_RECORD - 0x800) as usize] & 1 != 0
    }

    /// Records a sample of silence into FIFO A when one is due
    fn record(&mut self) {
        let period = if self.regs[(REG_PLAY_RECORD - 0x800) as usize] & 2 != 0 {
            RECORD_PERIOD_22K
        } else {
            RECORD_PERIOD_11K
        };
        self.record_clock += CYCLES_PER_SAMPLE;
        if self.record_clock >= period {
            self.record_clock -= period;
            // Dropped when the FIFO is full
            self.channels[0].fifo.push(0x80);
        }
    }

    fn clear_fifos(&mut self) {
        for channel in &mut self.channels {
            channel.fifo.clear();
        }
    }

    /// Takes the sample of a channel for one output sample
    fn sample(&mut self, ch: usize) -> i16 {
        let channel = &mut self.channels[ch];
        let cdxa = &mut self.cdxa[ch];
        let sample = if channel.read_reg(CHANNEL_REG_FIFO_CTRL) & 0x80 != 0 {
            let step = u16::from_be_bytes([
                channel.read_reg(CHANNEL_REG_SRC_H),
                channel.read_reg(CHANNEL_REG_SRC_L),
            ]);
            self.src[ch].convert(step, || input(channel, cdxa))
        } else {
            input(channel, cdxa)
        };
        if let Some(sample) = sample {
            self.last[ch] = sample;
        }
        self.last[ch]
    }

    /// Mixes the samples of the channels for one output side
    fn mix(&self, a: i16, b: i16, scale_reg: usize) -> i16 {
        let [ca, cb] = &self.channels;
        let a = i32::from(a) * i32::from(ca.read_reg(scale_reg));
        let b = i32::from(b) * i32::from(cb.read_reg(scale_reg));
        ((a + b) >> 7).clamp(-0x8000, 0x7FFF) as i16
    }

    /// Plays a sample, at the sample rate
    pub fn tick(&mut self, queue_sample: bool) -> Result<()> {
        let (a, b) = if !self.fifo_mode {
            (self.last[0], self.last[1])
        } else if self.recording() {
            self.record();
            (self.last[0], self.sample(1))
        } else {
            (self.sample(0), self.sample(1))
        };
        if queue_sample {
            let l = self.mix(a, b, CHANNEL_REG_SCALE_L);
            let r = self.mix(a, b, CHANNEL_REG_SCALE_R);
            self.out.push16(l, r)?;
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

/// Takes the next sample of a channel from its FIFO, decoding CD-XA data
fn input(channel: &mut Channel, cdxa: &mut CdxaDecoder) -> Option<i16> {
    let cdxa_mode = channel.read_reg(CHANNEL_REG_FIFO_CTRL) & 3;
    if cdxa_mode == 0 {
        if channel.empty() {
            return None;
        }
        return Some(i16::from((channel.fifo.pop() ^ 0x80) as i8) << 8);
    }
    // The Quadra 700 ROM plays the boot chime as 4-bit samples (FIFO control
    // $82), at $7FFF, 22050 Hz
    let bits = 8 >> (cdxa_mode - 1);
    if channel.fifo.len() < cdxa.bytes_needed(bits) {
        return None;
    }
    let coefs = std::array::from_fn(|i| channel.read_reg(CHANNEL_REG_CDXA + i));
    Some(cdxa.decode(bits, &coefs, || channel.fifo.pop()))
}

impl BusMember<Address> for Easc {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            0x000..=0x3FF if self.fifo_mode && self.recording() => {
                Some(self.channels[0].fifo.pop())
            }
            // What a read of the FIFO returns is not known
            0x000..=0x7FF => Some(0),
            REG_VERSION => Some(VERSION),
            REG_MODE => Some(self.fifo_mode.into()),
            REG_CONTROL => Some(0),
            REG_FIFO_STATUS => Some(self.fifo_status().0),
            // Would be 44.1 kHz on the original ASC
            REG_CLOCK => Some(3),
            REG_FIFO_MODE | 0x805 | REG_VOLUME | REG_PLAY_RECORD | 0x80F => {
                Some(self.regs[(addr - 0x800) as usize])
            }
            0xF00..=0xF3F => {
                let (channel, reg) = channel_reg(addr);
                Some(self.channels[channel].read_reg(reg))
            }
            // The reserved registers and the others read 0, as $808 on every
            // ASC ASCTester has run on
            _ => Some(0),
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            // FIFO A is read-only when recording
            0x000..=0x3FF if self.recording() => (),
            0x000..=0x7FF => {
                if self.fifo_mode {
                    self.channels[(addr / 0x400) as usize].fifo.push(val);
                }
            }
            // A mode change keeps the FIFOs, as on the Sonora and a LC 475
            // (ASCTester)
            REG_MODE => self.fifo_mode = val & 1 != 0,
            REG_FIFO_MODE => {
                if val & 0x80 != 0 {
                    self.clear_fifos();
                }
                self.regs[(addr - 0x800) as usize] = val & 0x7F;
            }
            // Fixed, or following the FIFOs
            REG_VERSION | REG_CONTROL | REG_FIFO_STATUS | REG_CLOCK => (),
            REG_VOLUME => self.regs[(addr - 0x800) as usize] = val & 0xE0,
            0x805 | REG_PLAY_RECORD | 0x80F => self.regs[(addr - 0x800) as usize] = val,
            0xF00..=0xF3F => {
                let (channel, reg) = channel_reg(addr);
                // The Quadra 700 ROM writes 0 here before setting the rate
                if reg == CHANNEL_REG_FIFO_CTRL && val & 0x80 == 0 {
                    self.src[channel].reset();
                }
                if reg == CHANNEL_REG_FIFO_CTRL && val & 3 == 0 {
                    self.cdxa[channel].reset();
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
        assert_eq!(easc.read(REG_FIFO_A_IRQ), Some(0));
        assert_eq!(easc.read(REG_PLAY_RECORD), Some(0x02));
        easc.write(REG_VOLUME, 0xFF);
        assert_eq!(easc.read(REG_VOLUME), Some(0xE0));

        // Enabled at reset, a level while a FIFO is half full or less
        assert!(easc.irq());
        easc.read(REG_FIFO_STATUS);
        assert!(easc.irq());
        easc.write(REG_FIFO_A_IRQ, 1);
        easc.write(REG_FIFO_B_IRQ, 1);
        assert!(!easc.irq());
        easc.write(REG_FIFO_B_IRQ, 0);
        assert!(easc.irq());

        easc.write(REG_MODE, 1);
        fill(&mut easc, 0x400, FIFO_HALF + 1);
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x03));
        assert!(!easc.irq());
        fill(&mut easc, 0x000, FIFO_SIZE);
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x02));

        easc.tick(false).unwrap();
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x04));
        assert!(easc.irq());

        // Leaving the FIFO mode keeps the FIFOs
        easc.write(REG_MODE, 0);
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x04));
        easc.write(REG_FIFO_MODE, 0x80);
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x0F));

        // Reserved registers read 0
        easc.write(0x808, 0x55);
        assert_eq!(easc.read(0x808), Some(0));
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

        // The rate the ROM sets, 22254 Hz
        easc.write(0xF04, 0x81);
        easc.write(0xF05, 0x2F);
        easc.write(0xF08, 0x80);
        for _ in 0..100 {
            easc.tick(false).unwrap();
        }
        assert_eq!(easc.channels[0].fifo.len(), FIFO_SIZE - 150);

        // Turning the conversion off and on again starts it over
        easc.write(0xF08, 0);
        easc.write(0xF08, 0x80);
        easc.tick(false).unwrap();
        assert_eq!(easc.channels[0].fifo.len(), FIFO_SIZE - 150);
        easc.tick(false).unwrap();
        assert_eq!(easc.channels[0].fifo.len(), FIFO_SIZE - 151);
    }

    #[test]
    fn stereo() {
        let mut easc = Easc::default();
        easc.write(REG_MODE, 1);
        easc.write(0x000, 0x12);
        easc.write(0x400, 0x34);
        easc.tick(false).unwrap();
        easc.tick(false).unwrap();
        assert_eq!(easc.last, [-0x6E00, -0x4C00]);
    }

    #[test]
    fn cdxa() {
        let mut easc = Easc::default();
        easc.write(REG_MODE, 1);
        // Filter 0, range 0, then two 4-bit samples
        easc.write(0xF08, 0x02);
        easc.write(0x000, 0x00);
        easc.write(0x000, 0x71);
        easc.tick(false).unwrap();
        assert_eq!(easc.last[0], 0x1000);
        easc.tick(false).unwrap();
        assert_eq!(easc.last[0], 0x7000);

        // An empty FIFO repeats the last sample decoded
        easc.tick(false).unwrap();
        assert_eq!(easc.last[0], 0x7000);

        // A byte short of the next sample: the sample is not decoded yet
        easc.write(0xF08, 0x00);
        easc.write(0xF08, 0x02);
        easc.write(0x000, 0x00);
        easc.tick(false).unwrap();
        assert_eq!(easc.channels[0].fifo.len(), 1);
        easc.write(0x000, 0x0F);
        easc.tick(false).unwrap();
        assert_eq!(easc.last[0], -0x1000);
    }

    #[test]
    fn record() {
        let mut easc = Easc::default();
        easc.write(REG_MODE, 1);
        easc.write(REG_FIFO_B_IRQ, 1);
        easc.write(REG_PLAY_RECORD, 0x03);
        assert!(!easc.irq());

        // 22245 Hz, CPU writes ignored
        for _ in 0..1000 {
            easc.tick(false).unwrap();
        }
        easc.write(0x000, 0x12);
        assert_eq!(easc.channels[0].fifo.len(), 504);
        assert!(!easc.irq());

        // More than half full: interrupt, until the CPU reads 512 samples
        for _ in 0..20 {
            easc.tick(false).unwrap();
        }
        assert_eq!(easc.read(REG_FIFO_STATUS), Some(0x0C));
        assert!(easc.irq());
        for _ in 0..512 {
            assert_eq!(easc.read(0x000), Some(0x80));
        }
        assert_eq!(easc.channels[0].fifo.len(), 2);
        assert!(!easc.irq());

        // 11 kHz, half the samples
        easc.write(REG_FIFO_MODE, 0x80);
        easc.write(REG_PLAY_RECORD, 0x01);
        for _ in 0..2030 {
            easc.tick(false).unwrap();
        }
        assert_eq!(easc.channels[0].fifo.len(), 512);

        // Samples are dropped when the FIFO is full
        for _ in 0..4000 {
            easc.tick(false).unwrap();
        }
        assert_eq!(easc.channels[0].fifo.len(), FIFO_SIZE);
    }

    #[test]
    fn mix() {
        let mut easc = Easc::default();

        // As the ROM sets them, A on the left and B on the right
        easc.write(0xF06, 0x7F);
        easc.write(0xF07, 0x00);
        easc.write(0xF26, 0x00);
        easc.write(0xF27, 0x7F);
        assert_eq!(easc.mix(0x4000, -0x4000, CHANNEL_REG_SCALE_L), 0x3F80);
        assert_eq!(easc.mix(0x4000, -0x4000, CHANNEL_REG_SCALE_R), -0x3F80);

        // Both on both sides, clipped
        easc.write(0xF06, 0x80);
        easc.write(0xF26, 0x80);
        assert_eq!(easc.mix(0x4000, 0x1000, CHANNEL_REG_SCALE_L), 0x5000);
        assert_eq!(easc.mix(0x7000, 0x7000, CHANNEL_REG_SCALE_L), 0x7FFF);
        assert_eq!(easc.mix(-0x8000, -0x7000, CHANNEL_REG_SCALE_L), -0x8000);
    }
}
