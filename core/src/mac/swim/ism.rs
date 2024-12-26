use std::mem;

use anyhow::Result;
use log::*;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::bus::Address;
use crate::mac::swim::SwimMode;
use crate::types::Byte;

use super::Swim;

#[derive(Debug)]
enum IsmRegister {
    Data,
    Correction,
    Mark,
    Crc,
    IwmConfig,
    Parameter,
    Phase,
    Setup,
    ModeZero,
    ModeOne,
    Status,
    Error,
    Handshake,
}

bitfield! {
    /// ISM mode/status register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IsmStatus(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        pub clear_fifo: bool @ 0,
        pub drive1_enable: bool @ 1,
        pub drive2_enable: bool @ 2,
        pub action: bool @ 3,
        pub write: bool @ 4,
        pub hdsel: bool @ 5,
        pub ism: bool @ 6,
        pub motor: bool @ 7,
    }
}

bitfield! {
    /// ISM error register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IsmError(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        pub underrun: bool @ 0,
        pub mark_from_dr: bool @ 1,
        pub overrun: bool @ 2,
        pub correction_err: bool @ 3,
        pub tr_too_narrow: bool @ 4,
        pub tr_too_wide: bool @ 5,
        pub tr_unresolved: bool @ 6,
    }
}

bitfield! {
    /// ISM handshake register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IsmHandshake(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        pub mark: bool @ 0,
        pub crc_error: bool @ 1,
        pub rddata: bool @ 2,
        pub sense: bool @ 3,
        pub motoron: bool @ 4,
        pub error: bool @ 5,
        pub fifo_two: bool @ 6,
        pub fifo_one: bool @ 7,
    }
}

bitfield! {
    /// ISM setup register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IsmSetup(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// HEADSEL/Q3
        pub hdsel: bool @ 0,
        /// 3.5SEL (inverted)
        pub sel35: bool @ 1,
        pub gcr: bool @ 2,
        pub fclk_div2: bool @ 3,
        pub ecm_enable: bool @ 4,
        /// If 0, RDDATA/WRDATA is transitions, if 1, it is pulses
        /// 'IBM/Apple drive'
        pub pulses: bool @ 5,
        /// Disable Trans-Space Machine
        pub tsm_disable: bool @ 6,
        pub motoron_tmr_enable: bool @ 7,
    }
}

impl IsmRegister {
    pub fn from(addr: Address, action: bool, write: bool) -> Option<Self> {
        match (addr & 0b111, action, write) {
            (0b000, _, _) => Some(Self::Data),
            //(0b000, false, false) => Some(Self::Correction),
            (0b001, _, _) => Some(Self::Mark),
            (0b010, true, true) => Some(Self::Crc),
            (0b010, false, true) => Some(Self::IwmConfig),
            (0b011, _, _) => Some(Self::Parameter),
            (0b100, _, _) => Some(Self::Phase),
            (0b101, _, _) => Some(Self::Setup),
            (0b110, _, true) => Some(Self::ModeZero),
            (0b111, _, true) => Some(Self::ModeOne),
            (0b110, _, false) => Some(Self::Status),
            (0b010, _, false) => Some(Self::Error),
            (0b111, _, false) => Some(Self::Handshake),
            _ => None,
        }
    }
}

pub(super) enum IsmFifoEntry {
    Marker(u8),
    Data(u8),
}

impl Swim {
    fn ism_fifo_pop(&mut self, expect_marker: bool) -> Option<(bool, u8)> {
        match self.ism_fifo.pop_front()? {
            IsmFifoEntry::Data(d) => Some((false, d)),
            IsmFifoEntry::Marker(d) => Some((!expect_marker, d)),
        }
    }

