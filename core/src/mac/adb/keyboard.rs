use crate::keymap::KeyEvent;
use crate::types::{KeyEventReceiver, KeyEventSender};

use super::{AdbDevice, AdbDeviceResponse, AdbReg3};

use log::*;

/// Apple Desktop Bus-connected keyboard
pub struct AdbKeyboard {
    address: u8,
    key_recv: KeyEventReceiver,
}

impl AdbKeyboard {
    pub const INITIAL_ADDRESS: u8 = 2;

    pub fn new() -> (Self, KeyEventSender) {
        let (s, key_recv) = crossbeam_channel::unbounded();
        (
            Self {
                key_recv,
                address: Self::INITIAL_ADDRESS,
            },
            s,
        )
    }
}

impl AdbDevice for AdbKeyboard {
    fn get_address(&self) -> u8 {
        self.address
    }

    fn reset(&mut self) {
        self.address = Self::INITIAL_ADDRESS;
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
                    .with_address(self.address)
                    .with_handler_id(2) // Apple Extended Keyboard M0115
                    .to_be_bytes(),
            ),
            _ => {
                warn!("Unimplemented talk register {}", reg);
                AdbDeviceResponse::default()
            }
        }
    }

    fn listen(&mut self, reg: u8, data: &[u8]) {
        match reg {
            3 => {
                if data.len() < 2 {
                    error!("Listen reg 3 invalid data length: {:02X?}", data);
                    return;
                }

                let value = AdbReg3(u16::from_be_bytes(data[0..2].try_into().unwrap()));
                if value.handler_id() == 0xFE {
                    // Address re-assignment
                    self.address = value.address();
                } else {
                    warn!(
                        "Unimplemented listen register 3, handler id {:02X} = {:02X?}",
                        value.handler_id(),
                        value
                    );
                }
            }
            _ => warn!("Unimplemented listen register {} = {:02X?}", reg, data),
        }
    }

    fn get_srq(&self) -> bool {
        !self.key_recv.is_empty()
    }
}
