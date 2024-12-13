use anyhow::{bail, Result};
use log::*;
use num::clamp;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use snow_floppy::{Floppy, FloppyImage, TrackLength, TrackType};

use crate::{
    bus::{Address, BusMember},
    tickable::{Tickable, Ticks},
    types::LatchingEvent,
};

use super::drive::FloppyDrive;
use super::FluxTransitionTime;

/// Integrated Woz Machine - floppy drive controller
pub struct Iwm {
    double_sided: bool,

    cycles: Ticks,

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

    status: IwmStatus,
    mode: IwmMode,
    shdata: u8,
    datareg: u8,
    write_shift: u8,
    write_pos: usize,
    write_buffer: Option<u8>,

    pub(crate) drives: [FloppyDrive; 3],

    pub dbg_pc: u32,
    pub dbg_break: LatchingEvent,
}

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

impl Iwm {
    pub fn new(double_sided: bool, drives: usize) -> Self {
        Self {
            drives: core::array::from_fn(|i| FloppyDrive::new(i, i < drives, double_sided)),
            double_sided,
            cycles: 0,

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

            status: IwmStatus(0),
            mode: IwmMode(0),

            enable: false,
            dbg_pc: 0,
            dbg_break: LatchingEvent::default(),
        }
    }

    fn get_selected_drive_idx(&self) -> usize {
        if self.extdrive {
            1
        } else if self.intdrive {
            2
        } else {
            0
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

    /// A memory-mapped I/O address was accessed (offset from IWM base address)
    fn access(&mut self, offset: Address) {
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

    /// Reads the currently selected (Q6, Q7) IWM register
    fn iwm_read(&mut self) -> u8 {
        match (self.q6, self.q7) {
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
                self.status.set_sense(sense);
                self.status.set_mode_low(self.mode.mode_low());
                self.status.set_enable(self.enable);

                self.status.0
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

    /// Inserts a disk into the disk drive
    pub fn disk_insert(&mut self, drive: usize, image: FloppyImage) -> Result<()> {
        if !self.drives[drive].present {
            bail!("Drive {} not present", drive);
        }

        self.drives[drive].disk_insert(image)
    }

    /// Gets the active (selected) drive head
    fn get_active_head(&self) -> usize {
        if !self.double_sided || self.get_selected_drive().floppy.get_side_count() == 1 || !self.sel
        {
            0
        } else {
            1
        }
    }

    /// Update current drive PWM signal from the sound buffer
    pub fn push_pwm(&mut self, pwm: u8) -> Result<()> {
        const VALUE_TO_LEN: [u8; 64] = [
            0, 1, 59, 2, 60, 40, 54, 3, 61, 32, 49, 41, 55, 19, 35, 4, 62, 52, 30, 33, 50, 12, 14,
            42, 56, 16, 27, 20, 36, 23, 44, 5, 63, 58, 39, 53, 31, 48, 18, 34, 51, 29, 11, 13, 15,
            26, 22, 43, 57, 38, 47, 17, 28, 10, 25, 21, 37, 46, 9, 24, 45, 8, 7, 6,
        ];

        if self.double_sided {
            // Only single-sided drives are PWM controlled
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

    pub fn get_active_image(&self, drive: usize) -> &FloppyImage {
        &self.drives[drive].floppy
    }

    fn tick_bitstream(&mut self, ticks: usize) -> Result<()> {
        assert_eq!(ticks, 1);
        if self.cycles % self.get_selected_drive().get_ticks_per_bit() != 0 {
            return Ok(());
        }

        let head = self.get_active_head();

        // Progress the head over the track
        let bit = self.get_selected_drive_mut().next_bit(head);
        self.shift_bit(bit);

        if self.write_pos == 0 && self.write_buffer.is_some() {
            // Write idle and new data in write FIFO, start writing 8 new bits
            let Some(v) = self.write_buffer else {
                unreachable!()
            };
            self.write_shift = v;
            self.write_pos = 8;
            self.write_buffer = None;
        }
        if self.write_pos > 0 {
            // Write in progress - write one bit to current head location
            let bit = self.write_shift & 0x80 != 0;
            let head = self.get_active_head();
            self.write_shift <<= 1;
            self.write_pos -= 1;
            self.get_selected_drive_mut().write_bit(head, bit);
        }

        Ok(())
    }

    fn tick_flux(&mut self, ticks: usize) -> Result<()> {
        let side = self.get_active_head();
        let track = self.get_selected_drive().get_active_track();
        self.get_selected_drive_mut().flux_ticks_left -= ticks as i16;

        // Not sure how long this should be?
        if self.get_selected_drive().flux_ticks_left < self.get_selected_drive().flux_ticks - 20 {
            self.get_selected_drive_mut().head_bit[side] = false;
        }

        if self.get_selected_drive().flux_ticks_left <= 0 {
            // Flux transition occured

            // Introduce some pseudo-random jitter on the timing to emulate
            // the minor differences introduced by motor RPM instability and
            // physical movement of the disk donut.
            let jitter = -2 + (self.cycles % 4) as i16;

            // Check bit cell window
            // TODO incorporate actual drive speed from PWM on 128K/512K?
            if let Some(time) = FluxTransitionTime::from_ticks_ex(
                self.get_selected_drive().flux_ticks + jitter,
                self.mode.fast(),
                self.mode.speed(),
            ) {
                // Transition occured within the window, shift bits into the
                // IWM shift register.
                for _ in 0..(time.get_zeroes()) {
                    self.shift_bit(false);
                }
                self.shift_bit(true);
                self.get_selected_drive_mut().head_bit[side] = true;
            }

            // Advance image to the next transition
            let TrackLength::Transitions(tlen) =
                self.get_selected_drive().get_track_len(side, track)
            else {
                unreachable!()
            };
            self.get_selected_drive_mut().track_position =
                (self.get_selected_drive().track_position + 1) % tlen;
            self.get_selected_drive_mut().flux_ticks = self
                .get_selected_drive()
                .floppy
                .get_track_transition(side, track, self.get_selected_drive().track_position);
            self.get_selected_drive_mut().flux_ticks_left = self.get_selected_drive().flux_ticks;
        }

        if self.write_pos == 0 && self.write_buffer.is_some() {
            // Write idle and new data in write FIFO
            error!("Writing to track {} (flux track) is unsupported!", track);
            self.write_buffer = None;
        }

        Ok(())
    }

    /// Shifts a bit into the read data shift register
    fn shift_bit(&mut self, bit: bool) {
        self.shdata <<= 1;
        if bit {
            self.shdata |= 1;
        }

        if self.shdata & 0x80 != 0 {
            // Data is moved to the data register when the most significant bit is set.
            // Because the Mac uses GCR encoding, the most significant bit is always set in
            // any valid data.
            self.datareg = self.shdata;
            self.shdata = 0;
        }
    }
}

impl BusMember<Address> for Iwm {
    fn read(&mut self, addr: Address) -> Option<u8> {
        // Only the lower 8-bits of the databus are connected to IWM.
        // Assume the upper 8 bits are undefined.
        if addr & 1 == 0 {
            return None;
        }

        self.access(addr - 0xDFE1FF);
        let result = self.iwm_read();
        Some(result)
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        // UDS/LDS are not connected to IWM, so ignore the lower address bit here.
        self.access((addr | 1) - 0xDFE1FF);

        match (self.q6, self.q7, self.enable) {
            (true, true, false) => {
                // Write MODE
                if val != 0x1F {
                    warn!("Non-standard IWM mode: {:02X}", val);
                }
                self.mode.set_mode(val);
            }
            (true, true, true) => {
                if self.write_buffer.is_some() {
                    warn!("Disk write while write buffer not empty");
                }
                self.write_buffer = Some(val);
            }
            _ => (),
        }

        Some(())
    }
}

impl Tickable for Iwm {
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
            match self.get_selected_drive().floppy.get_track_type(
                self.get_active_head(),
                self.get_selected_drive().get_active_track(),
            ) {
                TrackType::Bitstream => self.tick_bitstream(ticks)?,
                TrackType::Flux => self.tick_flux(ticks)?,
            }
        }

        Ok(ticks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disk revolutions/minute at outer track (0)
    const DISK_RPM_OUTER: Ticks = 402;

    /// Disk revolutions/minute at inner track (79)
    const DISK_RPM_INNER: Ticks = 603;

    #[test]
    fn disk_double_tacho_outer() {
        let mut drv = FloppyDrive::new(0, true, true);
        drv.floppy_inserted = true;
        drv.motor = true;
        drv.track = 0;

        let mut last = false;
        let mut result = 0;

        for _ in 0..(TICKS_PER_SECOND * 60) {
            drv.cycles += 1;
            if drv.get_tacho() != last {
                result += 1;
                last = drv.get_tacho();
            }
        }

        assert_eq!(result / 10, DISK_RPM_OUTER * 120 / 10);
    }

    #[test]
    fn disk_double_tacho_inner() {
        let mut drv = FloppyDrive::new(0, true, true);
        drv.floppy_inserted = true;
        drv.motor = true;
        drv.track = 79;

        let mut last = false;
        let mut result = 0;

        for _ in 0..(TICKS_PER_SECOND * 60) {
            drv.cycles += 1;
            if drv.get_tacho() != last {
                result += 1;
                last = drv.get_tacho();
            }
        }

        // Roughly is good enough..
        assert_eq!(result / 10, DISK_RPM_INNER * 120 / 10);
    }
}
