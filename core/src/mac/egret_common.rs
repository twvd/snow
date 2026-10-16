//! Egret and Cuda microcontrollers, packet level
//!
//! This is a high level model of what the firmware does with the packets
//! exchanged with the host. The transfer of the packets through the VIA
//! differs between the two and is left to the model of each.

use log::*;
use serde::{Deserialize, Serialize};

use crate::mac::adb::{AdbDevice, AdbDeviceInstance, AdbEvent};
use crate::mac::rtc::Rtc;
use crate::tickable::Ticks;

/// ADB command packet
pub const PACKET_ADB: u8 = 0x00;
/// Pseudo command packet
pub const PACKET_PSEUDO: u8 = 0x01;
/// Error response packet
pub const PACKET_ERROR: u8 = 0x02;
/// One second tick packet
pub const PACKET_TICK: u8 = 0x03;

/// Error codes of the error response packet
const ERROR_PACKET_TYPE: u8 = 1;
const ERROR_COMMAND: u8 = 2;
const ERROR_PARAMS: u8 = 3;
const ERROR_ADDRESS: u8 = 4;

/// Flags of the ADB response packet
const ADB_FLAG_SRQ: u8 = 1 << 0;
const ADB_FLAG_NO_DATA: u8 = 1 << 1;
const ADB_FLAG_AUTOPOLL: u8 = 1 << 6;

/// Pseudo commands
const CMD_AUTOPOLL: u8 = 0x01;
const CMD_READ_RAM: u8 = 0x02;
const CMD_READ_CLOCK: u8 = 0x03;
const CMD_READ_PRAM: u8 = 0x07;
const CMD_WRITE_RAM: u8 = 0x08;
const CMD_SET_CLOCK: u8 = 0x09;
const CMD_WRITE_PRAM: u8 = 0x0C;
const CMD_WRITE_DFAC: u8 = 0x0E;
const CMD_SET_DEVICE_LIST: u8 = 0x19;
const CMD_ONE_SEC_MODE: u8 = 0x1B;
const CMD_KEYBOARD_NMI: u8 = 0x1C;

/// Frequency of the VIA clock, which the model is run from. The Egret has
/// its own 32.768 kHz crystal.
pub const TICKS_PER_SECOND: Ticks = 783_360;

/// Time between polls of the ADB devices. The rate is a setting of the
/// Egret, of which the default value has not been verified.
const AUTOPOLL_TICKS: Ticks = TICKS_PER_SECOND * 11 / 1000;

/// PRAM has 8 bytes more than the 256 of the clock chip of older models
const PRAM_EXTRA: usize = 8;
const PRAM_SIZE: u16 = 256 + PRAM_EXTRA as u16;

/// Addresses of the two ADB devices polled first, in the memory of the
/// microcontroller. The order of polling is not modelled.
const RAM_ADB_POLL_FIRST: u16 = 0x00B3;
const RAM_ADB_POLL_SECOND: u16 = 0x00B4;

/// Location of PRAM in the memory of the microcontroller
const RAM_PRAM: u16 = 0x0100;
const RAM_PRAM_EXTRA: u16 = 0x00D7;

/// Read which goes on until the host ends it, with the next address
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Reading {
    Ram(u16),
    Pram(u16),
}

#[derive(Default, Serialize, Deserialize)]
pub struct EgretCommon {
    adb_devices: Vec<AdbDeviceInstance>,

    /// ADB devices are polled without a command from the host
    autopoll: bool,

    /// Time since the last poll
    autopoll_ticks: Ticks,

    /// Addresses of the ADB devices to poll (bit 0 = address 0)
    adb_device_list: u16,

    /// Mode of the one second tick packets (0 = none)
    one_sec_mode: u8,

    /// Time since the clock advanced
    second_ticks: Ticks,

    /// A second has passed, for which no tick packet was sent
    tick_pending: bool,

    /// The next tick packet has the time, whatever the mode
    tick_time: bool,

