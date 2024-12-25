//! Sander-Wozniak Integrated Machine
//!
//! Floppy drive controller consisting of two different controllers:
//! Integrated Wozniak Machine, Integrated Sander Machine.

pub mod drive;
pub mod ism;
pub mod iwm;

use std::collections::VecDeque;

use anyhow::{bail, Result};
use ism::{IsmError, IsmSetup, IsmStatus};

use drive::{DriveType, FloppyDrive};
use iwm::{IwmMode, IwmStatus};
use snow_floppy::flux::FluxTicks;
use snow_floppy::{Floppy, FloppyImage};

use crate::bus::{Address, BusMember};
use crate::mac::swim::ism::IsmFifoEntry;
use crate::tickable::{Tickable, Ticks};
use crate::types::LatchingEvent;

enum FluxTransitionTime {
    /// 1
    Short,
    /// 01
    Medium,
    /// 001
    Long,
    /// Something else, out of spec.
    /// Contains the amount of bit cells
    OutOfSpec(usize),
}

impl FluxTransitionTime {
    pub fn from_ticks_ex(ticks: FluxTicks, _fast: bool, _highf: bool) -> Option<Self> {
        // Below is from Integrated Woz Machine (IWM) Specification, 1982, rev 19, page 4.
        // TODO fast/low frequency mode.. The Mac SE sets mode to 0x17, which makes things not work?
        match (true, true) {
            (false, false) | (true, false) => match ticks {
                7..=20 => Some(Self::Short),
                21..=34 => Some(Self::Medium),
                35..=48 => Some(Self::Long),
                56.. => Some(Self::OutOfSpec(ticks as usize / 14)),
                _ => None,
            },
            (true, true) | (false, true) => match ticks {
                8..=23 => Some(Self::Short),
                24..=39 => Some(Self::Medium),
                40..=55 => Some(Self::Long),
                56.. => Some(Self::OutOfSpec(ticks as usize / 16)),
                _ => None,
            },
        }
    }

    #[allow(dead_code)]
    pub fn from_ticks(ticks: FluxTicks) -> Option<Self> {
        Self::from_ticks_ex(ticks, true, true)
    }

