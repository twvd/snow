use std::collections::VecDeque;

use anyhow::Result;
use log::*;

/// A keyboard event. Inner value is the scancode
pub enum KeyEvent {
    KeyDown(u8),
    KeyUp(u8),
}

/// Apple M0110 keyboard
#[derive(Default)]
pub struct Keyboard {
    event_queue: VecDeque<KeyEvent>,
}

// Scancodes
pub const SC_BACKSPACE: u8 = 0x67;
pub const SC_TAB: u8 = 0x61;
pub const SC_CAPSLOCK: u8 = 0x73;
pub const SC_RETURN: u8 = 0x49;
pub const SC_SHIFT: u8 = 0x71;
pub const SC_OPTION: u8 = 0x75;
pub const SC_APPLE: u8 = 0x6F;
pub const SC_SPACE: u8 = 0x63;

impl Keyboard {
    pub fn event(&mut self, ev: KeyEvent) -> Result<()> {
        self.event_queue.push_back(ev);
        Ok(())
    }

    pub fn char_to_scancode(chr: char) -> Option<u8> {
        // https://github.com/tmk/tmk_keyboard/wiki/Apple-M0110-Keyboard-Protocol
        // ,---------------------------------------------------------.    ,---------------.
        // |  `|  1|  2|  3|  4|  5|  6|  7|  8|  9|  0|  -|  =|Backs|    |Clr|  -|Lft|Rgt|
        // |---------------------------------------------------------|    |---------------|
        // |Tab  |  Q|  W|  E|  R|  T|  Y|  U|  I|  O|  P|  [|  ]|  \|    |  7|  8|  9|Up |
        // |---------------------------------------------------------|    |---------------|
        // |CapsLo|  A|  S|  D|  F|  G|  H|  J|  K|  L|  ;|  '|Return|    |  4|  5|  6|Dn |
        // |---------------------------------------------------------|    |---------------|
        // |Shift   |  Z|  X|  C|  V|  B|  N|  M|  ,|  .|  /|        |    |  1|  2|  3|   |
        // `---------------------------------------------------------'    |-----------|Ent|
        //      |Opt|Mac |         Space               |Enter|Opt|        |      0|  .|   |
        //      `------------------------------------------------'        `---------------'
        // ,---------------------------------------------------------.    ,---------------.
        // | 65| 25| 27| 29| 2B| 2F| 2D| 35| 39| 33| 3B| 37| 31|   67|    |+0F|+1D|+0D|+05|
        // |---------------------------------------------------------|    |---------------|
        // |   61| 19| 1B| 1D| 1F| 23| 21| 41| 45| 3F| 47| 43| 3D| 55|    |+33|+37|+39|+1B|
        // |---------------------------------------------------------|    |---------------|
        // |    73| 01| 03| 05| 07| 0B| 09| 4D| 51| 4B| 53| 4F|    49|    |+2D|+2F|+31|+11|
        // |---------------------------------------------------------|    |---------------|
        // |      71| 0D| 0F| 11| 13| 17| 5B| 5D| 27| 5F| 59|      71|    |+27|+29|+2B|   |
        // `---------------------------------------------------------'    |-----------|+19|
        //      | 75|   6F|            63              |   69| 75|        |    +25|+03|   |
        //      `------------------------------------------------'        `---------------'
        match chr.to_ascii_uppercase() {
            // |  `|  1|  2|  3|  4|  5|  6|  7|  8|  9|  0|  -|  =|Backs|    |Clr|  -|Lft|Rgt|
            // | 65| 25| 27| 29| 2B| 2F| 2D| 35| 39| 33| 3B| 37| 31|   67|    |+0F|+1D|+0D|+05|
            '`' => Some(0x65),
            '1' => Some(0x25),
            '2' => Some(0x27),
            '3' => Some(0x29),
            '4' => Some(0x2B),
            '5' => Some(0x2F),
            '6' => Some(0x2D),
            '7' => Some(0x35),
            '8' => Some(0x39),
            '9' => Some(0x33),
            '0' => Some(0x3B),
            '-' => Some(0x37),
            '=' => Some(0x31),
            // |Tab  |  Q|  W|  E|  R|  T|  Y|  U|  I|  O|  P|  [|  ]|  \|    |  7|  8|  9|Up |
            // |   61| 19| 1B| 1D| 1F| 23| 21| 41| 45| 3F| 47| 43| 3D| 55|    |+33|+37|+39|+1B|
            'Q' => Some(0x19),
            'W' => Some(0x1B),
            'E' => Some(0x1D),
            'R' => Some(0x1F),
            'T' => Some(0x23),
            'Y' => Some(0x21),
            'U' => Some(0x41),
            'I' => Some(0x45),
            'O' => Some(0x3F),
            'P' => Some(0x47),
            '[' => Some(0x43),
            ']' => Some(0x3D),
            '\\' => Some(0x55),
            // |CapsLo|  A|  S|  D|  F|  G|  H|  J|  K|  L|  ;|  '|Return|    |  4|  5|  6|Dn |
            // |    73| 01| 03| 05| 07| 0B| 09| 4D| 51| 4B| 53| 4F|    49|    |+2D|+2F|+31|+11|
            'A' => Some(0x01),
            'S' => Some(0x03),
            'D' => Some(0x05),
            'F' => Some(0x07),
            'G' => Some(0x0B),
            'H' => Some(0x09),
            'J' => Some(0x4D),
            'K' => Some(0x51),
            'L' => Some(0x4B),
            ';' => Some(0x53),
            '\'' => Some(0x4F),
            // |Shift   |  Z|  X|  C|  V|  B|  N|  M|  ,|  .|  /|        |    |  1|  2|  3|   |
            // |      71| 0D| 0F| 11| 13| 17| 5B| 5D| 27| 5F| 59|      71|    |+27|+29|+2B|   |
            'Z' => Some(0x0D),
            'X' => Some(0x0F),
            'C' => Some(0x11),
            'V' => Some(0x13),
            'B' => Some(0x17),
            'N' => Some(0x5B),
            'M' => Some(0x5D),
            ',' => Some(0x27),
            '.' => Some(0x5F),
            '/' => Some(0x59),
            //      |Opt|Mac |         Space               |Enter|Opt|        |      0|  .|   |
            //      | 75|   6F|            63              |   69| 75|        |    +25|+03|   |
            ' ' => Some(0x63),
            _ => None,
        }
    }

    pub fn cmd(&mut self, cmd: u8) -> Result<u8> {
        match cmd {
            // Inquire/Instant
            0x10 | 0x14 => {
                if let Some(ev) = self.event_queue.pop_front() {
                    let result = match ev {
                        KeyEvent::KeyDown(sc) => sc,
                        KeyEvent::KeyUp(sc) => 0x80 | sc,
                    };
                    Ok(result | 0x01)
                } else {
                    // Null
                    Ok(0x7B)
                }
            }
            // Model
            0x16 => {
                // US layout
                info!("Keyboard reset");
                Ok(3)
            }
            // Test
            0x36 => Ok(0x7D),
            _ => {
                warn!("Unknown keyboard command ${:02X}", cmd);
                Ok(0)
            }
        }
    }
}
