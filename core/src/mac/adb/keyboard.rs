use std::collections::VecDeque;

use crate::{
    keymap::{KeyEvent, Keymap},
    mac::adb::AdbEvent,
};

use super::{AdbDevice, AdbDeviceResponse, AdbReg3};

use log::*;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

bitfield! {
    /// Register 2
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct AdbKeyboardReg2(pub u16): Debug, FromStorage, IntoStorage, DerefStorage {
        pub led_numlock: bool @ 0,
        pub led_capslock: bool @ 1,
        pub led_scrolllock: bool @ 2,
        // Bit 3-5 reserved
        pub scrolllock: bool @ 6,
        pub numlock: bool @ 7,
        pub cmd: bool @ 8,
        pub option: bool @ 9,
        pub shift: bool @ 10,
        pub control: bool @ 11,
        pub reset: bool @ 12,
        pub capslock: bool @ 13,
        pub delete: bool @ 14,
        // 15 reserved
    }
}

const SC_CAPSLOCK: u8 = 0x39;
const SC_NUMLOCK: u8 = 0x47;
const SC_SCROLLOCK: u8 = 0x6B;
const SC_LCTRL: u8 = 0x36;
const SC_RCTRL: u8 = 0x7D;
const SC_COMMAND: u8 = 0x37;
const SC_LOPTION: u8 = 0x3A;
const SC_ROPTION: u8 = 0x7C;
const SC_DELETE: u8 = 0x75;

/// Apple Desktop Bus-connected keyboard
#[derive(Serialize, Deserialize)]
pub struct AdbKeyboard {
    address: u8,

    event_queue: VecDeque<KeyEvent>,

    #[serde(with = "BigArray")]
    keystate: [bool; 256],

    capslock: bool,
}

impl AdbKeyboard {
    pub const INITIAL_ADDRESS: u8 = 2;
    pub const KEYMAP: Keymap = Keymap::AekM0115;

    pub fn new() -> Self {
        Self {
            event_queue: VecDeque::new(),
            address: Self::INITIAL_ADDRESS,
            keystate: [false; 256],
            capslock: false,
        }
    }
}

#[typetag::serde]
impl AdbDevice for AdbKeyboard {
    fn event(&mut self, event: &AdbEvent) {
        match event {
            AdbEvent::Key(ke) => self.event_queue.push_back(*ke),
            AdbEvent::ReleaseAll => {
                for (sc, _) in self.keystate.iter().enumerate().filter(|(_, &s)| s) {
                    self.event_queue
                        .push_back(KeyEvent::KeyUp(sc.try_into().unwrap(), Self::KEYMAP));
                }
            }
            _ => (),
        }
    }

    fn get_address(&self) -> u8 {
        self.address
    }

    fn reset(&mut self) {
        self.address = Self::INITIAL_ADDRESS;
        self.flush();
    }

    fn flush(&mut self) {
        self.event_queue.clear();
    }

    fn talk(&mut self, reg: u8) -> AdbDeviceResponse {
        match reg {
            0 => {
                let mut response = AdbDeviceResponse::default();
                for _ in 0..2 {
                    if let Some(ke) = self
                        .event_queue
                        .pop_front()
                        .and_then(|ke| ke.translate_scancode(Self::KEYMAP))
                    {
                        match ke {
                            KeyEvent::KeyDown(sc, _) => {
                                self.keystate[sc as usize] = true;
                                if sc == SC_CAPSLOCK {
                                    // Capslock is a mechanical sticking key
                                    self.capslock = !self.capslock;
                                    if self.capslock {
                                        response.push(sc);
                                    } else {
                                        response.push(0x80 | sc);
                                    }
                                } else {
                                    // Normal/other keys
                                    response.push(sc);
                                }
                            }
                            KeyEvent::KeyUp(sc, _) => {
                                self.keystate[sc as usize] = false;
                                if sc != SC_CAPSLOCK {
                                    response.push(0x80 | sc);
                                }
                            }
                        }
                    }
                }
                if response.len() == 1 {
                    // Must respond either 0 or 2 bytes
                    response.push(0xFF);
                }
                response
            }
            2 => AdbDeviceResponse::from_iter(
                AdbKeyboardReg2::default()
                    .with_led_numlock(self.keystate[SC_NUMLOCK as usize])
                    .with_led_capslock(self.capslock)
                    .with_led_scrolllock(self.keystate[SC_SCROLLOCK as usize])
                    .with_numlock(self.keystate[SC_NUMLOCK as usize])
                    .with_capslock(self.capslock)
                    .with_scrolllock(self.keystate[SC_SCROLLOCK as usize])
                    .with_cmd(self.keystate[SC_COMMAND as usize])
                    .with_control(
                        self.keystate[SC_LCTRL as usize] || self.keystate[SC_RCTRL as usize],
                    )
                    .with_option(
                        self.keystate[SC_LOPTION as usize] || self.keystate[SC_ROPTION as usize],
                    )
                    .with_delete(self.keystate[SC_DELETE as usize])
                    .to_be_bytes(),
            ),
            3 => AdbDeviceResponse::from_iter(
                AdbReg3::default()
                    .with_exceptional(true)
                    .with_srq(true)
                    .with_address(Self::INITIAL_ADDRESS)
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
            2 => (),
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
        !self.event_queue.is_empty()
    }
}
