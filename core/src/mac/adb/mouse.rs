use super::{AdbDevice, AdbDeviceResponse};

use crossbeam_channel::{Receiver, Sender};
use log::*;

pub type AdbMouseSender = Sender<bool>;

/// Apple Desktop Bus-connected mouse
pub struct AdbMouse {
    button: bool,
    button_recv: Receiver<bool>,
}

impl AdbMouse {
    pub const ADDRESS: usize = 3;

    pub fn new() -> (Self, AdbMouseSender) {
        let (s, button_recv) = crossbeam_channel::unbounded();
        (
            Self {
                button: false,
                button_recv,
            },
            s,
        )
    }
}

impl AdbDevice for AdbMouse {
    fn reset(&mut self) {}

    fn flush(&mut self) {
        trace!("mouse flush");
    }

    fn talk(&mut self, reg: u8) -> AdbDeviceResponse {
        trace!("mouse talk: {:02X}", reg);

        while !self.button_recv.is_empty() {
            self.button = self.button_recv.recv().unwrap();
        }

        match reg {
            0 => {
                AdbDeviceResponse::from_iter([if self.button { 0x00_u8 } else { 0x80_u8 }, 0x00_u8])
            }
            3 => AdbDeviceResponse::from_iter([0x03_u8, 0x01_u8]),
            _ => AdbDeviceResponse::default(),
        }
    }

    fn listen(&mut self, reg: u8) -> AdbDeviceResponse {
        trace!("mouse listen: {:02X}", reg);
        AdbDeviceResponse::default()
    }
}
