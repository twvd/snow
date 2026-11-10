//! A channel of the cut down Apple Sound Chips: a FIFO and, except on the
//! V8, a block of registers. Shared by the V8 and Sonora (v8_sonora.rs) and
//! the EASC (easc.rs).

use super::fifo::Fifo;
use crate::bus::Address;
use crate::types::Byte;

use serde::{Deserialize, Serialize};

/// Registers of the channels, a block for each
const CHANNEL_REGS: Address = 0xF00;
const CHANNEL_REGS_SIZE: Address = 0x20;

/// Scales of a channel for the left and right outputs, taken as fractions
/// of $80
pub const CHANNEL_REG_SCALE_L: usize = 0x06;
pub const CHANNEL_REG_SCALE_R: usize = 0x07;

/// Register of a channel which disables its interrupt
const CHANNEL_REG_IRQ: usize = 0x09;

#[derive(Serialize, Deserialize)]
pub struct Channel {
    pub fifo: Fifo,

    /// Level at or below which the FIFO is half full
    half: usize,

    /// Interrupt of the channel disabled
    pub irq_disable: bool,

    /// Registers which are only stored: FIFO pointers, sample rate, volume,
    /// FIFO control, and coefficients for CD-XA sound
    regs: [u8; CHANNEL_REGS_SIZE as usize],
}

impl Channel {
    pub fn new(size: usize, half: usize) -> Self {
        Self {
            fifo: Fifo::new(0x80, size),
            half,
            irq_disable: false,
            regs: [0; CHANNEL_REGS_SIZE as usize],
        }
    }

    /// Half full or less
    pub fn half(&self) -> bool {
        self.fifo.len() <= self.half
    }

    pub fn empty(&self) -> bool {
        self.fifo.is_empty()
    }

    pub fn full(&self) -> bool {
        self.fifo.len() == self.fifo.size()
    }

    pub fn read_reg(&self, reg: usize) -> Byte {
        match reg {
            CHANNEL_REG_IRQ => self.irq_disable.into(),
            _ => self.regs[reg],
        }
    }

    pub fn write_reg(&mut self, reg: usize, val: Byte) {
        match reg {
            CHANNEL_REG_IRQ => self.irq_disable = val & 1 != 0,
            _ => self.regs[reg] = val,
        }
    }
}

/// Mixes the samples of two channels for one output side, each by its scale
/// for that side, clipped
pub fn mix(a: &Channel, b: &Channel, samples: (i16, i16), scale_reg: usize) -> i16 {
    let sa = i32::from(samples.0) * i32::from(a.read_reg(scale_reg));
    let sb = i32::from(samples.1) * i32::from(b.read_reg(scale_reg));
    ((sa + sb) >> 7).clamp(-0x8000, 0x7FFF) as i16
}

/// Channel and number of the register at an address in the registers of
/// the channels
pub fn channel_reg(addr: Address) -> (usize, usize) {
    let offset = addr - CHANNEL_REGS;
    (
        (offset / CHANNEL_REGS_SIZE) as usize,
        (offset % CHANNEL_REGS_SIZE) as usize,
    )
}