    pub fn get_zeroes(self) -> usize {
        match self {
            Self::Short => 0,
            Self::Medium => 1,
            Self::Long => 2,
            Self::OutOfSpec(bc) => bc - 1,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum SwimMode {
    #[default]
    Iwm,
    Ism,
}

/// Sander-Wozniak Integrated Machine - floppy drive controller
pub struct Swim {
    ism_available: bool,

    cycles: Ticks,
    mode: SwimMode,

    pub ca0: bool,
    pub ca1: bool,
    pub ca2: bool,
    pub lstrb: bool,
    pub q6: bool,
    pub q7: bool,
    pub extdrive: bool,
    pub enable: bool,
    pub sel: bool,

    /// Internal drive select for SE
    pub(crate) intdrive: bool,

    iwm_status: IwmStatus,
    iwm_mode: IwmMode,
    shdata: u8,
    datareg: u8,
    write_shift: u8,
    write_pos: usize,
    write_buffer: Option<u8>,

    pub(super) ism_phase_mask: u8,
    pub(super) ism_error: IsmError,
    pub(super) ism_mode: IsmStatus,
    pub(super) ism_params: [u8; 16],
    pub(super) ism_param_idx: usize,
    pub(super) ism_setup: IsmSetup,
    pub(super) ism_switch_ctr: usize,
    pub(super) ism_fifo: VecDeque<IsmFifoEntry>,

    pub(crate) drives: [FloppyDrive; 3],

    pub dbg_pc: u32,
    pub dbg_break: LatchingEvent,
}

impl Swim {
    pub fn new(drives: &[DriveType], ism_available: bool) -> Self {
        Self {
            drives: core::array::from_fn(|i| {
                FloppyDrive::new(i, *drives.get(i).unwrap_or(&DriveType::None))
            }),
            ism_available,

            cycles: 0,
            // SWIM boots in IWM mode
            mode: Default::default(),

            ca0: false,
            ca1: false,
            ca2: false,
            lstrb: false,
            q6: false,
            q7: false,
            extdrive: false,
            sel: false,
            intdrive: false,

            shdata: 0,
            datareg: 0,
            write_shift: 0,
            write_pos: 0,
            write_buffer: None,

            iwm_status: IwmStatus(0),
            iwm_mode: IwmMode(0),

            ism_phase_mask: 0,
            ism_error: IsmError(0),
            ism_mode: IsmStatus(0),
            ism_params: [0; 16],
            ism_param_idx: 0,
            ism_setup: IsmSetup(0),
            ism_switch_ctr: 0,
            ism_fifo: VecDeque::new(),

            enable: false,
            dbg_pc: 0,
            dbg_break: LatchingEvent::default(),
        }
    }

    fn get_selected_drive_idx(&self) -> usize {
        if self.mode == SwimMode::Iwm {
            if self.extdrive {
                1
            } else if self.intdrive {
                2
            } else {
                0
            }
        } else {
            // ISM
            if self.ism_mode.drive2_enable() {
                1
            } else if self.ism_mode.drive1_enable() {
                0
            } else {
                // ????
                0
            }
        }
    }

    pub fn is_writing(&self) -> bool {
        self.write_buffer.is_some()
    }

    fn get_selected_drive(&self) -> &FloppyDrive {
        &self.drives[self.get_selected_drive_idx()]
    }

    fn get_selected_drive_mut(&mut self) -> &mut FloppyDrive {
        &mut self.drives[self.get_selected_drive_idx()]
    }

    /// Inserts a disk into the disk drive
    pub fn disk_insert(&mut self, drive: usize, image: FloppyImage) -> Result<()> {
        if !self.drives[drive].is_present() {
            bail!("Drive {} not present", drive);
        }

        self.drives[drive].disk_insert(image)
    }

    /// Gets the active (selected) drive head
    fn get_active_head(&self) -> usize {
        if !self.get_selected_drive().drive_type.is_doublesided()
            || self.get_selected_drive().floppy.get_side_count() == 1
            || !self.sel
        {
            0
        } else {
            1
        }
    }

    /// Converts the four register selection I/Os to a u8 value which can be used
    /// to convert to an enum value.
    fn get_selected_drive_reg_u8(&self) -> u8 {
        let mut v = 0;
        if self.ca2 {
            v |= 0b1000;
        };
        if self.ca1 {
            v |= 0b0100;
        };
        if self.ca0 {
            v |= 0b0010;
        };
        if self.sel {
            v |= 0b0001;
        };
        v
    }

    pub fn get_active_image(&self, drive: usize) -> &FloppyImage {
        &self.drives[drive].floppy
    }
}

impl BusMember<Address> for Swim {
    fn read(&mut self, addr: Address) -> Option<u8> {
        match self.mode {
            SwimMode::Iwm => self.iwm_read(addr),
            SwimMode::Ism => self.ism_read(addr),
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        match self.mode {
            SwimMode::Iwm => self.iwm_write(addr, val),
            SwimMode::Ism => self.ism_write(addr, val),
        }
        Some(())
    }
}

impl Tickable for Swim {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        debug_assert_eq!(ticks, 1);

        // This is called at the Macintosh main clock speed (TICKS_PER_SECOND == 8 MHz)
        self.cycles += ticks;
        for drv in &mut self.drives {
            drv.cycles = self.cycles;
        }

        // When an EJECT command is sent, do not actually eject the disk until eject strobe has been
        // asserted for at least 500ms. Specifications say a 750ms strobe is required.
        // For some reason, the Mac Plus ROM gives a very short eject strobe on bootup during drive
        // enumeration. If we do not ignore that, the Mac Plus always ejects the boot disk.
        if self.get_selected_drive().ejecting.is_some() && self.lstrb {
            let Some(eject_ticks) = self.get_selected_drive().ejecting else {
                unreachable!()
            };
            if eject_ticks < self.cycles {
                self.get_selected_drive_mut().eject();
            }
        } else if !self.lstrb {
            self.get_selected_drive_mut().ejecting = None;
        }

        if self.get_selected_drive().is_running() {
            // Decrement 'head stepping' timer
            let new_stepping = self.get_selected_drive().stepping.saturating_sub(ticks);
            self.get_selected_drive_mut().stepping = new_stepping;

            // Advance read/write operation
            match self.mode {
                SwimMode::Iwm => self.iwm_tick(ticks)?,
                SwimMode::Ism => self.ism_tick(ticks)?,
            }
        }

        Ok(ticks)
    }
}
