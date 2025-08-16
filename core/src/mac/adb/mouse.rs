use crate::{
    types::{MouseEventReceiver, MouseEventSender},
    util::take_from_accumulator,
};

use super::{AdbDevice, AdbDeviceResponse, AdbReg3};

use log::*;
use proc_bitfield::bitfield;

bitfield! {
    /// Talk Register 0
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct AdbMouseReg0(pub u16): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Buffered relative X motion
        pub x: i8 @ 0..=6,
        /// Buffered relative Y motion
        pub y: i8 @ 8..=14,
        /// Primary button (inverted)
        pub btn: bool @ 15,
    }
}

const MAX_REL_MOVE: i32 = 31;
const MOUSE_FACTOR: i32 = 1;

/// Apple Desktop Bus-connected mouse
pub struct AdbMouse {
    address: u8,
    event_recv: MouseEventReceiver,
    button: bool,
    rel_move_x: i32,
    rel_move_y: i32,
}

impl AdbMouse {
    pub const INITIAL_ADDRESS: u8 = 3;

    pub fn new() -> (Self, MouseEventSender) {
        let (s, button_recv) = crossbeam_channel::unbounded();
        (
            Self {
                event_recv: button_recv,
                address: Self::INITIAL_ADDRESS,
                button: false,
                rel_move_x: 0,
                rel_move_y: 0,
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
        while !self.event_recv.is_empty() {
            let _ = self.event_recv.recv();
        }
        self.rel_move_x = 0;
        self.rel_move_y = 0;
        self.button = false;
    }

    fn talk(&mut self, reg: u8) -> AdbDeviceResponse {
        match reg {
            0 => {
                if self.get_srq() {
                    while !self.event_recv.is_empty() {
                        let event = self.event_recv.recv().unwrap();
                        if let Some(btn) = event.button {
                            self.button = btn;
                        }
                        if let Some(rel_move) = event.rel_movement {
                            self.rel_move_x =
                                self.rel_move_x.saturating_add(rel_move.0 * MOUSE_FACTOR);
                            self.rel_move_y =
                                self.rel_move_y.saturating_add(rel_move.1 * MOUSE_FACTOR);
                        }
                    }
                    let motion_x = take_from_accumulator(&mut self.rel_move_x, MAX_REL_MOVE) as i8;
                    let motion_y = take_from_accumulator(&mut self.rel_move_y, MAX_REL_MOVE) as i8;

                    AdbDeviceResponse::from_iter(
                        AdbMouseReg0::default()
                            .with_btn(!self.button)
                            .with_x(motion_x)
                            .with_y(motion_y)
                            .to_be_bytes(),
                    )
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
        !self.event_recv.is_empty() || self.rel_move_x != 0 || self.rel_move_y != 0
    }
}