    /// The programmer's key combination on the keyboard raises an NMI
    keyboard_nmi: bool,

    /// Clock and the first 256 bytes of PRAM. Only the storage is used.
    pub(crate) rtc: Rtc,

    /// Last bytes of PRAM, which are not saved to the PRAM file
    pram_extra: [u8; PRAM_EXTRA],

    /// Read in progress
    reading: Option<Reading>,
}

impl EgretCommon {
    /// Advances by a number of VIA clock cycles
    pub fn tick(&mut self, ticks: Ticks) {
        self.autopoll_ticks += ticks;
        self.second_ticks += ticks;
        if self.second_ticks >= TICKS_PER_SECOND {
            self.second_ticks -= TICKS_PER_SECOND;
            self.rtc.second();

            // The host gets the time again after it has missed a tick
            self.tick_time |= self.tick_pending;
            self.tick_pending = true;
        }
    }

    /// Returns a packet to send to the host without a command from it
    pub fn packet(&mut self) -> Option<Vec<u8>> {
        // The host has ended the response to its last command, which may be
        // one that goes on until then
        self.reading = None;

        if self.one_sec_mode == 0 || !self.tick_pending {
            return self.adb_autopoll();
        }
        self.tick_pending = false;

        let packet = match self.one_sec_mode {
            _ if self.tick_time => self.read_clock(),
            1 => self.read_clock(),
            2 => vec![PACKET_PSEUDO, 0, CMD_READ_CLOCK],
            _ => vec![PACKET_TICK],
        };
        trace!("Tick: {:02X?}", packet);
        Some(packet)
    }

    pub fn adb_add_device<T>(&mut self, device: T)
    where
        T: AdbDevice + Send + 'static,
    {
        self.adb_devices.push(Box::new(device));
    }

    pub fn adb_event(&mut self, event: &AdbEvent) {
        for d in &mut self.adb_devices {
            d.event(event);
        }
    }

    /// A device other than the one at the address requests service
    fn adb_srq(&self, address: u8) -> bool {
        self.adb_devices
            .iter()
            .any(|d| d.get_address() != address && d.get_srq())
    }

    /// Handles an ADB command from the host. The ADB bus is not modelled:
    /// the transaction with the device takes no time.
    fn adb_command(&mut self, cmd: u8, data: &[u8]) -> Vec<u8> {
        let address = cmd >> 4;
        let reg = cmd & 3;
        let mut device = self
            .adb_devices
            .iter_mut()
            .find(|d| d.get_address() == address);

        match (cmd & 0x0F, data.len()) {
            // Reset of all the devices
            (0x00, 0) => {
                for d in &mut self.adb_devices {
                    d.reset();
                }
            }
            // Flush
            (0x01, 0) => {
                if let Some(d) = &mut device {
                    d.flush();
                }
            }
            // Listen
            (0x08..=0x0B, 2..=8) => {
                if let Some(d) = &mut device {
                    d.listen(reg, data);
                }
            }
            // Talk
            (0x0C..=0x0F, 0) => {
                let response = device.map(|d| d.talk(reg)).unwrap_or_default();
                let mut flags = 0;
                if response.is_empty() {
                    flags |= ADB_FLAG_NO_DATA;
                }
                if self.adb_srq(address) {
                    flags |= ADB_FLAG_SRQ;
                }
                return [&[PACKET_ADB, flags, cmd], response.as_slice()].concat();
            }
            (0x02..=0x07, _) => return Self::error(ERROR_COMMAND, PACKET_ADB, cmd),
            _ => return Self::error(ERROR_PARAMS, PACKET_ADB, cmd),
        }
        vec![PACKET_ADB, 0, cmd]
    }

