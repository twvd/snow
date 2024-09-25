use crate::keymap::KeyEvent;
use crate::types::{KeyEventReceiver, KeyEventSender};

use super::{AdbDevice, AdbDeviceResponse, AdbReg3};

use log::*;

/// Apple Desktop Bus-connected keyboard
pub struct AdbKeyboard {
    key_recv: KeyEventReceiver,
}

impl AdbKeyboard {
    pub const ADDRESS: usize = 2;

    pub fn new() -> (Self, KeyEventSender) {
        let (s, key_recv) = crossbeam_channel::unbounded();
        (Self { key_recv }, s)
    }
}

impl AdbDevice for AdbKeyboard {
    fn reset(&mut self) {
        self.flush();
    }

    fn flush(&mut self) {
        while !self.key_recv.is_empty() {
            let _ = self.key_recv.recv();
        }
    }

    fn talk(&mut self, reg: u8) -> AdbDeviceResponse {
        match reg {
            0 => {
                let mut response = AdbDeviceResponse::default();
                for _ in 0..2 {
                    if let Ok(ke) = self.key_recv.try_recv() {
                        match ke {
                            KeyEvent::KeyDown(sc) => {
                                response.push(sc);
                            }
                            KeyEvent::KeyUp(sc) => {
                                response.push(0x80 | sc);
                            }
                        }
                    }
                }
                response
            }
            3 => AdbDeviceResponse::from_iter(
                AdbReg3::default()
                    .with_exceptional(true)
                    .with_srq(true)
                    .with_address(Self::ADDRESS as u8)
                    .with_handler_id(2) // Apple Extended Keyboard M0115
                    .to_be_bytes(),
            ),
            _ => AdbDeviceResponse::default(),
        }
    }

    fn listen(&mut self, reg: u8) -> AdbDeviceResponse {
        trace!("keyboard listen: {:02X}", reg);
        AdbDeviceResponse::default()
    }

    fn get_srq(&self) -> bool {
        !self.key_recv.is_empty()
    }
}
