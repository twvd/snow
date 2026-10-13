//! Egret and Cuda microcontrollers, packet level
//!
//! This is a high level model of what the firmware does with the packets
//! exchanged with the host. The transfer of the packets through the VIA
//! differs between the two and is left to the model of each.

use log::*;
use serde::{Deserialize, Serialize};

use crate::mac::rtc::Rtc;
use crate::tickable::Ticks;

/// ADB command packet
pub const PACKET_ADB: u8 = 0x00;
/// Pseudo command packet
pub const PACKET_PSEUDO: u8 = 0x01;
/// Error response packet
pub const PACKET_ERROR: u8 = 0x02;

/// Error codes of the error response packet
const ERROR_PACKET_TYPE: u8 = 1;
const ERROR_PSEUDO_CMD: u8 = 2;
const ERROR_PARAMS: u8 = 3;
const ERROR_ADDRESS: u8 = 4;

/// Pseudo commands
const CMD_AUTOPOLL: u8 = 0x01;
const CMD_READ_RAM: u8 = 0x02;
const CMD_READ_CLOCK: u8 = 0x03;
const CMD_READ_PRAM: u8 = 0x07;
const CMD_WRITE_RAM: u8 = 0x08;
const CMD_SET_CLOCK: u8 = 0x09;
const CMD_WRITE_PRAM: u8 = 0x0C;
const CMD_WRITE_DFAC: u8 = 0x0E;
const CMD_ONE_SEC_MODE: u8 = 0x1B;
const CMD_KEYBOARD_NMI: u8 = 0x1C;

/// PRAM has 8 bytes more than the 256 of the clock chip of older models
const PRAM_EXTRA: usize = 8;
const PRAM_SIZE: u16 = 256 + PRAM_EXTRA as u16;

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
    /// ADB devices are polled without a command from the host
    autopoll: bool,

    /// Mode of the one second tick packets (0 = none)
    one_sec_mode: u8,

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
    pub fn tick(&mut self, _ticks: Ticks) {}

    /// Returns a packet to send to the host without a command from it.
    /// There is no such packet yet.
    pub fn packet(&mut self) -> Option<Vec<u8>> {
        None
    }

    /// Handles a command packet from the host and returns the response packet,
    /// without the attention byte which precedes it.
    pub fn command(&mut self, packet: &[u8]) -> Vec<u8> {
        trace!("Command: {:02X?}", packet);

        self.reading = None;

        let response = match packet {
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
                Self::params(params).map(|[]| self.rtc.seconds().to_be_bytes().to_vec())
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
            CMD_ONE_SEC_MODE => Self::params(params).map(|[mode]| {
                self.one_sec_mode = mode;
                vec![]
            }),
            CMD_KEYBOARD_NMI => Self::params(params).map(|[enable]| {
                self.keyboard_nmi = enable != 0;
                vec![]
            }),
            _ => {
                warn!("Unimplemented pseudo command {:02X} {:02X?}", cmd, params);
                Err(ERROR_PSEUDO_CMD)
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
    fn errors() {
        let mut egret = EgretCommon::default();
        assert_eq!(egret.command(&[0x07, 0x12]), [0x02, 0x01, 0x07, 0x12]);
        assert_eq!(egret.command(&[0x01, 0xFE]), [0x02, 0x02, 0x01, 0xFE]);
        assert_eq!(egret.command(&[0x01, 0x01]), [0x02, 0x03, 0x01, 0x01]);
    }
}
