//! A channel of the cut down Apple Sound Chips: a FIFO and, except on the
//! V8, a block of registers. Shared by the V8 and Sonora (v8_sonora.rs) and
//! the EASC (easc.rs).

use super::fifo::{FIFO_SIZE, Fifo};
use crate::bus::Address;
use crate::types::Byte;

use serde::{Deserialize, Serialize};

/// Registers of the channels, a block for each
const CHANNEL_REGS: Address = 0xF00;
const CHANNEL_REGS_SIZE: Address = 0x20;

/// Register of a channel which disables its interrupt
const CHANNEL_REG_IRQ: usize = 0x09;

#[derive(Serialize, Deserialize)]
pub struct Channel {
    pub fifo: Fifo,

    /// Interrupt of the channel disabled
    pub irq_disable: bool,

    /// Registers which are only stored: FIFO pointers, sample rate, volume,
    /// FIFO control, and coefficients for CD-XA sound
    regs: [u8; CHANNEL_REGS_SIZE as usize],
}

impl Channel {
    pub fn new() -> Self {
        Self {
            fifo: Fifo::new(0x80),
            irq_disable: false,
            regs: [0; CHANNEL_REGS_SIZE as usize],
        }
    }

    /// Less than half full
    pub fn half(&self) -> bool {
        self.fifo.len() < FIFO_SIZE / 2
    }

    pub fn empty(&self) -> bool {
        self.fifo.is_empty()
    }

    pub fn full(&self) -> bool {
        self.fifo.len() == FIFO_SIZE
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

/// Channel and number of the register at an address in the registers of
/// the channels
pub fn channel_reg(addr: Address) -> (usize, usize) {
    let offset = addr - CHANNEL_REGS;
    (
        (offset / CHANNEL_REGS_SIZE) as usize,
        (offset % CHANNEL_REGS_SIZE) as usize,
    )
}