    /// Polls the ADB devices and returns a packet with the data of one.
    ///
    /// The Egret polls the last device which had data and those which
    /// request service. Here the devices are only polled when they have data.
    fn adb_autopoll(&mut self) -> Option<Vec<u8>> {
        if !self.autopoll || self.autopoll_ticks < AUTOPOLL_TICKS {
            return None;
        }
        self.autopoll_ticks = 0;

        let list = self.adb_device_list;
        let device = self
            .adb_devices
            .iter_mut()
            .find(|d| d.get_srq() && list & (1 << d.get_address()) != 0)?;
        let address = device.get_address();
        let response = device.talk(0);
        if response.is_empty() {
            return None;
        }

        let mut flags = ADB_FLAG_AUTOPOLL;
        if self.adb_srq(address) {
            flags |= ADB_FLAG_SRQ;
        }
        let packet = [
            &[PACKET_ADB, flags, address << 4 | 0x0C],
            response.as_slice(),
        ]
        .concat();
        trace!("Autopoll: {:02X?}", packet);
        Some(packet)
    }

    /// Response to the read clock command, also used for tick packets
    fn read_clock(&mut self) -> Vec<u8> {
        self.tick_time = false;
        [
            &[PACKET_PSEUDO, 0, CMD_READ_CLOCK],
            self.rtc.seconds().to_be_bytes().as_slice(),
        ]
        .concat()
    }

    /// Handles a command packet from the host and returns the response packet,
    /// without the attention byte which precedes it.
    pub fn command(&mut self, packet: &[u8]) -> Vec<u8> {
        trace!("Command: {:02X?}", packet);

        self.reading = None;

        let response = match packet {
            [PACKET_ADB, cmd, data @ ..] => self.adb_command(*cmd, data),
            [PACKET_PSEUDO, cmd, params @ ..] => self.pseudo_command(*cmd, params),
            [packet_type, cmd, ..] => Self::error(ERROR_PACKET_TYPE, *packet_type, *cmd),
            _ => {
                warn!("Short packet: {:02X?}", packet);
                Self::error(ERROR_PACKET_TYPE, *packet.first().unwrap_or(&0), 0)
            }
        };

        if response[0] == PACKET_ERROR {
            debug!("Command {:02X?} failed: {:02X?}", packet, response);
        } else {
            trace!("Response: {:02X?}", response);
        }
        response
    }

    /// The response goes on after the packet, until the host ends the transfer
    pub fn response_continues(&self) -> bool {
        self.reading.is_some()
    }

    /// Returns the next byte of a response which goes on after the packet
    pub fn response_next(&mut self) -> Option<u8> {
        Some(match self.reading? {
            Reading::Ram(addr) => {
                self.reading = Some(Reading::Ram(addr.wrapping_add(1)));
                self.ram_read(addr)
            }
            Reading::Pram(addr) => {
                self.reading = Some(Reading::Pram((addr + 1) % PRAM_SIZE));
                self.pram_read(addr)
            }
        })
    }

    /// Reads the memory of the microcontroller, of which only PRAM is modelled
    fn ram_read(&self, addr: u16) -> u8 {
        if let Some(addr) = Self::ram_to_pram(addr) {
            self.pram_read(addr)
        } else {
            warn!("Read of unimplemented memory address {:04X}", addr);
            0
        }
    }

    fn ram_write(&mut self, addr: u16, val: u8) {
        if let Some(addr) = Self::ram_to_pram(addr) {
            self.pram_write(addr, val);
        } else if matches!(addr, RAM_ADB_POLL_FIRST | RAM_ADB_POLL_SECOND) {
            // Written by the host to work around a defect of the firmware
        } else {
            warn!(
                "Write to unimplemented memory address {:04X}: {:02X}",
                addr, val
            );
        }
    }

    fn ram_to_pram(addr: u16) -> Option<u16> {
        const EXTRA_END: u16 = RAM_PRAM_EXTRA + PRAM_EXTRA as u16;
        match addr {
            RAM_PRAM..=0x01FF => Some(addr - RAM_PRAM),
            RAM_PRAM_EXTRA..EXTRA_END => Some(addr - RAM_PRAM_EXTRA + 256),
            _ => None,
        }
    }

