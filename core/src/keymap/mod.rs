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

/// A keystroke: scancode plus whether Shift is required.
pub struct KeyStroke {
    pub scancode: Scancode,
    pub shift: bool,
}

/// Maps a character to the scancode(s) needed to type it on 'Snow universal'.
/// Returns `None` for characters that cannot be typed.
pub fn char_to_keystroke(ch: char) -> Option<KeyStroke> {
    let (scancode, shift) = match ch {
        'a' => (0x00, false),
        'b' => (0x0B, false),
        'c' => (0x08, false),
        'd' => (0x02, false),
        'e' => (0x0E, false),
        'f' => (0x03, false),
        'g' => (0x05, false),
        'h' => (0x04, false),
        'i' => (0x22, false),
        'j' => (0x26, false),
        'k' => (0x28, false),
        'l' => (0x25, false),
        'm' => (0x2E, false),
        'n' => (0x2D, false),
        'o' => (0x1F, false),
        'p' => (0x23, false),
        'q' => (0x0C, false),
        'r' => (0x0F, false),
        's' => (0x01, false),
        't' => (0x11, false),
        'u' => (0x20, false),
        'v' => (0x09, false),
        'w' => (0x0D, false),
        'x' => (0x07, false),
        'y' => (0x10, false),
        'z' => (0x06, false),

        'A' => (0x00, true),
        'B' => (0x0B, true),
        'C' => (0x08, true),
        'D' => (0x02, true),
        'E' => (0x0E, true),
        'F' => (0x03, true),
        'G' => (0x05, true),
        'H' => (0x04, true),
        'I' => (0x22, true),
        'J' => (0x26, true),
        'K' => (0x28, true),
        'L' => (0x25, true),
        'M' => (0x2E, true),
        'N' => (0x2D, true),
        'O' => (0x1F, true),
        'P' => (0x23, true),
        'Q' => (0x0C, true),
        'R' => (0x0F, true),
        'S' => (0x01, true),
        'T' => (0x11, true),
        'U' => (0x20, true),
        'V' => (0x09, true),
        'W' => (0x0D, true),
        'X' => (0x07, true),
        'Y' => (0x10, true),
        'Z' => (0x06, true),

        '0' => (0x1D, false),
        '1' => (0x12, false),
        '2' => (0x13, false),
        '3' => (0x14, false),
        '4' => (0x15, false),
        '5' => (0x17, false),
        '6' => (0x16, false),
        '7' => (0x1A, false),
        '8' => (0x1C, false),
        '9' => (0x19, false),

        ')' => (0x1D, true),
        '!' => (0x12, true),
        '@' => (0x13, true),
        '#' => (0x14, true),
        '$' => (0x15, true),
        '%' => (0x17, true),
        '^' => (0x16, true),
        '&' => (0x1A, true),
        '*' => (0x1C, true),
        '(' => (0x19, true),

        ' ' => (0x31, false),
        '\n' => (0x24, false),
        '\r' => (0x24, false),
        '\t' => (0x30, false),

        '`' => (0x32, false),
        '~' => (0x32, true),
        '-' => (0x1B, false),
        '_' => (0x1B, true),
        '=' => (0x18, false),
        '+' => (0x18, true),
        '[' => (0x21, false),
        '{' => (0x21, true),
        ']' => (0x1E, false),
        '}' => (0x1E, true),
        '\\' => (0x2A, false),
        '|' => (0x2A, true),
        ';' => (0x29, false),
        ':' => (0x29, true),
        '\'' => (0x27, false),
        '"' => (0x27, true),
        ',' => (0x2B, false),
        '<' => (0x2B, true),
        '.' => (0x2F, false),
        '>' => (0x2F, true),
        '/' => (0x2C, false),
        '?' => (0x2C, true),

        _ => return None,
    };
    Some(KeyStroke { scancode, shift })
}