    /// A memory-mapped I/O address was read
    pub(super) fn ism_read(&mut self, addr: Address) -> Option<Byte> {
        let offset = (addr - 0xDFE1FF) / 512;

        if let Some(reg) = IsmRegister::from(offset, false, false) {
            let result = match reg {
                IsmRegister::Data | IsmRegister::Mark => {
                    if let Some((e, v)) = self.ism_fifo_pop(matches!(reg, IsmRegister::Mark)) {
                        if e {
                            self.ism_error.set_mark_from_dr(true);
                        }
                        Some(v)
                    } else {
                        warn!("ISM FIFO overrun (CPU reading too fast)");
                        self.ism_error.set_overrun(true);
                        Some(0xFF)
                    }
                }
                IsmRegister::Error => Some(mem::replace(&mut self.ism_error, IsmError(0)).0),
                IsmRegister::Status => Some(self.ism_mode.0),
                IsmRegister::Phase => Some(self.ism_read_phases()),
                IsmRegister::Handshake => Some(
                    IsmHandshake(0)
                        .with_sense(
                            !self.get_selected_drive().is_present()
                                || self
                                    .get_selected_drive()
                                    .read_sense(self.get_selected_drive_reg_u8()),
                        )
                        .with_motoron(self.get_selected_drive().motor)
                        .with_error(self.ism_error.0 != 0)
                        .with_fifo_two(
                            // TODO write mode
                            self.ism_fifo.len() >= 2,
                        )
                        .with_fifo_one(
                            // TODO write mode
                            !self.ism_fifo.is_empty(),
                        )
                        .0,
                ),
                IsmRegister::Parameter => {
                    let value = self.ism_params[self.ism_param_idx];
                    self.ism_param_idx = (self.ism_param_idx + 1) % self.ism_params.len();
                    Some(value)
                }
                IsmRegister::Setup => Some(self.ism_setup.0),
                _ => Some(0),
            };
            //debug!(
            //    "ISM read {:06X} {:02X} {:?}: {:02X}",
            //    addr,
            //    offset,
            //    reg,
            //    result.unwrap()
            //);
            result
        } else {
            error!("Unknown ISM register read {:04X}", offset);
            Some(0)
        }
    }

    pub(super) fn ism_write(&mut self, addr: Address, value: Byte) {
        let offset = (addr - 0xDFE1FF) / 512;

        if let Some(reg) = IsmRegister::from(offset, false, true) {
            //debug!(
            //    "ISM write {:06X} {:02X} {:?}: {:02X}",
            //    addr, offset, reg, value
            //);
            match reg {
                IsmRegister::Data | IsmRegister::Mark => (),
                IsmRegister::Phase => self.ism_write_phases(value),
                IsmRegister::ModeZero => {
                    self.ism_param_idx = 0;

                    let clr = IsmStatus(value);
                    if clr.clear_fifo() && self.ism_mode.clear_fifo() {
                        self.ism_fifo.clear();
                    }

                    self.ism_mode.0 &= !value;
                    if !self.ism_mode.ism() {
                        debug!("IWM mode");
                        self.mode = SwimMode::Iwm;
                    }
                }
                IsmRegister::ModeOne => {
                    self.ism_mode.0 |= value;
                }
                IsmRegister::Parameter => {
                    self.ism_params[self.ism_param_idx] = value;
                    self.ism_param_idx = (self.ism_param_idx + 1) % self.ism_params.len();
                }
                IsmRegister::Setup => {
                    self.ism_setup.0 = value;
                }
                _ => (),
            }
        } else {
            error!("Unknown ISM register write {:04X}", offset);
        }
    }

    fn ism_read_phases(&self) -> u8 {
        self.ism_phase_mask
    }

    fn ism_write_phases(&mut self, phases: u8) {
        self.ism_phase_mask = phases;
        if self.ism_phase_mask & (1 << 4) == 0 {
            self.ca0 = phases & (1 << 0) != 0;
        }
        if self.ism_phase_mask & (1 << 5) == 0 {
            self.ca1 = phases & (1 << 1) != 0;
        }
        if self.ism_phase_mask & (1 << 6) == 0 {
            self.ca2 = phases & (1 << 2) != 0;
        }
        if self.ism_phase_mask & (1 << 7) == 0 {
            self.lstrb = phases & (1 << 3) != 0;
        }
    }

    pub(super) fn ism_tick(&mut self, _ticks: usize) -> Result<()> {
        // This is only called when the drive is active and running
        if self.cycles % self.get_selected_drive().get_ticks_per_bit() * 16 != 0 {
            return Ok(());
        }

        if self.cycles % (16 * 16) == 0 {
            let head = self.get_active_head();
            let mut mfm = 0;
            let mut data = 0;

            for bit_num in 0..16 {
                let bit = self.get_selected_drive_mut().next_bit(head);

                if bit {
                    mfm |= 1 << (15 - bit_num);
                }
                if bit_num % 2 == 1 && bit {
                    data |= 1 << (7 - (bit_num / 2));
                }
            }

            if mfm == 0b10001001_0001001u16 {
                //debug!("Marker {:02X}", data);
                self.ism_fifo.push_back(IsmFifoEntry::Marker(data));
            } else {
                self.ism_fifo.push_back(IsmFifoEntry::Data(data));
            }
            if self.ism_fifo.len() > 2 {
                //warn!("ISM read underrun (CPU not reading fast enough)");
                self.ism_error.set_underrun(true);
                self.ism_fifo.pop_front();
            }
        }

        Ok(())
    }
}
