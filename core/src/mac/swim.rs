//! Sander-Wozniak Integrated Machine
//! A dual-purpose floppy drive controller consisting of two modes:
//!  * IWM - traditional IWM controller
//!  * ISM - Integrated Sander Machine, capable of reading GCR and MFM

use anyhow::Result;
use snow_floppy::{Floppy, FloppyImage};

use crate::bus::{Address, BusMember};
use crate::emulator::comm::FddStatus;
use crate::tickable::{Tickable, Ticks};

use super::iwm::Iwm;

#[derive(Debug, Default)]
enum SwimMode {
    #[default]
    Iwm,
    Ism,
}

/// Sander-Wozniak Integrated Machine
pub struct Swim {
    iwm: Iwm,

    mode: SwimMode,
    ism_available: bool,
}

impl Swim {
    pub fn new(double_sided: bool, drives: usize, hd: bool) -> Self {
        Self {
            iwm: Iwm::new(double_sided, drives),
            // SWIM boots in IWM mode
            mode: Default::default(),
            ism_available: hd,
        }
    }

    pub fn io_headsel(&mut self, value: bool) {
        self.iwm.sel = value;
    }

    pub fn io_drivesel(&mut self, value: bool) {
        self.iwm.intdrive = value;
    }

    pub fn push_pwm(&mut self, value: u8) -> Result<()> {
        self.iwm.push_pwm(value)
    }

    pub fn get_fdd_status(&self, drive: usize) -> FddStatus {
        FddStatus {
            present: self.iwm.drives[drive].present,
            ejected: !self.iwm.drives[drive].floppy_inserted,
            motor: self.iwm.drives[drive].motor,
            writing: self.iwm.drives[drive].motor && self.iwm.is_writing(),
            track: self.iwm.drives[drive].track,
            image_title: self.iwm.drives[drive].floppy.get_title().to_owned(),
        }
    }

    pub fn disk_insert(&mut self, drive: usize, image: FloppyImage) -> Result<()> {
        self.iwm.disk_insert(drive, image)
    }

    pub fn get_active_image(&self, drive: usize) -> &FloppyImage {
        self.iwm.get_active_image(drive)
    }
}

impl BusMember<Address> for Swim {
    fn read(&mut self, addr: Address) -> Option<u8> {
        match self.mode {
            SwimMode::Iwm => self.iwm.read(addr),
            SwimMode::Ism => todo!(),
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        match self.mode {
            SwimMode::Iwm => self.iwm.write(addr, val),
            SwimMode::Ism => todo!(),
        }
    }
}

impl Tickable for Swim {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        self.iwm.tick(ticks)
    }
}
