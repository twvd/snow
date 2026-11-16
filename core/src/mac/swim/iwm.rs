//! Integrated Wozniak Machine

use anyhow::Result;
use log::*;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use snow_floppy::TrackType;

use super::stream::FloppyStream;
use super::{FluxTransitionTime, MOTOR_TIMER, Swim, SwimChip, SwimMode};
use crate::bus::Address;
use crate::tickable::Ticks;
use crate::types::Byte;

bitfield! {
    /// IWM handshake register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IwmHandshake(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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
    pub struct IwmStatus(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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
    pub struct IwmMode(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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
    /// The MOTOR-ON state: ENABLE is on, or the one-second timer still holds
    /// it. It enables the selected drive, and selects the registers.
    pub(super) fn iwm_drive_enabled(&self) -> bool {
        self.enable || self.enable_hold > self.cycles
    }

    /// A memory-mapped I/O address was accessed
    pub(super) fn iwm_access(&mut self, offset: Address) {
        match (offset >> 9) & 0x0F {
            0 => self.ca0 = false,
            1 => self.ca0 = true,
            2 => self.ca1 = false,
            3 => self.ca1 = true,
            4 => self.ca2 = false,
            5 => self.ca2 = true,
            6 => self.lstrb = false,
            7 => {
                // The drive latches its control register on the rising edge of LSTRB,
                // so touching LSTRBH while it is already high does nothing.
                // "For any communication with the drive, the /ENBL line must
                // be low" (Apple 400K drive spec 699-0285 and 800K drive ERS
                // 669-0452, section 3.1).
                if !self.lstrb {
                    self.lstrb = true;
                    if !self.iwm_drive_enabled() {
                        return;
                    }

                    self.strobe();
                }
            }
            8 => {
                if self.enable && !self.chip.iwm_mode.onesec() {
                    // The IWM patent (US 4,742,448) gives "one-half
                    // millisecond" for the timer when the LATCH mode bit is
                    // set.
                    let hold = if self.chip.iwm_mode.latch() {
                        self.base_frequency / 2000
                    } else {
                        MOTOR_TIMER
                    };
                    self.enable_hold = self.cycles + hold;
                }
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
        self.iwm_access(addr);

        let val = match (self.q6, self.q7) {
            (false, false) => {
                // Data register
                if !self.iwm_drive_enabled() {
                    0xFF
                } else {
                    std::mem::replace(&mut self.chip.datareg, 0)
                }
            }
            (true, false) => {
                // Read status register
                let sense = self.sense();
                self.chip.iwm_status.set_sense(sense);
                self.chip
                    .iwm_status
                    .set_mode_low(self.chip.iwm_mode.mode_low());
                self.chip.iwm_status.set_enable(self.iwm_drive_enabled());

                // Reading status clears the shifter
                // (used in copy protections)
                self.chip.shdata = 0;

                self.chip.iwm_status.0
            }
            (false, true) => {
                // Read handshake register
                let mut result = IwmHandshake::default();

                result
                    .set_underrun(!(self.chip.write_pos == 0 && self.chip.write_buffer.is_none()));
                result.set_ready(self.chip.write_buffer.is_none());
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
        self.iwm_access(addr);

        match (self.q6, self.q7, self.iwm_drive_enabled()) {
            (true, true, false) => {
                // Write MODE
                if self.ism_available && ISM_SWITCH_PATTERN[self.chip.ism_switch_ctr] == value {
                    self.chip.ism_switch_ctr += 1;
                } else {
                    self.chip.ism_switch_ctr = 0;
                }
                if self.chip.ism_switch_ctr == ISM_SWITCH_PATTERN.len() {
                    self.mode = SwimMode::Ism;
                    self.chip.ism_mode.set_ism(true);
                    self.chip.ism_switch_ctr = 0;
                    return;
                }
                if ![0x17, 0x1F, 0x57].contains(&value) {
                    warn!("Non-standard IWM mode: {:02X}", value);
                }
                self.chip.iwm_mode.set_mode(value);
            }
            (true, true, true) => {
                if self.chip.write_buffer.is_some() {
                    warn!("Disk write while write buffer not empty");
                }
                self.chip.write_buffer = Some(value);
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

        let pulse = VALUE_TO_LEN[usize::from(pwm) % VALUE_TO_LEN.len()];
        self.sync_devices();
        for device in self.drives.iter_mut().flatten() {
            device.set_pwm(pulse);
        }
        Ok(())
    }
}

impl SwimChip {
    /// Shifts a bit into the read data shift register
    fn iwm_shift_bit(&mut self, stream: &FloppyStream, bit: bool) {
        self.shdata <<= 1;
        if bit {
            // 1 coming off the disk
            self.iwm_zeroes = 0;
            self.shdata |= 1;
        } else {
            // 0 coming off the disk (no transition within the bit cell window)
            //
            // If this happens more than twice (valid sequences are 1, 01, 001),
            // meaning there are no flux transitions for a period beyond the drive/IWM
            // specs, the analogue characteristics (automatic gain control) of the
            // drive will cause it to start picking up more noise and generate spurious
            // transitions at random intervals.
            // This is sometimes called 'weak bits'. Some copy protection schemes rely on
            // this phenomenon.
            self.iwm_zeroes += 1;
            if self.iwm_zeroes > 4 && stream.weak_bit() {
                self.shdata |= 1;
            }
        }

        if self.shdata & 0x80 != 0 {
            // Data is moved to the data register when the most significant bit is set.
            // Because the Mac uses GCR encoding, the most significant bit is always set in
            // any valid data.
            self.datareg = self.shdata;
            self.shdata = 0;
        }
    }

    fn iwm_tick_flux(
        &mut self,
        stream: &mut FloppyStream,
        head: usize,
        ticks: Ticks,
        cycles: Ticks,
    ) -> Result<()> {
        // Run up to each flux transition within the ticks in turn
        let mut left = ticks;
        while left > 0 {
            let step = left.min(stream.ticks_to_transition());
            left -= step;
            if stream.advance_flux(head, step) {
                self.iwm_flux_transition(stream, head, cycles - left);
            }
        }

        if self.write_pos == 0 && self.write_buffer.is_some() {
            // Write idle and new data in write FIFO
            error!(
                "Writing to track {} (flux track) is unsupported!",
                stream.track()
            );
            self.write_buffer = None;
        }

        Ok(())
    }

    /// Handles a flux transition passing under the head at `cycles`
    fn iwm_flux_transition(&mut self, stream: &mut FloppyStream, head: usize, cycles: Ticks) {
        // Introduce some pseudo-random jitter on the timing to emulate
        // the minor differences introduced by motor RPM instability and
        // physical movement of the disk donut.
        let jitter = -3 + (cycles % 6) as i16;

        // Check bit cell window
        // TODO incorporate actual drive speed from PWM on 128K/512K?
        if let Some(time) = FluxTransitionTime::from_ticks_ex(
            stream.transition_ticks() + jitter,
            self.iwm_mode.fast(),
            self.iwm_mode.speed(),
        ) {
            // Transition occured within the window, shift bits into the
            // IWM shift register.
            for _ in 0..(time.get_zeroes()) {
                self.iwm_shift_bit(stream, false);
            }
            self.iwm_shift_bit(stream, true);
            stream.transition_read(head);
        }

        // Advance image to the next transition
        stream.next_transition(head);
    }

    fn iwm_tick_bitstream(
        &mut self,
        stream: &mut FloppyStream,
        head: usize,
        ticks: Ticks,
    ) -> Result<()> {
        self.bit_cycles += ticks;
        while self.bit_cycles >= stream.ticks_per_bit() {
            self.bit_cycles -= stream.ticks_per_bit();
            self.iwm_tick_bit(stream, head)?;
        }
        Ok(())
    }

    /// Moves the head over one bit cell
    fn iwm_tick_bit(&mut self, stream: &mut FloppyStream, head: usize) -> Result<()> {
        // Progress the head over the track
        let bit = stream.next_bit(head);
        self.iwm_shift_bit(stream, bit);

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
            self.write_shift <<= 1;
            self.write_pos -= 1;
            stream.write_bit(head, bit);
        }

        Ok(())
    }

    /// Advances the read/write operation on the disk under the head, by `ticks` up to the
    /// time `cycles`
    pub(super) fn iwm_tick(
        &mut self,
        stream: &mut FloppyStream,
        head: usize,
        ticks: Ticks,
        cycles: Ticks,
    ) -> Result<()> {
        match stream.track_type(head) {
            TrackType::Bitstream => self.iwm_tick_bitstream(stream, head, ticks),
            TrackType::Flux => self.iwm_tick_flux(stream, head, ticks, cycles),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::BusMember;
    use crate::mac::swim::drive::DriveType;
    use crate::tickable::Tickable;
    use snow_floppy::{FloppyImage, FloppyType};

    const CA0L: Address = 0 << 9;
    const CA0H: Address = 1 << 9;
    const LSTRBL: Address = 6 << 9;
    const LSTRBH: Address = 7 << 9;
    const CA1H: Address = 3 << 9;
    const CA2H: Address = 5 << 9;
    const DISABLE: Address = 8 << 9;
    const ENABLE: Address = 9 << 9;

    #[test]
    fn bit_rate_follows_the_pwm_speed() {
        let mut swim = Swim::new(false, 8_000_000).with_drives(&[DriveType::GCR400K]);
        swim.drive(0)
            .disk_insert(FloppyImage::new(FloppyType::Mac400K, ""))
            .unwrap();

        // No drive command in between: these only change the PWM duty cycle
        let mut ticks_per_bit = |pwm| {
            for _ in 0..100 {
                swim.push_pwm(pwm).unwrap();
            }
            swim.drive(0).get_ticks_per_bit()
        };
        let fast = ticks_per_bit(0x2B);
        let slow = ticks_per_bit(0x3E);
        assert!(
            slow > fast,
            "{} ticks at the slow speed, {} at the fast",
            slow,
            fast
        );
    }

    #[test]
    fn lstrb_writes_on_rising_edge_only() {
        let mut swim = Swim::new(false, 16_000_000).with_drives(&[DriveType::GCR800K]);
        swim.read(ENABLE);
        let start = swim.drive(0).stream.track;

        // TRACKUP (all lines low), then TRACKSTEP (CA0 high) with LSTRBH accessed
        // five times: one rising edge, one step
        swim.read(CA0L);
        swim.read(LSTRBH);
        swim.read(LSTRBL);
        swim.read(CA0H);
        for _ in 0..5 {
            swim.read(LSTRBH);
        }
        swim.read(LSTRBL);
        assert_eq!(swim.drive(0).stream.track, start + 1);

        // A new pulse steps again
        swim.read(LSTRBH);
        swim.read(LSTRBL);
        assert_eq!(swim.drive(0).stream.track, start + 2);
    }

    #[test]
    fn strobe_ignored_without_enable() {
        const Q6L: Address = 12 << 9;
        const Q6H: Address = 13 << 9;
        const Q7L: Address = 14 << 9;
        const MODE: Address = 15 << 9;

        let mut swim = Swim::new(false, 16_000_000).with_drives(&[DriveType::GCR800K]);
        swim.drive(0)
            .disk_insert(FloppyImage::new(FloppyType::Mac800K, ""))
            .unwrap();
        // Mode $17, as Mac OS sets it: one-second timer disabled
        swim.read(Q6H);
        swim.write(MODE, 0x17);
        swim.read(Q6L);
        swim.read(Q7L);

        // EJECT (CA2..0 high, SEL low) strobed with the drive disabled
        swim.read(DISABLE);
        swim.read(CA0H);
        swim.read(CA1H);
        swim.read(CA2H);
        swim.read(LSTRBH);
        swim.tick(1, ()).unwrap();
        swim.read(LSTRBL);
        swim.tick(1, ()).unwrap();
        assert!(swim.drive(0).floppy_inserted);

        // The same strobe with the drive enabled ejects
        swim.read(ENABLE);
        swim.read(LSTRBH);
        swim.tick(1, ()).unwrap();
        swim.read(LSTRBL);
        swim.tick(1, ()).unwrap();
        assert!(!swim.drive(0).floppy_inserted);
    }

    #[test]
    fn enable_held_by_timer() {
        let mut swim = Swim::new(false, 16_000_000).with_drives(&[DriveType::GCR800K]);
        swim.drive(0)
            .disk_insert(FloppyImage::new(FloppyType::Mac800K, ""))
            .unwrap();

        // Mode 0, as after a reset: the drive stays enabled for 2^23 FCLK
        // periods after ENABLE goes off
        swim.read(ENABLE);
        swim.read(DISABLE);
        swim.read(CA0H);
        swim.read(CA1H);
        swim.read(CA2H);
        swim.tick((1 << 23) + 1, ()).unwrap();
        swim.read(LSTRBH);
        swim.tick(1, ()).unwrap();
        swim.read(LSTRBL);
        swim.tick(1, ()).unwrap();
        assert!(swim.drive(0).floppy_inserted);

        swim.read(ENABLE);
        swim.read(DISABLE);
        swim.read(LSTRBH);
        swim.tick(1, ()).unwrap();
        swim.read(LSTRBL);
        swim.tick(1, ()).unwrap();
        assert!(!swim.drive(0).floppy_inserted);
    }

    #[test]
    fn motor_on_held_by_timer() {
        const Q6H: Address = 13 << 9;
        const Q7L: Address = 14 << 9;
        const MODE: Address = 15 << 9;

        // Status register: MOTOR-ON (bit 5) stays set until the timer
        // has counted down
        let mut swim = Swim::new(false, 16_000_000).with_drives(&[DriveType::GCR800K]);
        swim.read(ENABLE);
        swim.read(DISABLE);
        swim.read(Q7L);
        assert_eq!(swim.read(Q6H).unwrap() & 0x20, 0x20);
        swim.tick((1 << 23) + 1, ()).unwrap();
        assert_eq!(swim.read(Q6H).unwrap() & 0x20, 0x00);

        // With LATCH set, the timer runs for half a millisecond
        swim.write(MODE, 0x01);
        swim.read(Q7L);
        swim.read(ENABLE);
        swim.read(DISABLE);
        assert_eq!(swim.read(Q6H).unwrap() & 0x20, 0x20);
        swim.tick(16_000_000 / 2000 + 1, ()).unwrap();
        assert_eq!(swim.read(Q6H).unwrap() & 0x20, 0x00);
    }

    #[test]
    fn eject_brief_strobe() {
        // As the Mac OS driver ejects in IWM mode: drive enabled, LSTRB
        // raised and lowered again
        let mut swim = Swim::new(false, 16_000_000).with_drives(&[DriveType::GCR800K]);
        swim.drive(0)
            .disk_insert(FloppyImage::new(FloppyType::Mac800K, ""))
            .unwrap();
        swim.read(ENABLE);
        swim.read(CA0H);
        swim.read(CA1H);
        swim.read(CA2H);
        swim.read(LSTRBH);
        swim.read(LSTRBL);
        assert!(!swim.drive(0).floppy_inserted);
    }
}
