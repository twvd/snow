//! Keyboard scancode (re-)mapping code
//!
//! The Snow core expects keyboard scancode inputs from the frontend to follow
//! the 'Snow universal' keymap, which is identical to the Apple Extended keyboard M0115.
//!
//! A frontend will need to translate its own native scancodes (e.g. SDL) to the Snow universal
//! keymap.

use serde::{Deserialize, Serialize};

mod aekm0115;
mod akm0110;

/// Type to represent a keyboard scancode
pub type Scancode = u8;

/// A keyboard event. Inner value is the scancode
#[derive(Serialize, Deserialize, Copy, Clone)]
pub enum KeyEvent {
    KeyDown(u8, Keymap),
    KeyUp(u8, Keymap),
}

impl KeyEvent {
    pub fn translate_scancode(self, to_map: Keymap) -> Option<Self> {
        Some(match self {
            Self::KeyDown(sc, km) => {
                if km != to_map {
                    Self::KeyDown(to_map.translate(sc)?, to_map)
                } else {
                    self
                }
            }
            Self::KeyUp(sc, km) => {
                if km != to_map {
                    Self::KeyUp(to_map.translate(sc)?, to_map)
                } else {
                    self
                }
            }
        })
    }

    pub fn as_scancode(self) -> u8 {
        match self {
            Self::KeyDown(sc, _) => sc,
            Self::KeyUp(sc, _) => sc,
        }
    }
}

/// A keyboard mapping
#[derive(Serialize, Deserialize, Copy, Clone, Eq, PartialEq)]
pub enum Keymap {
    /// Snow universal
    Universal,
    /// Apple extended keyboard M0115 (ADB)
    AekM0115,
    /// Apple M0110 keyboard (512K/Plus)
    AkM0110,
}

impl Keymap {
    /// Translates a scancode from 'universal' to a target keyboard scancode.
    /// Returns None if no scancode available on the target keyboard.
    pub fn translate(self, sc_in: Scancode) -> Option<Scancode> {
        match self {
            Self::Universal => panic!("Invalid translation"),
            Self::AekM0115 => aekm0115::translate(sc_in),
            Self::AkM0110 => akm0110::translate(sc_in),
        }
    }
}
