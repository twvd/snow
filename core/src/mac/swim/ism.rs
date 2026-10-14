use std::mem;

use anyhow::Result;
use log::*;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use snow_floppy::TrackType;

use crate::bus::Address;
use crate::mac::swim::SwimMode;
use crate::tickable::Ticks;
use crate::types::Byte;

use super::Swim;

#[derive(Debug)]
enum IsmRegister {
    Data,
    #[allow(dead_code)]
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
    pub struct IsmStatus(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct IsmError(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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
    pub struct IsmHandshake(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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
    pub struct IsmSetup(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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

#[derive(Debug, strum::Display, Serialize, Deserialize)]
pub(super) enum IsmFifoEntry {
    Marker(u8),
    Data { value: u8, crc_valid: bool },
    Crc,
    CrcLow(u8),
}

impl IsmFifoEntry {
    pub fn inner(&self) -> u8 {
        match self {
            Self::Marker(d) => *d,
            Self::Data { value, .. } => *value,
            Self::Crc => 0, // Will be replaced with actual CRC
            Self::CrcLow(b) => *b,
        }
    }

    pub fn crc_valid(&self) -> bool {
        match self {
            Self::Marker(_) | Self::Crc | Self::CrcLow(_) => false,
            Self::Data { crc_valid, .. } => *crc_valid,
        }
    }
}

impl Swim {
    /// MFM sync marker (0xA1 with dropped clock)
    const MFM_SYNC_MARKER: u16 = 0b01_00_01_00_10_00_10_01u16;
    /// ISM CRC-CCITT polynomial
    const ISM_CRC_POLYNOMIAL: u16 = 0x1021;
    /// ISM CRC initialization value
    pub(super) const ISM_CRC_INIT: u16 = 0xcdb4;

    /// Update CRC with a single bit
    fn ism_crc_update(&mut self, bit: bool) {
        if (self.ism_crc ^ (if bit { 0x8000 } else { 0x0000 })) & 0x8000 != 0 {
            self.ism_crc = (self.ism_crc << 1) ^ Self::ISM_CRC_POLYNOMIAL;
        } else {
            self.ism_crc <<= 1;
        }
    }

    /// Update CRC, takes an MFM decoded byte
    fn ism_crc_update_byte(&mut self, byte: u8) {
        for i in (0..8).rev() {
            let bit = (byte >> i) & 1 != 0;
            self.ism_crc_update(bit);
        }
    }

    fn ism_mfm_decode(mfm: u16) -> u8 {
        let mut out = 0;
        for i in 0..8 {
            if mfm & (1 << (i * 2)) != 0 {
                out |= 1 << i;
            }
        }
        out
    }

    /// Encodes a byte using MFM encoding.
    /// MFM places clock bits between data bits: C7 D7 C6 D6 ... C0 D0
    /// Clock bit is 1 only when both previous and current data bits are 0.
    /// Returns (encoded 16-bit word, last data bit for chaining)
    fn ism_mfm_encode(data: u8, prev_bit: bool) -> (u16, bool) {
        let mut out: u16 = 0;
        let mut last_bit = prev_bit;

        // Process bits from MSB to LSB (bit 7 to bit 0)
        for i in (0..8).rev() {
            let data_bit = (data >> i) & 1 != 0;
            let bit_pos = i * 2; // Data bit position in output

            // Set data bit
            if data_bit {
                out |= 1 << bit_pos;
            }

            // Set clock bit (position is bit_pos + 1)
            // Clock is 1 only if both previous data bit and current data bit are 0
            if !last_bit && !data_bit {
                out |= 1 << (bit_pos + 1);
            }

            last_bit = data_bit;
        }

        (out, last_bit)
    }

    fn ism_fifo_pop(&mut self, expect_marker: bool) -> Option<(bool, u8)> {
        match self.ism_fifo.pop_front()? {
            IsmFifoEntry::Data { value, .. } => Some((false, value)),
            IsmFifoEntry::Marker(d) => Some((!expect_marker, d)),
            IsmFifoEntry::Crc | IsmFifoEntry::CrcLow(_) => {
                log::error!("CRC FIFO entry in read mode");
                Some((false, 0))
            }
        }
    }

    /// A memory-mapped I/O address was read
    pub(super) fn ism_read(&mut self, addr: Address) -> Option<Byte> {
        let offset = (addr >> 9) & 0x0F;

        if let Some(reg) = IsmRegister::from(offset, false, false) {
            let result = match reg {
                IsmRegister::Data | IsmRegister::Mark => {
                    if !self.ism_mode.action() {
                        return Some(0xFF);
                    }
                    if self.ism_mode.write() {
                        log::warn!("Reading data/mark while in write mode??");
                    }
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
                IsmRegister::Error => Some(mem::take(&mut self.ism_error).0),
                IsmRegister::Status => Some(self.ism_mode.0),
                IsmRegister::Phase => Some(self.ism_read_phases()),
                IsmRegister::Handshake => {
                    let default_entry = IsmFifoEntry::Data {
                        value: 0,
                        crc_valid: false,
                    };
                    let last_entry = self.ism_fifo.back().unwrap_or(&default_entry);
                    Some(
                        IsmHandshake(0)
                            .with_mark(matches!(
                                *self.ism_fifo.front().unwrap_or(&default_entry),
                                IsmFifoEntry::Marker(_)
                            ))
                            .with_crc_error(!last_entry.crc_valid())
                            .with_sense(
                                !self.get_selected_drive().is_present()
                                    || self
                                        .get_selected_drive()
                                        .read_sense(self.get_selected_drive_reg_u8()),
                            )
                            .with_motoron(self.get_selected_drive().motor)
                            .with_error(self.ism_error.0 != 0)
                            .with_fifo_two(if self.ism_mode.write() {
                                // In write mode, indicates FIFO has room for 2+ bytes
                                self.ism_fifo.is_empty()
                            } else {
                                // In read mode, indicates FIFO has 2+ bytes available
                                self.ism_fifo.len() >= 2
                            })
                            .with_fifo_one(if self.ism_mode.write() {
                                // In write mode, indicates FIFO has room for 1+ byte
                                self.ism_fifo.len() < 2
                            } else {
                                // In read mode, indicates FIFO has 1+ byte available
                                !self.ism_fifo.is_empty()
                            })
                            .0,
                    )
                }
                IsmRegister::Parameter => {
                    let value = self.ism_params[self.ism_param_idx];
                    self.ism_param_idx = (self.ism_param_idx + 1) % self.ism_params.len();
                    Some(value)
                }
                IsmRegister::Setup => Some(self.ism_setup.0),
                _ => {
                    warn!("Unimplemented read {:?}", reg);
                    Some(0)
                }
            };
            //debug!(
            //    "ISM read {:06X} {:02X} {:?}: {:02X}",
            //    addr,
            //    offset,
            //    reg,
            //    result.unwrap()
            //);
            #[allow(clippy::let_and_return)]
            result
        } else {
            error!("Unknown ISM register read {:04X}", offset);
            Some(0)
        }
    }

    pub(super) fn ism_write(&mut self, addr: Address, value: Byte) {
        let offset = (addr >> 9) & 0x0F;

        if let Some(reg) = IsmRegister::from(offset, self.ism_mode.action(), true) {
            //debug!(
            //    "ISM write {:06X} {:02X} {:?}: {:02X}",
            //    addr, offset, reg, value
            //);
            match reg {
                IsmRegister::Data | IsmRegister::Mark => {
                    // In write mode, push data to FIFO for writing to disk
                    if self.ism_fifo.len() >= 2 {
                        warn!("ISM write FIFO overrun");
                        self.ism_error.set_overrun(true);
                    } else if matches!(reg, IsmRegister::Mark) {
                        self.ism_fifo.push_back(IsmFifoEntry::Marker(value));
                    } else {
                        self.ism_fifo.push_back(IsmFifoEntry::Data {
                            value,
                            crc_valid: false,
                        });
                    }
                }
                IsmRegister::Phase => self.ism_write_phases(value),
                IsmRegister::ModeZero => {
                    self.ism_param_idx = 0;

                    // Without an IWM register set, the ISM one stays selected
                    let value = if self.iwm_available {
                        value
                    } else {
                        IsmStatus(value).with_ism(false).0
                    };

                    let clr = IsmStatus(value & self.ism_mode.0);
                    if clr.clear_fifo() {
                        self.ism_fifo.clear();
                        self.ism_crc = Self::ISM_CRC_INIT;
                    }
                    if clr.ism() {
                        self.mode = SwimMode::Iwm;
                    }

                    self.ism_mode.0 &= !value;
                }
                IsmRegister::ModeOne => {
                    let set = IsmStatus(value & !self.ism_mode.0);
                    if set.action() {
                        self.ism_synced = false;

                        if self.ism_mode.write() {
                            // Entering write mode - initialize write state
                            self.ism_write_shreg = 0;
                            self.ism_write_shreg_cnt = 0;
                            self.ism_write_prev_bit = false;
                        } else {
                            // Entering read mode - reset sync/shifter
                            self.ism_shreg = 0;
                            self.ism_shreg_cnt = 0;
                        }
                    }
                    self.ism_mode.0 |= value;
                }
                IsmRegister::Parameter => {
                    self.ism_params[self.ism_param_idx] = value;
                    self.ism_param_idx = (self.ism_param_idx + 1) % self.ism_params.len();
                }
                IsmRegister::Setup => {
                    self.ism_setup.0 = value;
                }
                IsmRegister::Crc => {
                    // In write mode, push CRC entry to FIFO
                    // Hardware will automatically write actual CRC bytes
                    if self.ism_fifo.len() >= 2 {
                        log::warn!("ISM write FIFO overrun");
                        self.ism_error.set_overrun(true);
                    } else {
                        self.ism_fifo.push_back(IsmFifoEntry::Crc);
                    }
                }
                _ => (),
            }
        } else {
            error!("Unknown ISM register write {:04X}", offset);
        }
    }

    /// One of the drive enable lines is asserted
    fn ism_drive_enabled(&self) -> bool {
        self.ism_mode.drive1_enable() || self.ism_mode.drive2_enable()
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
        if self.lstrb {
            phases |= 1 << 3;
        }
        phases
    }

    fn ism_write_phases(&mut self, phases: u8) {
        let outputs = (phases >> 4) & (phases & 0x0F);
        self.ism_phase_mask = phases;

        self.ca0 = outputs & (1 << 0) != 0;
        self.ca1 = outputs & (1 << 1) != 0;
        self.ca2 = outputs & (1 << 2) != 0;
        if !self.lstrb && outputs & (1 << 3) != 0 && self.ism_drive_enabled() {
            // Write strobe
            let reg = self.get_selected_drive_reg_u8();
            let cycles = self.cycles;
            self.get_selected_drive_mut().write_drive_reg(reg, cycles);
        }
        self.lstrb = outputs & (1 << 3) != 0;
    }

    pub(super) fn ism_tick(&mut self, ticks: Ticks) -> Result<()> {
        self.bit_cycles += ticks;
        if self.bit_cycles < self.get_selected_drive().get_ticks_per_bit() {
            return Ok(());
        }

        self.bit_cycles -= self.get_selected_drive().get_ticks_per_bit();

        if self.get_selected_drive().floppy.get_track_type(
            self.get_active_head(),
            self.get_selected_drive().get_active_track(),
        ) == TrackType::Flux
        {
            error!("TODO flux track on ISM");
            return Ok(());
        }

        // Progress head over the track
        let head = self.get_active_head();
        let bit = self.get_selected_drive_mut().next_bit(head);
        self.ism_shreg <<= 1;
        if bit {
            self.ism_shreg |= 1;
        }
        self.ism_shreg_cnt += 1;

        // Action mode must be enabled for ISM read/write operations
        if !self.ism_mode.action() {
            return Ok(());
        }

        if self.ism_mode.write() {
            // Handle write mode
            return self.ism_tick_write();
        }

        // Read mode
        if self.ism_setup.gcr() {
            // A byte is complete when its most significant bit is set
            if self.ism_shreg & 0x80 != 0 {
                self.ism_fifo.push_back(IsmFifoEntry::Data {
                    value: self.ism_shreg as u8,
                    crc_valid: false,
                });
                if self.ism_fifo.len() > 2 {
                    self.ism_error.set_underrun(true);
                    self.ism_fifo.pop_front();
                }
                self.ism_shreg = 0;
            }
            return Ok(());
        }

        if !self.ism_synced && self.ism_shreg == Self::MFM_SYNC_MARKER {
            // Synchronized to the markers now, get ready to clock out bytes
            self.ism_shreg_cnt = 0;
            self.ism_shreg = 0;
            self.ism_synced = true;
            self.ism_crc = Self::ISM_CRC_INIT;

            self.ism_fifo
                .push_back(IsmFifoEntry::Marker(Self::ism_mfm_decode(
                    Self::MFM_SYNC_MARKER,
                )));
        }

        if self.ism_synced && self.ism_shreg_cnt == 16 {
            let decoded_byte = Self::ism_mfm_decode(self.ism_shreg);
            let is_marker = self.ism_shreg == Self::MFM_SYNC_MARKER;

            if !is_marker {
                // Data
                self.ism_crc_update_byte(decoded_byte);
                self.ism_fifo.push_back(IsmFifoEntry::Data {
                    value: decoded_byte,
                    crc_valid: self.ism_crc == 0,
                });
            } else {
                // Reset CRC when another marker is detected
                self.ism_crc = Self::ISM_CRC_INIT;
                self.ism_fifo.push_back(IsmFifoEntry::Marker(decoded_byte));
            }

            if self.ism_fifo.len() > 2 {
                self.ism_error.set_underrun(true);
                self.ism_fifo.pop_front();
            }

            self.ism_shreg = 0;
            self.ism_shreg_cnt = 0;
        }

        Ok(())
    }

    /// The bytes of the FIFO get shifted out without a spinning disk too.
    /// They take 16 microseconds each here, whatever the setup and the
    /// parameters are.
    pub(super) fn ism_tick_no_disk(&mut self, ticks: Ticks) {
        if !self.ism_mode.action() || !self.ism_mode.write() {
            return;
        }

        let byte_ticks = self.get_selected_drive().base_frequency * 16 / 1_000_000;
        self.bit_cycles += ticks;
        while self.bit_cycles >= byte_ticks && self.ism_mode.action() {
            self.bit_cycles -= byte_ticks;

            if self.ism_fifo.pop_front().is_none() {
                self.ism_error.set_underrun(true);
                self.ism_mode.set_action(false);
            }
        }
    }

    /// Handle ISM write mode tick
    fn ism_tick_write(&mut self) -> Result<()> {
        let head = self.get_active_head();

        if self.ism_write_shreg_cnt == 0 {
            // Load next data from FIFO
            let Some(entry) = self.ism_fifo.pop_front() else {
                // FIFO empty - underrun, end of write
                self.ism_error.set_underrun(true);
                self.ism_mode.set_action(false);
                return Ok(());
            };
            //log::debug!(
            //    "ISM write track {} head {} {:?}",
            //    self.get_selected_drive().track,
            //    head,
            //    entry
            //);

            match entry {
                entry if self.ism_setup.tsm_disable() => {
                    // GCR, the bytes are written as they are
                    self.ism_write_shreg = u16::from(entry.inner()) << 8;
                    self.ism_write_shreg_cnt = 8;
                }
                IsmFifoEntry::Marker(data) => {
                    // Markers use pre-encoded sync pattern (0xA1 with missing clock)
                    // Reset CRC when marker is encountered
                    self.ism_crc = Self::ISM_CRC_INIT;

                    if data == 0xA1 {
                        // Default encoded sync marker
                        self.ism_write_shreg = Self::MFM_SYNC_MARKER;
                        // For sync marker, the last data bit is 1 (0xA1 = 10100001)
                        self.ism_write_prev_bit = true;
                    } else {
                        // Other markers are MFM encoded normally
                        let (encoded, prev) = Self::ism_mfm_encode(data, self.ism_write_prev_bit);
                        self.ism_write_shreg = encoded;
                        self.ism_write_prev_bit = prev;
                    }
                    self.ism_write_shreg_cnt = 16;
                }
                IsmFifoEntry::Data { value: data, .. } => {
                    self.ism_crc_update_byte(data);

                    let (encoded, prev) = Self::ism_mfm_encode(data, self.ism_write_prev_bit);
                    self.ism_write_shreg = encoded;
                    self.ism_write_prev_bit = prev;
                    self.ism_write_shreg_cnt = 16;
                }
                IsmFifoEntry::Crc => {
                    // Write CRC, high byte
                    let high_byte = (self.ism_crc >> 8) as u8;
                    let low_byte = self.ism_crc as u8;

                    // Queue low byte to be written next
                    self.ism_fifo.push_front(IsmFifoEntry::CrcLow(low_byte));

                    // Encode and push high byte to shift register for writing
                    let (encoded, prev) = Self::ism_mfm_encode(high_byte, self.ism_write_prev_bit);
                    self.ism_write_shreg = encoded;
                    self.ism_write_prev_bit = prev;
                    self.ism_write_shreg_cnt = 16;
                }
                IsmFifoEntry::CrcLow(byte) => {
                    // Write CRC low byte
                    let (encoded, prev) = Self::ism_mfm_encode(byte, self.ism_write_prev_bit);
                    self.ism_write_shreg = encoded;
                    self.ism_write_prev_bit = prev;
                    self.ism_write_shreg_cnt = 16;
                }
            }
        }

        // Write one bit from shift register (MSB first)
        let bit = (self.ism_write_shreg >> 15) & 1 != 0;
        self.get_selected_drive_mut().write_bit(head, bit);
        self.ism_write_shreg <<= 1;
        self.ism_write_shreg_cnt -= 1;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use snow_floppy::{FloppyImage, FloppyType};

    use super::*;
    use crate::bus::BusMember;
    use crate::mac::swim::drive::DriveType;
    use crate::tickable::Tickable;

    const REG_DATA: Address = 0;
    const REG_ERROR: Address = 2;
    const REG_PARAM: Address = 3;
    const REG_PHASES: Address = 4;
    const REG_SETUP: Address = 5;
    const REG_MODE_ZERO: Address = 6;
    const REG_MODE_ONE: Address = 7;
    const REG_STATUS: Address = 6;
    const REG_HANDSHAKE: Address = 7;

    const MODE_CLEAR_FIFO: u8 = 0x01;
    const MODE_DRIVE1: u8 = 0x02;
    const MODE_MOTOR: u8 = 0x80;
    const MODE_ACTION: u8 = 0x08;
    const MODE_WRITE: u8 = 0x10;

    const DRIVE_MOTOR_ON: u8 = 0b0100;

    /// CA0, CA1 and CA2 high, LSTRB low then high: eject with SEL low
    const PHASES_EJECT: [u8; 2] = [0xF7, 0xFF];

    fn reg(reg: Address) -> Address {
        reg << 9
    }

    /// SWIM in ISM mode, a disk spinning in the first drive
    fn swim(setup: u8) -> Swim {
        let mut swim = Swim::new(&[DriveType::SuperDrive], true, 16_000_000);
        swim.disk_insert(0, FloppyImage::new(FloppyType::Mac800K, ""))
            .unwrap();
        swim.drives[0].write_drive_reg(DRIVE_MOTOR_ON, 0);
        swim.drives[0].not_ready = 0;

        // Q6 on, then the sequence on the mode register
        swim.write(reg(13), 0);
        for val in [0x57, 0x17, 0x57, 0x57] {
            swim.write(reg(15), val);
        }
        assert_eq!(swim.mode, SwimMode::Ism);

        swim.write(reg(REG_SETUP), setup);
        swim.write(reg(REG_MODE_ONE), MODE_DRIVE1);
        swim
    }

    fn start(swim: &mut Swim, mode: u8) {
        swim.write(reg(REG_MODE_ZERO), MODE_WRITE | MODE_ACTION);
        swim.write(reg(REG_MODE_ONE), mode);
        swim.write(reg(REG_MODE_ONE), MODE_CLEAR_FIFO);
        swim.write(reg(REG_MODE_ZERO), MODE_CLEAR_FIFO);
        swim.read(reg(REG_ERROR));
    }

    fn tick_bit(swim: &mut Swim) {
        let ticks = swim.drives[0].get_ticks_per_bit();
        swim.tick(ticks, ()).unwrap();
    }

    #[test]
    fn gcr_write_read() {
        const DATA: [u8; 8] = [0xFF, 0xD5, 0xAA, 0x96, 0x9A, 0xB3, 0xDE, 0xAA];

        let mut swim = swim(0x44);
        let position = swim.drives[0].track_position;

        start(&mut swim, MODE_WRITE);
        let mut data = DATA.iter();
        swim.write(reg(REG_DATA), *data.next().unwrap());
        swim.write(reg(REG_MODE_ONE), MODE_ACTION);
        while swim.ism_mode.action() {
            if swim.read(reg(REG_HANDSHAKE)).unwrap() & 0x80 != 0
                && let Some(val) = data.next()
            {
                swim.write(reg(REG_DATA), *val);
            }
            tick_bit(&mut swim);
        }
        // The write ends when the FIFO runs empty
        assert_eq!(swim.read(reg(REG_ERROR)), Some(0x01));
        assert_eq!(swim.drives[0].track_position, position + DATA.len() * 8 + 1);

        swim.drives[0].track_position = position;
        start(&mut swim, 0);
        swim.write(reg(REG_MODE_ONE), MODE_ACTION);
        let mut read = vec![];
        while read.len() < DATA.len() {
            tick_bit(&mut swim);
            if swim.read(reg(REG_HANDSHAKE)).unwrap() & 0x80 != 0 {
                read.push(swim.read(reg(REG_DATA)).unwrap());
            }
        }
        assert_eq!(read, DATA);
        assert_eq!(swim.read(reg(REG_ERROR)), Some(0x00));
    }

    #[test]
    fn gcr_read_overrun() {
        let mut swim = swim(0x44);
        start(&mut swim, 0);
        swim.write(reg(REG_MODE_ONE), MODE_ACTION);
        // Nothing reads the FIFO
        for _ in 0..100 {
            tick_bit(&mut swim);
        }
        assert_eq!(swim.read(reg(REG_HANDSHAKE)).unwrap() & 0xE0, 0xE0);
        assert_eq!(swim.read(reg(REG_ERROR)), Some(0x01));
    }

    fn params(swim: &mut Swim, count: u8) -> Vec<u8> {
        swim.write(reg(REG_MODE_ZERO), 0);
        for val in 1..=count {
            swim.write(reg(REG_PARAM), val);
        }
        (0..count)
            .map(|_| swim.read(reg(REG_PARAM)).unwrap())
            .collect()
    }

    #[test]
    fn swim_params() {
        let mut swim = swim(0x20);
        assert_eq!(params(&mut swim, 16), (1..=16).collect::<Vec<_>>());
    }

    #[test]
    fn swim2_params() {
        let mut swim = Swim::new_swim2(&[DriveType::SuperDrive], 16_000_000);
        assert_eq!(params(&mut swim, 4), [1, 2, 3, 4]);
        // Wraps after four
        assert_eq!(params(&mut swim, 6), [3, 4, 5, 6, 3, 4]);
    }

    #[test]
    fn swim_to_iwm() {
        let mut swim = swim(0x20);
        swim.write(reg(REG_MODE_ZERO), 0xF8);
        assert_eq!(swim.mode, SwimMode::Iwm);
    }

    #[test]
    fn swim2_stays_ism() {
        let mut swim = Swim::new_swim2(&[DriveType::SuperDrive], 16_000_000);
        assert_eq!(swim.read(reg(REG_STATUS)), Some(0x40));
        swim.write(reg(REG_MODE_ONE), 0x17);
        assert_eq!(swim.read(reg(REG_STATUS)), Some(0x57));
        swim.write(reg(REG_MODE_ZERO), 0xF8);
        assert_eq!(swim.mode, SwimMode::Ism);
        assert_eq!(swim.read(reg(REG_STATUS)), Some(0x47));

        // The phase register still answers, which is how the ROM tells
        // the SWIM2 from the SWIM
        for val in [0xF5, 0xF6, 0xF7] {
            swim.write(reg(REG_PHASES), val);
            assert_eq!(swim.read(reg(REG_PHASES)), Some(val));
        }
    }

    #[test]
    fn write_no_disk() {
        const BYTE_TICKS: Ticks = 16 * 16;

        let mut swim = Swim::new_swim2(&[DriveType::SuperDrive], 16_000_000);
        swim.write(reg(REG_SETUP), 0x20);
        start(&mut swim, MODE_WRITE);
        swim.write(reg(REG_DATA), 0);
        swim.write(reg(REG_DATA), 0);
        swim.write(reg(REG_MODE_ONE), MODE_ACTION);
        assert_eq!(swim.read(reg(REG_HANDSHAKE)).unwrap() & 0xC0, 0x00);

        // Each byte makes room in the FIFO after its time
        for _ in 0..20 {
            swim.tick(BYTE_TICKS - 1, ()).unwrap();
            assert_eq!(swim.read(reg(REG_HANDSHAKE)).unwrap() & 0xC0, 0x00);
            swim.tick(1, ()).unwrap();
            assert_eq!(swim.read(reg(REG_HANDSHAKE)).unwrap() & 0xC0, 0x80);
            swim.write(reg(REG_DATA), 0);
        }

        // Then the FIFO runs empty, which ends the write
        swim.tick(2 * BYTE_TICKS, ()).unwrap();
        assert!(swim.ism_mode.action());
        assert_eq!(swim.read(reg(REG_HANDSHAKE)).unwrap() & 0xE0, 0xC0);
        swim.tick(BYTE_TICKS, ()).unwrap();
        assert!(!swim.ism_mode.action());
        assert_eq!(swim.read(reg(REG_ERROR)), Some(0x01));
    }

    #[test]
    fn strobe_no_drive_enabled() {
        let mut swim = swim(0x20);
        swim.write(reg(REG_MODE_ZERO), MODE_DRIVE1);

        for phases in PHASES_EJECT {
            swim.write(reg(REG_PHASES), phases);
        }
        swim.tick(1, ()).unwrap();
        assert!(swim.drives[0].floppy_inserted);

        swim.write(reg(REG_MODE_ONE), MODE_MOTOR | MODE_DRIVE1);
        for phases in PHASES_EJECT {
            swim.write(reg(REG_PHASES), phases);
        }
        swim.tick(1, ()).unwrap();
        assert!(!swim.drives[0].floppy_inserted);
    }
}
