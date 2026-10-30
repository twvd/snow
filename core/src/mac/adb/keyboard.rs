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
const SC_LSHIFT: u8 = 0x38;
const SC_RSHIFT: u8 = 0x7B;
const SC_LCTRL: u8 = 0x36;
const SC_RCTRL: u8 = 0x7D;
const SC_COMMAND: u8 = 0x37;
const SC_LOPTION: u8 = 0x3A;
const SC_ROPTION: u8 = 0x7C;
const SC_DELETE: u8 = 0x75;
const SC_POWER: u8 = 0x7F;

/// Apple Desktop Bus-connected keyboard
#[derive(Serialize, Deserialize)]
pub struct AdbKeyboard {
    address: u8,
    handler_id: u8,

    event_queue: VecDeque<KeyEvent>,

    /// Keys the guest has been told are down: set as talk register 0
    /// sends their events, read by talk register 2
    #[serde(with = "BigArray")]
    reported_keys: [bool; 256],

    /// Keys down on the keyboard, from the key events received. The events
    /// still in the queue are not in `reported_keys` yet. Caps Lock is a
    /// locking key: it is down while locked.
    #[serde(with = "BigArray")]
    physical_keys: [bool; 256],
}

impl AdbKeyboard {
    pub const INITIAL_ADDRESS: u8 = 2;
    pub const INITIAL_HANDLER_ID: u8 = 2;
    pub const KEYMAP: Keymap = Keymap::AekM0115;

    pub fn new() -> Self {
        Self {
            event_queue: VecDeque::new(),
            address: Self::INITIAL_ADDRESS,
            handler_id: Self::INITIAL_HANDLER_ID,
            reported_keys: [false; 256],
            physical_keys: [false; 256],
        }
    }
}

#[typetag::serde]
impl AdbDevice for AdbKeyboard {
    fn event(&mut self, event: &AdbEvent) {
        match event {
            AdbEvent::Key(ke) => match ke.translate_scancode(Self::KEYMAP) {
                // Caps Lock locks down on one press and comes back up on the
                // next one, the release of the key does nothing
                Some(KeyEvent::KeyDown(SC_CAPSLOCK, _)) => {
                    let locked = !self.physical_keys[SC_CAPSLOCK as usize];
                    self.physical_keys[SC_CAPSLOCK as usize] = locked;
                    self.event_queue.push_back(if locked {
                        KeyEvent::KeyDown(SC_CAPSLOCK, Self::KEYMAP)
                    } else {
                        KeyEvent::KeyUp(SC_CAPSLOCK, Self::KEYMAP)
                    });
                }
                Some(KeyEvent::KeyUp(SC_CAPSLOCK, _)) => (),
                Some(ke @ KeyEvent::KeyDown(sc, _)) => {
                    self.physical_keys[sc as usize] = true;
                    self.event_queue.push_back(ke);
                }
                Some(ke @ KeyEvent::KeyUp(sc, _)) => {
                    self.physical_keys[sc as usize] = false;
                    self.event_queue.push_back(ke);
                }
                None => (),
            },
            // We cannot know the state of the host keyboard, Caps Lock
            // included: release everything rather than leave keys stuck
            AdbEvent::ReleaseAll => {
                for (sc, _) in self.physical_keys.iter().enumerate().filter(|(_, d)| **d) {
                    self.event_queue
                        .push_back(KeyEvent::KeyUp(sc.try_into().unwrap(), Self::KEYMAP));
                }
                self.physical_keys = [false; 256];
            }
            _ => (),
        }
    }

    fn get_address(&self) -> u8 {
        self.address
    }

    fn reset(&mut self) {
        self.address = Self::INITIAL_ADDRESS;
        self.handler_id = Self::INITIAL_HANDLER_ID;
        self.flush();
    }

    fn flush(&mut self) {
        // The keyboard firmware (M0115, M3501) marks all the keys up on a
        // reset or a flush, so its next scan reports again the keys down in
        // `physical_keys`. The Mac OS relies on it: it clears KeyMap and
        // flushes the keyboard at startup, and Shift held from power on must
        // still turn off the extensions.
        self.event_queue.clear();
        self.reported_keys = [false; 256];
        for (sc, _) in self.physical_keys.iter().enumerate().filter(|(_, d)| **d) {
            self.event_queue
                .push_back(KeyEvent::KeyDown(sc.try_into().unwrap(), Self::KEYMAP));
        }
    }

