//! Integrated Wozniak Machine

use anyhow::Result;
use log::*;
use num::clamp;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use super::{Swim, SwimMode};
use crate::{bus::Address, mac::swim::drive::DriveType, types::Byte};

bitfield! {
    /// IWM handshake register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IwmHandshake(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Write buffer underrun
        /// 1 = no under-run, 0 = under-run occurred
        pub underrun: bool @ 6,

        /// Register ready for data
        /// 1 = ready, 0 = not ready
        pub ready: bool @ 7,
    }
}

impl Default for IwmHandshake {
    fn default() -> Self {
        Self(0xFF)
    }
}

bitfield! {
    /// IWM status register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IwmStatus(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Lower bits of MODE
        pub mode_low: u8 @ 0..=4,

        /// Enable active
        /// Enable means: disk locked, drive light on, drive ready
        /// Does not mean motor active
        /// Follows the 'ENABLE' I/O
        pub enable: bool @ 5,

        /// MZ (always 0)
        pub mz: bool @ 6,

        /// SENSE (current register read)
        pub sense: bool @ 7,
    }
}

bitfield! {
    /// IWM mode
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IwmMode(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Full MODE register
        pub mode: u8 @ 0..=7,

        /// Lower bits of MODE (to write to status)
        pub mode_low: u8 @ 0..=4,

        /// Latch mode (1)
        pub latch: bool @ 0,

        /// 0 = synchronous, 1 = asynchronous
        pub sync: bool @ 1,

        /// One-sec output enabled (0)
        pub onesec: bool @ 2,

        /// 0 = slow mode, 1 = fast mode
        pub fast: bool @ 3,

        /// Clock speed (0 = 7 MHz, 1 = 8 MHz)
        pub speed: bool @ 4,

        /// Test mode
        pub test: bool @ 5,

        /// MZ-Reset
        pub mzreset: bool @ 6,

        /// Reserved
        pub reserved: bool @ 7,
    }
}

impl Swim {
    /// A memory-mapped I/O address was accessed (offset from IWM base address)
    pub(super) fn iwm_access(&mut self, offset: Address) {
        match offset / 512 {
            0 => self.ca0 = false,
            1 => self.ca0 = true,
            2 => self.ca1 = false,
            3 => self.ca1 = true,
            4 => self.ca2 = false,
            5 => self.ca2 = true,
            6 => self.lstrb = false,
            7 => {
                self.lstrb = true;

                let reg = self.get_selected_drive_reg_u8();
                let cycles = self.cycles;
                self.get_selected_drive_mut().write_drive_reg(reg, cycles);
            }
            8 => {
                self.enable = false;
            }
            9 => {
                self.enable = true;
            }
            10 => self.extdrive = false,
            11 => self.extdrive = true,
            12 => self.q6 = false,
            13 => self.q6 = true,
            14 => {
                self.q7 = false;
            }
            15 => self.q7 = true,
            _ => (),
        }
    }

    /// Read on the bus
    pub(super) fn iwm_read(&mut self, addr: Address) -> Option<u8> {
        // Only the lower 8-bits of the databus are connected to IWM.
        // Assume the upper 8 bits are undefined.
        if addr & 1 == 0 {
            return None;
        }

        self.iwm_access(addr - 0xDFE1FF);

        let val = match (self.q6, self.q7) {
            (false, false) => {
                // Data register
                if !self.enable {
                    0xFF
                } else {
                    std::mem::replace(&mut self.datareg, 0)
                }
            }
            (true, false) => {
                // Read status register
                let sense = self
                    .get_selected_drive()
                    .read_sense(self.get_selected_drive_reg_u8());
                self.iwm_status.set_sense(sense);
                self.iwm_status.set_mode_low(self.iwm_mode.mode_low());
                self.iwm_status.set_enable(self.enable);

                self.iwm_status.0
            }
            (false, true) => {
                // Read handshake register
                let mut result = IwmHandshake::default();

                result.set_underrun(!(self.write_pos == 0 && self.write_buffer.is_none()));
                result.set_ready(self.write_buffer.is_none());
                result.0
            }
            _ => {
                warn!("IWM unknown read q6 = {:?} q7 = {:?}", self.q6, self.q7);
                0
            }
        };

        Some(val)
    }

    pub(super) fn iwm_write(&mut self, addr: Address, value: Byte) {
        const ISM_SWITCH_PATTERN: [u8; 4] = [0x57, 0x17, 0x57, 0x57];

        // UDS/LDS are not connected to IWM, so ignore the lower address bit here.
        self.iwm_access((addr | 1) - 0xDFE1FF);

        match (self.q6, self.q7, self.enable) {
            (true, true, false) => {
                // Write MODE
                if self.ism_available && ISM_SWITCH_PATTERN[self.ism_switch_ctr] == value {
                    self.ism_switch_ctr += 1;
                } else {
                    self.ism_switch_ctr = 0;
                }
                if self.ism_switch_ctr == ISM_SWITCH_PATTERN.len() {
                    debug!("ISM mode");
                    self.mode = SwimMode::Ism;
                    self.ism_mode.set_ism(true);
                    self.ism_switch_ctr = 0;
                    return;
                }
                if value != 0x1F {
                    warn!("Non-standard IWM mode: {:02X}", value);
                }
                self.iwm_mode.set_mode(value);
            }
            (true, true, true) => {
                if self.write_buffer.is_some() {
                    warn!("Disk write while write buffer not empty");
                }
                self.write_buffer = Some(value);
            }
            _ => (),
        }
    }

    /// Update current drive PWM signal from the sound buffer
    pub fn push_pwm(&mut self, pwm: u8) -> Result<()> {
        const VALUE_TO_LEN: [u8; 64] = [
            0, 1, 59, 2, 60, 40, 54, 3, 61, 32, 49, 41, 55, 19, 35, 4, 62, 52, 30, 33, 50, 12, 14,
            42, 56, 16, 27, 20, 36, 23, 44, 5, 63, 58, 39, 53, 31, 48, 18, 34, 51, 29, 11, 13, 15,
            26, 22, 43, 57, 38, 47, 17, 28, 10, 25, 21, 37, 46, 9, 24, 45, 8, 7, 6,
        ];

        if self.get_selected_drive().drive_type != DriveType::GCR400K {
            // Only 400K drives are PWM controlled
            return Ok(());
        }

        for drv in &mut self.drives {
            drv.pwm_avg_sum += VALUE_TO_LEN[usize::from(pwm) % VALUE_TO_LEN.len()] as i64;
            drv.pwm_avg_count += 1;
            if drv.pwm_avg_count >= 100 {
                let idx = clamp(
                    drv.pwm_avg_sum / (drv.pwm_avg_count as i64 / 10) - 11,
                    0,
                    399,
                );
                drv.pwm_dutycycle = ((idx * 100) / 419).try_into()?;
                drv.pwm_avg_sum = 0;
                drv.pwm_avg_count = 0;
            }
        }
        Ok(())
    }
}
