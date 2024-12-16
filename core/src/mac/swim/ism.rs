use log::*;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::bus::Address;
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

impl IsmRegister {
    pub fn from(addr: Address, action: bool, write: bool) -> Option<Self> {
        match (addr & 0b111, action, write) {
            (0b000, true, _) => Some(Self::Data),
            (0b000, false, false) => Some(Self::Correction),
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

impl Swim {
    /// A memory-mapped I/O address was read
    pub(super) fn ism_read(&self, addr: Address) -> Option<Byte> {
        let offset = (addr - 0xDFE1FF) >> 8;
        if let Some(reg) = IsmRegister::from(offset, false, false) {
            debug!("ISM read {:?}", reg);
            match reg {
                IsmRegister::Status => {
                    let status = IsmStatus::from(0).with_ism(true);
                    Some(status.0)
                }
                IsmRegister::Phase => Some(self.ism_read_phases()),
                _ => Some(0),
            }
        } else {
            error!("Unknown ISM register read {:04X}", offset);
            Some(0)
        }
    }

    pub(super) fn ism_write(&mut self, addr: Address, value: Byte) {
        let offset = (addr - 0xDFE1FF) >> 8;
        if let Some(reg) = IsmRegister::from(offset, false, true) {
            debug!("ISM write {:?}: {:02X}", reg, value);
            match reg {
                IsmRegister::Phase => self.ism_write_phases(value),
                _ => (),
            }
        } else {
            error!("Unknown ISM register write {:04X}", offset);
        }
    }

    fn ism_read_phases(&self) -> u8 {
        let mut phases = self.ism_phase_mask & 0xF0;
        if self.ca0 {
            phases |= 1 << 0;
        }
        if self.ca1 {
            phases |= 1 << 1;
        }
        if self.ca2 {
            phases |= 1 << 2;
        }
        if self.ism_phase_mask & 0x80 == 0 {
            if self.lstrb {
                phases |= 1 << 3;
            }
        } else if self
            .get_selected_drive()
            .read_sense(self.get_selected_drive_reg_u8())
        {
            phases |= 1 << 3;
        }

        phases
    }

    fn ism_write_phases(&mut self, phases: u8) {
        self.ism_phase_mask = phases & 0xF0;
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
}
