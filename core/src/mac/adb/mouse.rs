use crate::types::{ClickEventReceiver, ClickEventSender};

use super::{AdbDevice, AdbDeviceResponse, AdbReg3};

use log::*;

/// Apple Desktop Bus-connected mouse
pub struct AdbMouse {
    button_recv: ClickEventReceiver,
}

impl AdbMouse {
    pub const ADDRESS: usize = 3;

    pub fn new() -> (Self, ClickEventSender) {
        let (s, button_recv) = crossbeam_channel::unbounded();
        (Self { button_recv }, s)
    }
}

impl AdbDevice for AdbMouse {
    fn reset(&mut self) {
        self.flush();
    }

    fn flush(&mut self) {
        while !self.button_recv.is_empty() {
            let _ = self.button_recv.recv();
        }
    }

    fn talk(&mut self, reg: u8) -> AdbDeviceResponse {
        match reg {
            0 => {
                if !self.button_recv.is_empty() {
                    let mut button = false;
                    while !self.button_recv.is_empty() {
                        button = self.button_recv.recv().unwrap();
                    }
                    AdbDeviceResponse::from_iter([if button { 0x00_u8 } else { 0x80_u8 }, 0x00_u8])
                } else {
                    AdbDeviceResponse::default()
                }
            }
            3 => AdbDeviceResponse::from_iter(
                AdbReg3::default()
                    .with_exceptional(true)
                    .with_srq(true)
                    .with_address(Self::ADDRESS as u8)
                    .with_handler_id(1)
                    .to_be_bytes(),
            ),
            _ => AdbDeviceResponse::default(),
        }
    }

    fn listen(&mut self, reg: u8) -> AdbDeviceResponse {
        trace!("mouse listen: {:02X}", reg);
        AdbDeviceResponse::default()
    }

    fn get_srq(&self) -> bool {
        !self.button_recv.is_empty()
    }
}
