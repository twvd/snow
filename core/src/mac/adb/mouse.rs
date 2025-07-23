use crate::types::{ClickEventReceiver, ClickEventSender};

use super::{AdbDevice, AdbDeviceResponse, AdbReg3};

use log::*;

/// Apple Desktop Bus-connected mouse
pub struct AdbMouse {
    address: u8,
    button_recv: ClickEventReceiver,
}

impl AdbMouse {
    pub const INITIAL_ADDRESS: u8 = 3;

    pub fn new() -> (Self, ClickEventSender) {
        let (s, button_recv) = crossbeam_channel::unbounded();
        (
            Self {
                button_recv,
                address: Self::INITIAL_ADDRESS,
            },
            s,
        )
    }
}

impl AdbDevice for AdbMouse {
    fn get_address(&self) -> u8 {
        self.address
    }

    fn reset(&mut self) {
        self.address = Self::INITIAL_ADDRESS;
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
                    .with_address(Self::INITIAL_ADDRESS)
                    .with_handler_id(1)
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
        !self.button_recv.is_empty()
    }
}