    fn pram_read(&self, addr: u16) -> u8 {
        match u8::try_from(addr) {
            Ok(addr) => self.rtc.pram_read(addr),
            Err(_) => self.pram_extra[usize::from(addr) - 256],
        }
    }

    fn pram_addr(addr: [u8; 2]) -> Result<u16, u8> {
        let addr = u16::from_be_bytes(addr);
        if addr < PRAM_SIZE {
            Ok(addr)
        } else {
            Err(ERROR_ADDRESS)
        }
    }

    fn pram_write(&mut self, addr: u16, val: u8) {
        match u8::try_from(addr) {
            Ok(addr) => self.rtc.pram_write(addr, val),
            Err(_) => self.pram_extra[usize::from(addr) - 256] = val,
        }
    }

    fn error(code: u8, packet_type: u8, cmd: u8) -> Vec<u8> {
        vec![PACKET_ERROR, code, packet_type, cmd]
    }

    fn pseudo_command(&mut self, cmd: u8, params: &[u8]) -> Vec<u8> {
        let result = match cmd {
            CMD_AUTOPOLL => Self::params(params).map(|[enable]| {
                self.autopoll = enable != 0;
                vec![]
            }),
            CMD_READ_RAM => Self::params(params).map(|addr| {
                self.reading = Some(Reading::Ram(u16::from_be_bytes(addr)));
                vec![]
            }),
            CMD_READ_CLOCK => {
                return Self::params(params).map_or_else(
                    |code| Self::error(code, PACKET_PSEUDO, cmd),
                    |[]| self.read_clock(),
                );
            }
            CMD_READ_PRAM => Self::params(params).and_then(|addr| {
                self.reading = Some(Reading::Pram(Self::pram_addr(addr)?));
                Ok(vec![])
            }),
            CMD_WRITE_RAM => match *params {
                [hi, mut lo, ref data @ ..] => {
                    // The address stays in the same page
                    for &val in data {
                        self.ram_write(u16::from_be_bytes([hi, lo]), val);
                        lo = lo.wrapping_add(1);
                    }
                    Ok(vec![])
                }
                _ => Err(ERROR_PARAMS),
            },
            CMD_WRITE_PRAM => match *params {
                [hi, lo, ref data @ ..] => Self::pram_addr([hi, lo]).map(|mut addr| {
                    for &val in data {
                        self.pram_write(addr, val);
                        addr = (addr + 1) % PRAM_SIZE;
                    }
                    vec![]
                }),
                _ => Err(ERROR_PARAMS),
            },
            CMD_SET_CLOCK => Self::params(params).map(|seconds| {
                self.rtc.set_seconds(u32::from_be_bytes(seconds));
                vec![]
            }),
            // Settings of the sound input circuit, which is not modelled
            CMD_WRITE_DFAC if (1..=4).contains(&params.len()) => Ok(vec![]),
            CMD_WRITE_DFAC => Err(ERROR_PARAMS),
            CMD_SET_DEVICE_LIST => Self::params(params).map(|list| {
                self.adb_device_list = u16::from_be_bytes(list);
                vec![]
            }),
            CMD_ONE_SEC_MODE => Self::params(params).map(|[mode]| {
                self.one_sec_mode = if mode < 4 { mode } else { 0 };
                self.tick_time = self.one_sec_mode != 0;
                vec![]
            }),
            CMD_KEYBOARD_NMI => Self::params(params).map(|[enable]| {
                self.keyboard_nmi = enable != 0;
                vec![]
            }),
            _ => {
                warn!("Unimplemented pseudo command {:02X} {:02X?}", cmd, params);
                Err(ERROR_COMMAND)
            }
        };

        match result {
            Ok(data) => [&[PACKET_PSEUDO, 0, cmd], data.as_slice()].concat(),
            Err(code) => Self::error(code, PACKET_PSEUDO, cmd),
        }
    }

