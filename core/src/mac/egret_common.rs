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
/// One second tick packet
pub const PACKET_TICK: u8 = 0x03;

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

/// Frequency of the VIA clock, which the model is run from. The Egret has
/// its own 32.768 kHz crystal.
pub const TICKS_PER_SECOND: Ticks = 783_360;

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
            return None;
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
            [PACKET_PSEUDO, cmd, params @ ..] => self.egret_command(*cmd, params),
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

    fn egret_command(&mut self, cmd: u8, params: &[u8]) -> Vec<u8> {
        let result = match cmd {
            // Start or stop polling the ADB devices
            CMD_AUTOPOLL => Self::params(params).map(|[enable]| {
                self.autopoll = enable != 0;
                vec![]
            }),

            // Read or write the memory of the microcontroller. Only the PRAM is
            // emulated, at the addresses where the firmware keeps it ($0100-$01FF
            // and $00D7-$00DE). Other addresses read 0 and ignore writes.
            CMD_READ_RAM => Self::params(params).map(|addr| {
                self.reading = Some(Reading::Ram(u16::from_be_bytes(addr)));
                vec![]
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

            // Access the Real Time Clock
            CMD_READ_CLOCK => {
                return Self::params(params).map_or_else(
                    |code| Self::error(code, PACKET_PSEUDO, cmd),
                    |[]| self.read_clock(),
                );
            }
            CMD_SET_CLOCK => Self::params(params).map(|seconds| {
                self.rtc.set_seconds(u32::from_be_bytes(seconds));
                vec![]
            }),

            // Access the parameter RAM
            CMD_READ_PRAM => Self::params(params).and_then(|addr| {
                self.reading = Some(Reading::Pram(Self::pram_addr(addr)?));
                Ok(vec![])
            }),
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

            // Set up the sound input circuit (not modelled)
            CMD_WRITE_DFAC if (1..=4).contains(&params.len()) => Ok(vec![]),
            CMD_WRITE_DFAC => Err(ERROR_PARAMS),

            // Choose the packet sent every second, if any
            CMD_ONE_SEC_MODE => Self::params(params).map(|[mode]| {
                self.one_sec_mode = if mode < 4 { mode } else { 0 };
                self.tick_time = self.one_sec_mode != 0;
                vec![]
            }),

            // Allow the programmer's key combination to raise an NMI (not modelled)
            CMD_KEYBOARD_NMI => Self::params(params).map(|[enable]| {
                self.keyboard_nmi = enable != 0;
                vec![]
            }),

            // Unknown or unimplemented command
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
    fn errors() {
        let mut egret = EgretCommon::default();
        assert_eq!(egret.command(&[0x07, 0x12]), [0x02, 0x01, 0x07, 0x12]);
        assert_eq!(egret.command(&[0x01, 0xFE]), [0x02, 0x02, 0x01, 0xFE]);
        assert_eq!(egret.command(&[0x01, 0x01]), [0x02, 0x03, 0x01, 0x01]);
    }
}
