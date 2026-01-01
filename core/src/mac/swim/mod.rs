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
use serde::{Deserialize, Serialize};
use snow_floppy::flux::FluxTicks;
use snow_floppy::{Floppy, FloppyImage};

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
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

#[derive(
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    strum::IntoStaticStr,
    Clone,
    Serialize,
    Deserialize,
)]
enum SwimMode {
    #[default]
    Iwm,
    Ism,
}

/// Sander-Wozniak Integrated Machine - floppy drive controller
#[derive(Serialize, Deserialize)]
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
    iwm_zeroes: usize,
    write_shift: u8,
    write_pos: usize,
    write_buffer: Option<u8>,

    ism_phase_mask: u8,
    ism_error: IsmError,
    ism_mode: IsmStatus,
    ism_params: [u8; 16],
    ism_param_idx: usize,
    ism_setup: IsmSetup,
    ism_switch_ctr: usize,
    ism_fifo: VecDeque<IsmFifoEntry>,
    ism_shreg: u16,
    ism_synced: bool,
    ism_shreg_cnt: usize,
    ism_crc: u16,
    /// ISM write shift register (16-bit MFM encoded data)
    ism_write_shreg: u16,
    /// Bits remaining in write shift register
    ism_write_shreg_cnt: u8,
    /// Previous data bit for MFM clock calculation
    ism_write_prev_bit: bool,

    pub(crate) drives: [FloppyDrive; 3],

    pub dbg_pc: u32,
    pub dbg_break: LatchingEvent,
}

impl Swim {
    pub fn new(drives: &[DriveType], ism_available: bool, base_frequency: Ticks) -> Self {
        Self {
            drives: core::array::from_fn(|i| {
                FloppyDrive::new(
                    i,
                    *drives.get(i).unwrap_or(&DriveType::None),
                    base_frequency,
                )
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
            iwm_zeroes: 0,
            write_shift: 0,
            write_pos: 0,
            write_buffer: None,

            iwm_status: IwmStatus(0),
            iwm_mode: IwmMode(0),

            ism_phase_mask: 0xF0,
            ism_error: IsmError(0),
            ism_mode: IsmStatus(0),
            ism_params: [0; 16],
            ism_param_idx: 0,
            ism_setup: IsmSetup(0),
            ism_switch_ctr: 0,
            ism_fifo: VecDeque::new(),
            ism_shreg: 0,
            ism_synced: false,
            ism_shreg_cnt: 0,
            ism_crc: Self::ISM_CRC_INIT,
            ism_write_shreg: 0,
            ism_write_shreg_cnt: 0,
            ism_write_prev_bit: false,

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
                if self.intdrive {
                    2
                } else {
                    0
                }
            } else {
                // ???
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

        if self.get_selected_drive().ejecting.is_some() && self.lstrb {
            let Some(eject_ticks) = self.get_selected_drive().ejecting else {
                unreachable!()
            };
            if eject_ticks < self.cycles {
                self.get_selected_drive_mut().eject();
            }
        } else if !self.lstrb {
            if let Some(eject_ticks) = self.get_selected_drive().ejecting {
                log::debug!(
                    "Eject strobe too short ({} cycles)",
                    eject_ticks - self.cycles
                );
                self.get_selected_drive_mut().ejecting = None;
            }
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

impl Debuggable for Swim {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{
            dbgprop_bool, dbgprop_byte, dbgprop_byte_bin, dbgprop_enum, dbgprop_group,
            dbgprop_header, dbgprop_nest, dbgprop_string, dbgprop_udec, dbgprop_word,
            dbgprop_word_bin,
        };

        vec![
            dbgprop_enum!("Mode", self.mode),
            dbgprop_udec!("ISM switch counter", self.ism_switch_ctr),
            dbgprop_group!(
                "I/O",
                vec![
                    dbgprop_bool!("CA0", self.ca0),
                    dbgprop_bool!("CA1", self.ca1),
                    dbgprop_bool!("CA2", self.ca2),
                    dbgprop_bool!("LSTRB", self.lstrb),
                    dbgprop_bool!("Q6", self.q6),
                    dbgprop_bool!("Q7", self.q7),
                    dbgprop_bool!("Extdrive", self.extdrive),
                    dbgprop_bool!("Enable", self.enable),
                    dbgprop_bool!("SEL", self.sel),
                    dbgprop_bool!("Intdrive", self.intdrive),
                ]
            ),
            dbgprop_group!(
                "IWM",
                vec![
                    dbgprop_header!("Registers"),
                    dbgprop_byte!("Status", self.iwm_status.0),
                    dbgprop_byte!("Mode", self.iwm_mode.0),
                    dbgprop_header!("Reading"),
                    dbgprop_byte!("Data register", self.datareg),
                    dbgprop_byte_bin!("Read shifter", self.shdata),
                    dbgprop_udec!("Zeroes", self.iwm_zeroes),
                    dbgprop_header!("Writing"),
                    dbgprop_byte_bin!("Write shifter", self.write_shift),
                    dbgprop_udec!("Write position", self.write_pos),
                    dbgprop_byte!("Write buffer", self.write_buffer.unwrap_or(0)),
                ]
            ),
            dbgprop_group!(
                "ISM",
                vec![
                    dbgprop_header!("Registers"),
                    dbgprop_byte_bin!("Phase mask", self.ism_phase_mask),
                    dbgprop_byte!("Error", self.ism_error.0),
                    dbgprop_byte!("Mode", self.ism_mode.0),
                    dbgprop_byte!("Setup", self.ism_setup.0),
                    dbgprop_header!("Parameters"),
                    dbgprop_udec!("Parameter index", self.ism_param_idx),
                    dbgprop_group!(
                        "Parameters",
                        Vec::from_iter(
                            self.ism_params
                                .iter()
                                .enumerate()
                                .map(|(i, p)| dbgprop_byte!(format!("[{}]", i), *p))
                        )
                    ),
                    dbgprop_header!("Reading/writing"),
                    dbgprop_group!(
                        "FIFO",
                        Vec::from_iter(self.ism_fifo.iter().enumerate().map(
                            |(i, p)| dbgprop_string!(
                                format!("[{}]", i),
                                format!("{} {:08b} (${:02X})", p, p.inner(), p.inner())
                            )
                        ))
                    ),
                    dbgprop_word_bin!("Shifter", self.ism_shreg),
                    dbgprop_udec!("Shifter bits", self.ism_shreg_cnt),
                    dbgprop_bool!("Synchronized", self.ism_synced),
                    dbgprop_word!("CRC", self.ism_crc),
                ]
            ),
            dbgprop_nest!(
                format!("Drive #1 ({})", self.drives[0].drive_type),
                self.drives[0]
            ),
            dbgprop_nest!(
                format!("Drive #2 ({})", self.drives[1].drive_type),
                self.drives[1]
            ),
            dbgprop_nest!(
                format!("Drive #3 ({})", self.drives[2].drive_type),
                self.drives[2]
            ),
        ]
    }
}
