//! Egret and Cuda microcontrollers, packet level
//!
//! This is a high level model of what the firmware does with the packets
//! exchanged with the host. The transfer of the packets through the VIA
//! differs between the two and is left to the model of each.

use log::*;
use serde::{Deserialize, Serialize};

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

/// Pseudo commands
const CMD_AUTOPOLL: u8 = 0x01;
const CMD_WRITE_DFAC: u8 = 0x0E;
const CMD_ONE_SEC_MODE: u8 = 0x1B;
const CMD_KEYBOARD_NMI: u8 = 0x1C;

#[derive(Default, Serialize, Deserialize)]
pub struct EgretCommon {
    /// ADB devices are polled without a command from the host
    autopoll: bool,

    /// Mode of the one second tick packets (0 = none)
    one_sec_mode: u8,

    /// The programmer's key combination on the keyboard raises an NMI
    keyboard_nmi: bool,
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

    /// The response goes on after the packet, until the host ends the transfer.
    /// No command has such a response yet.
    pub fn response_continues(&self) -> bool {
        false
    }

    /// Returns the next byte of a response which goes on after the packet
    pub fn response_next(&mut self) -> Option<u8> {
        None
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
    fn errors() {
        let mut egret = EgretCommon::default();
        assert_eq!(egret.command(&[0x07, 0x12]), [0x02, 0x01, 0x07, 0x12]);
        assert_eq!(egret.command(&[0x01, 0xFE]), [0x02, 0x02, 0x01, 0xFE]);
        assert_eq!(egret.command(&[0x01, 0x01]), [0x02, 0x03, 0x01, 0x01]);
    }
}