    /// Checks the number of parameters of a command
    fn params<const N: usize>(params: &[u8]) -> Result<[u8; N], u8> {
        params.try_into().map_err(|_| ERROR_PARAMS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::adb::{AdbKeyboard, AdbMouse};
    use crate::types::MouseEvent;

    #[test]
    fn autopoll() {
        let mut egret = EgretCommon::default();
        assert_eq!(egret.command(&[0x01, 0x01, 0x01]), [0x01, 0x00, 0x01]);
        assert!(egret.autopoll);
        assert_eq!(egret.command(&[0x01, 0x01, 0x00]), [0x01, 0x00, 0x01]);
        assert!(!egret.autopoll);
    }

    #[test]
    fn write_dfac() {
        let mut egret = EgretCommon::default();
        assert_eq!(egret.command(&[0x01, 0x0E, 0x57]), [0x01, 0x00, 0x0E]);
        assert_eq!(egret.command(&[0x01, 0x0E]), [0x02, 0x03, 0x01, 0x0E]);
    }

    #[test]
    fn read_pram() {
        let mut egret = EgretCommon::default();
        egret.pram_extra[0] = 0x55;
        assert!(!egret.response_continues());
        assert_eq!(egret.response_next(), None);

        assert_eq!(egret.command(&[0x01, 0x07, 0x00, 0xFF]), [0x01, 0x00, 0x07]);
        assert!(egret.response_continues());
        assert_eq!(egret.response_next(), Some(0x00));
        assert_eq!(egret.response_next(), Some(0x55));

        // The address wraps around after the last byte
        assert_eq!(egret.command(&[0x01, 0x07, 0x01, 0x07]), [0x01, 0x00, 0x07]);
        assert_eq!(egret.response_next(), Some(0x00));
        assert_eq!(egret.reading, Some(Reading::Pram(0)));

        assert_eq!(
            egret.command(&[0x01, 0x07, 0x01, 0x08]),
            [0x02, 0x04, 0x01, 0x07]
        );
        assert_eq!(egret.response_next(), None);
        assert_eq!(egret.command(&[0x01, 0x07, 0x00]), [0x02, 0x03, 0x01, 0x07]);

        // Another command ends the read
        egret.command(&[0x01, 0x07, 0x00, 0x00]);
        egret.command(&[0x01, 0x01, 0x00]);
        assert_eq!(egret.response_next(), None);
    }

    #[test]
    fn write_pram() {
        let mut egret = EgretCommon::default();

        assert_eq!(
            egret.command(&[0x01, 0x0C, 0x00, 0xF9, 0x12, 0x34]),
            [0x01, 0x00, 0x0C]
        );
        assert_eq!(egret.rtc.pram_read(0xF9), 0x12);
        assert_eq!(egret.rtc.pram_read(0xFA), 0x34);

        // The address wraps around after the last byte
        assert_eq!(
            egret.command(&[0x01, 0x0C, 0x01, 0x06, 0x56, 0x78, 0x9A]),
            [0x01, 0x00, 0x0C]
        );
        assert_eq!(egret.pram_extra, [0, 0, 0, 0, 0, 0, 0x56, 0x78]);
        assert_eq!(egret.rtc.pram_read(0x00), 0x9A);

        // No data is not an error
        assert_eq!(egret.command(&[0x01, 0x0C, 0x00, 0x10]), [0x01, 0x00, 0x0C]);
        assert_eq!(
            egret.command(&[0x01, 0x0C, 0x01, 0x08, 0xFF]),
            [0x02, 0x04, 0x01, 0x0C]
        );
        assert_eq!(egret.command(&[0x01, 0x0C, 0x00]), [0x02, 0x03, 0x01, 0x0C]);

        egret.command(&[0x01, 0x07, 0x00, 0xF9]);
        assert_eq!(egret.response_next(), Some(0x12));
    }

    #[test]
    fn read_write_ram() {
        let mut egret = EgretCommon::default();

        assert_eq!(
            egret.command(&[0x01, 0x08, 0x01, 0xFF, 0x12, 0x34]),
            [0x01, 0x00, 0x08]
        );
        assert_eq!(egret.rtc.pram_read(0xFF), 0x12);
        assert_eq!(egret.rtc.pram_read(0x00), 0x34);

        assert_eq!(
            egret.command(&[0x01, 0x08, 0x00, 0xDE, 0x56, 0x78]),
            [0x01, 0x00, 0x08]
        );
        assert_eq!(egret.pram_extra[7], 0x56);
        assert_eq!(egret.command(&[0x01, 0x08, 0x01]), [0x02, 0x03, 0x01, 0x08]);

        assert_eq!(egret.command(&[0x01, 0x02, 0x00, 0xFF]), [0x01, 0x00, 0x02]);
        assert_eq!(egret.response_next(), Some(0x00));
        assert_eq!(egret.response_next(), Some(0x34));
        assert_eq!(egret.command(&[0x01, 0x02, 0x01, 0xFF]), [0x01, 0x00, 0x02]);
        assert_eq!(egret.response_next(), Some(0x12));
        assert_eq!(egret.response_next(), Some(0x00));
        assert_eq!(egret.command(&[0x01, 0x02, 0x00]), [0x02, 0x03, 0x01, 0x02]);
    }

    #[test]
    fn clock() {
        let mut egret = EgretCommon::default();

        assert_eq!(
            egret.command(&[0x01, 0x09, 0x12, 0x34, 0x56, 0x78]),
            [0x01, 0x00, 0x09]
        );
        assert_eq!(egret.rtc.seconds(), 0x12345678);
        assert_eq!(
            egret.command(&[0x01, 0x03]),
            [0x01, 0x00, 0x03, 0x12, 0x34, 0x56, 0x78]
        );
        assert_eq!(egret.command(&[0x01, 0x03, 0x00]), [0x02, 0x03, 0x01, 0x03]);
        assert_eq!(egret.command(&[0x01, 0x09, 0x00]), [0x02, 0x03, 0x01, 0x09]);
    }

    #[test]
    fn one_sec_ticks() {
        let mut egret = EgretCommon::default();
        egret.command(&[0x01, 0x09, 0x00, 0x00, 0x00, 0xFE]);

        // The clock advances without tick packets
        egret.tick(TICKS_PER_SECOND - 1);
        assert_eq!(egret.rtc.seconds(), 0xFE);
        egret.tick(1);
        assert_eq!(egret.rtc.seconds(), 0xFF);
        assert_eq!(egret.packet(), None);

        // The first packet has the time, in all modes
        assert_eq!(egret.command(&[0x01, 0x1B, 0x03]), [0x01, 0x00, 0x1B]);
        assert_eq!(
            egret.packet(),
            Some(vec![0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0xFF])
        );
        assert_eq!(egret.packet(), None);
        egret.tick(TICKS_PER_SECOND);
        assert_eq!(egret.packet(), Some(vec![0x03]));

        // As does the packet which follows a missed tick
        egret.tick(2 * TICKS_PER_SECOND - 1);
        assert_eq!(egret.packet(), Some(vec![0x03]));
        egret.tick(1);
        egret.tick(TICKS_PER_SECOND);
        assert_eq!(
            egret.packet(),
            Some(vec![0x01, 0x00, 0x03, 0x00, 0x00, 0x01, 0x03])
        );

        egret.command(&[0x01, 0x1B, 0x02]);
        egret.command(&[0x01, 0x03]);
        egret.tick(TICKS_PER_SECOND);
        assert_eq!(egret.packet(), Some(vec![0x01, 0x00, 0x03]));

        egret.command(&[0x01, 0x1B, 0x01]);
        egret.command(&[0x01, 0x03]);
        egret.tick(TICKS_PER_SECOND);
        assert_eq!(
            egret.packet(),
            Some(vec![0x01, 0x00, 0x03, 0x00, 0x00, 0x01, 0x05])
        );

        egret.command(&[0x01, 0x1B, 0x04]);
        egret.tick(TICKS_PER_SECOND);
        assert_eq!(egret.packet(), None);

        // A read which the host has ended does not go on after a tick packet
        egret.command(&[0x01, 0x1B, 0x03]);
        egret.command(&[0x01, 0x07, 0x00, 0x00]);
        assert!(egret.response_continues());
        egret.tick(TICKS_PER_SECOND);
        assert!(egret.packet().is_some());
        assert!(!egret.response_continues());
    }

    #[test]
    fn adb_commands() {
        let mut egret = EgretCommon::default();
        egret.adb_add_device(AdbMouse::new());
        egret.adb_add_device(AdbKeyboard::new());

        // Talk register 3 of the mouse, then of an address without a device
        let response = egret.command(&[0x00, 0x3F]);
        assert_eq!(response[..3], [0x00, 0x00, 0x3F]);
        assert_eq!(response.len(), 5);
        assert_eq!(egret.command(&[0x00, 0x4F]), [0x00, 0x02, 0x4F]);

        // Talk register 0 without data, with the mouse requesting service
        egret.adb_event(&AdbEvent::Mouse(MouseEvent {
            button: Some(true),
            rel_movement: None,
        }));
        assert_eq!(egret.command(&[0x00, 0x2C]), [0x00, 0x03, 0x2C]);
        assert_eq!(egret.command(&[0x00, 0x3C])[..3], [0x00, 0x00, 0x3C]);

        // Listen register 3 moves the mouse to address 4
        assert_eq!(egret.command(&[0x00, 0x3B, 0x04, 0xFE]), [0x00, 0x00, 0x3B]);
        assert_eq!(egret.command(&[0x00, 0x4F]).len(), 5);
        // Reset moves it back
        assert_eq!(egret.command(&[0x00, 0x00]), [0x00, 0x00, 0x00]);
        assert_eq!(egret.command(&[0x00, 0x3F]).len(), 5);
        assert_eq!(egret.command(&[0x00, 0x31]), [0x00, 0x00, 0x31]);

        assert_eq!(egret.command(&[0x00, 0x32]), [0x02, 0x02, 0x00, 0x32]);
        assert_eq!(egret.command(&[0x00, 0x3F, 0x00]), [0x02, 0x03, 0x00, 0x3F]);
        assert_eq!(egret.command(&[0x00, 0x3B, 0x04]), [0x02, 0x03, 0x00, 0x3B]);
    }

    #[test]
    fn adb_autopoll() {
        let mut egret = EgretCommon::default();
        egret.adb_add_device(AdbMouse::new());
        let click = AdbEvent::Mouse(MouseEvent {
            button: Some(true),
            rel_movement: None,
        });

        // Nothing without autopoll, nor for a device which is not in the list
        egret.adb_event(&click);
        egret.tick(AUTOPOLL_TICKS);
        assert_eq!(egret.packet(), None);
        assert_eq!(egret.command(&[0x01, 0x01, 0x01]), [0x01, 0x00, 0x01]);
        egret.tick(AUTOPOLL_TICKS);
        assert_eq!(egret.packet(), None);

        assert_eq!(egret.command(&[0x01, 0x19, 0x00, 0x08]), [0x01, 0x00, 0x19]);
        egret.tick(AUTOPOLL_TICKS - 1);
        assert_eq!(egret.packet(), None);
        egret.tick(1);
        let packet = egret.packet().unwrap();
        assert_eq!(packet[..3], [0x00, 0x40, 0x3C]);
        assert_eq!(packet.len(), 5);

        // No packet when the device has no data
        egret.tick(AUTOPOLL_TICKS);
        assert_eq!(egret.packet(), None);
    }

    #[test]
    fn errors() {
        let mut egret = EgretCommon::default();
        assert_eq!(egret.command(&[0x07, 0x12]), [0x02, 0x01, 0x07, 0x12]);
        assert_eq!(egret.command(&[0x01, 0xFE]), [0x02, 0x02, 0x01, 0xFE]);
        assert_eq!(egret.command(&[0x01, 0x01]), [0x02, 0x03, 0x01, 0x01]);
    }
}