    fn talk(&mut self, reg: u8) -> AdbDeviceResponse {
        match reg {
            0 => {
                let mut response = AdbDeviceResponse::default();
                // The events are already in the keyboard's key map
                for _ in 0..2 {
                    // The Power key is reported alone, in both bytes:
                    // $7F7F when pressed, $FFFF when released
                    if let Some(ke) = self.event_queue.front()
                        && ke.as_scancode() == SC_POWER
                    {
                        if response.is_empty() {
                            let down = matches!(ke, KeyEvent::KeyDown(..));
                            self.event_queue.pop_front();
                            self.reported_keys[SC_POWER as usize] = down;
                            let code = if down { SC_POWER } else { 0xFF };
                            response.push(code);
                            response.push(code);
                        }
                        break;
                    }
                    match self.event_queue.pop_front() {
                        Some(KeyEvent::KeyDown(sc, _)) => {
                            self.reported_keys[sc as usize] = true;
                            response.push(sc);
                        }
                        Some(KeyEvent::KeyUp(sc, _)) => {
                            self.reported_keys[sc as usize] = false;
                            response.push(0x80 | sc);
                        }
                        None => (),
                    }
                }
                if response.len() == 1 {
                    // Must respond either 0 or 2 bytes
                    response.push(0xFF);
                }
                response
            }
            2 => AdbDeviceResponse::from_iter(
                // The key/modifier bits of register 2 are ACTIVE LOW
                AdbKeyboardReg2(0xFFFF)
                    // LED state bits (0..2) are host-controlled and active-high
                    .with_led_numlock(self.reported_keys[SC_NUMLOCK as usize])
                    .with_led_capslock(self.reported_keys[SC_CAPSLOCK as usize])
                    .with_led_scrolllock(self.reported_keys[SC_SCROLLOCK as usize])
                    .with_numlock(!self.reported_keys[SC_NUMLOCK as usize])
                    .with_capslock(!self.reported_keys[SC_CAPSLOCK as usize])
                    .with_scrolllock(!self.reported_keys[SC_SCROLLOCK as usize])
                    .with_cmd(!self.reported_keys[SC_COMMAND as usize])
                    .with_control(
                        !(self.reported_keys[SC_LCTRL as usize]
                            || self.reported_keys[SC_RCTRL as usize]),
                    )
                    .with_option(
                        !(self.reported_keys[SC_LOPTION as usize]
                            || self.reported_keys[SC_ROPTION as usize]),
                    )
                    .with_shift(
                        !(self.reported_keys[SC_LSHIFT as usize]
                            || self.reported_keys[SC_RSHIFT as usize]),
                    )
                    .with_delete(!self.reported_keys[SC_DELETE as usize])
                    .to_be_bytes(),
            ),
            3 => AdbDeviceResponse::from_iter(
                AdbReg3::default()
                    .with_exceptional(true)
                    .with_srq(true)
                    .with_address(self.address)
                    .with_handler_id(self.handler_id)
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
                value.apply_listen(&mut self.address, &mut self.handler_id);
            }
            _ => warn!("Unimplemented listen register {} = {:02X?}", reg, data),
        }
    }

    fn get_srq(&self) -> bool {
        !self.event_queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(down: bool, sc: u8) -> AdbEvent {
        AdbEvent::Key(if down {
            KeyEvent::KeyDown(sc, AdbKeyboard::KEYMAP)
        } else {
            KeyEvent::KeyUp(sc, AdbKeyboard::KEYMAP)
        })
    }

    fn reg2(kbd: &mut AdbKeyboard) -> AdbKeyboardReg2 {
        AdbKeyboardReg2(u16::from_be_bytes(kbd.talk(2)[..].try_into().unwrap()))
    }

    #[test]
    fn reset_reports_keys_down() {
        let mut kbd = AdbKeyboard::new();
        kbd.event(&key(true, SC_LSHIFT));
        kbd.reset();
        assert_eq!(kbd.talk(0).as_slice(), [SC_LSHIFT, 0xFF]);
        assert!(!kbd.get_srq());

        kbd.reset();
        assert_eq!(kbd.talk(0).as_slice(), [SC_LSHIFT, 0xFF]);
    }

    #[test]
    fn flush_reports_keys_down() {
        let mut kbd = AdbKeyboard::new();
        kbd.event(&key(true, SC_LSHIFT));
        kbd.event(&key(true, SC_COMMAND));
        assert_eq!(kbd.talk(0).as_slice(), [SC_LSHIFT, SC_COMMAND]);

        // A release not sent yet is not lost
        kbd.event(&key(false, SC_COMMAND));
        kbd.flush();
        assert_eq!(kbd.talk(0).as_slice(), [SC_LSHIFT, 0xFF]);
        assert!(kbd.talk(0).is_empty());
        let reg2 = reg2(&mut kbd);
        assert!(!reg2.shift());
        assert!(reg2.cmd());
    }

    #[test]
    fn capslock_locks() {
        let mut kbd = AdbKeyboard::new();
        kbd.event(&key(true, SC_CAPSLOCK));
        kbd.event(&key(false, SC_CAPSLOCK));
        assert_eq!(kbd.talk(0).as_slice(), [SC_CAPSLOCK, 0xFF]);
        assert!(!reg2(&mut kbd).capslock());

        // Still locked after a flush
        kbd.flush();
        assert_eq!(kbd.talk(0).as_slice(), [SC_CAPSLOCK, 0xFF]);

        kbd.event(&key(true, SC_CAPSLOCK));
        kbd.event(&key(false, SC_CAPSLOCK));
        assert_eq!(kbd.talk(0).as_slice(), [0x80 | SC_CAPSLOCK, 0xFF]);
        assert!(reg2(&mut kbd).capslock());
    }

    #[test]
    fn power_key_is_reported_alone() {
        let mut kbd = AdbKeyboard::new();
        kbd.event(&key(true, 0x00));
        kbd.event(&key(true, SC_POWER));
        kbd.event(&key(false, SC_POWER));
        assert_eq!(kbd.talk(0).as_slice(), &[0x00, 0xFF]);
        assert_eq!(kbd.talk(0).as_slice(), &[0x7F, 0x7F]);
        assert_eq!(kbd.talk(0).as_slice(), &[0xFF, 0xFF]);
        assert!(kbd.talk(0).is_empty());
    }

    #[test]
    fn release_all() {
        let mut kbd = AdbKeyboard::new();
        kbd.event(&key(true, SC_CAPSLOCK));
        kbd.event(&key(true, SC_LSHIFT));
        kbd.event(&AdbEvent::ReleaseAll);
        assert_eq!(kbd.talk(0).as_slice(), [SC_CAPSLOCK, SC_LSHIFT]);
        assert_eq!(
            kbd.talk(0).as_slice(),
            [0x80 | SC_LSHIFT, 0x80 | SC_CAPSLOCK]
        );
        kbd.flush();
        assert!(kbd.talk(0).is_empty());
    }
}
