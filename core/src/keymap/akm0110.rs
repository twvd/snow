//! Apple Keyboard M0110
//!
//! https://github.com/tmk/tmk_keyboard/wiki/Apple-M0110-Keyboard-Protocol
//! ,---------------------------------------------------------.
//! |  `|  1|  2|  3|  4|  5|  6|  7|  8|  9|  0|  -|  =|Backs|
//! |---------------------------------------------------------|
//! |Tab  |  Q|  W|  E|  R|  T|  Y|  U|  I|  O|  P|  [|  ]|  \|
//! |---------------------------------------------------------|
//! |CapsLo|  A|  S|  D|  F|  G|  H|  J|  K|  L|  ;|  '|Return|
//! |---------------------------------------------------------|
//! |Shift   |  Z|  X|  C|  V|  B|  N|  M|  ,|  .|  /|        |
//! `---------------------------------------------------------'
//!      |Opt|Mac |         Space               |Enter|Opt|    
//!      `------------------------------------------------'    
//! ,---------------------------------------------------------.
//! | 65| 25| 27| 29| 2B| 2F| 2D| 35| 39| 33| 3B| 37| 31|   67|
//! |---------------------------------------------------------|
//! |   61| 19| 1B| 1D| 1F| 23| 21| 41| 45| 3F| 47| 43| 3D| 55|
//! |---------------------------------------------------------|
//! |    73| 01| 03| 05| 07| 0B| 09| 4D| 51| 4B| 53| 4F|    49|
//! |---------------------------------------------------------|
//! |      71| 0D| 0F| 11| 13| 17| 5B| 5D| 57| 5F| 59|      71|
//! `---------------------------------------------------------'
//!      | 75|   6F|            63              |   69| 75|    
//!      `------------------------------------------------'    

use super::Scancode;

pub(super) fn translate(sc: Scancode) -> Option<Scancode> {
    match sc {
        // |  `|  1|  2|  3|  4|  5|  6|  7|  8|  9|  0|  -|  =|Backspa|
        // | 32| 12| 13| 14| 15| 17| 16| 1A| 1C| 19| 1D| 1B| 18|   33  |
        // |  `|  1|  2|  3|  4|  5|  6|  7|  8|  9|  0|  -|  =|Backs|
        // | 65| 25| 27| 29| 2B| 2F| 2D| 35| 39| 33| 3B| 37| 31|   67|
        0x32 => Some(0x65),
        0x12 => Some(0x25),
        0x13 => Some(0x27),
        0x14 => Some(0x29),
        0x15 => Some(0x2B),
        0x17 => Some(0x2F),
        0x16 => Some(0x2D),
        0x1A => Some(0x35),
        0x1C => Some(0x39),
        0x19 => Some(0x33),
        0x1D => Some(0x3B),
        0x1B => Some(0x37),
        0x18 => Some(0x31),
        0x33 => Some(0x67),

        // |Tab  |  Q|  W|  E|  R|  T|  Y|  U|  I|  O|  P|  [|  ]|  \  | |Del|End|PgD| |  7|  8|  9|  -|
        // |  30 | 0C| 0D| 0E| 0F| 11| 10| 20| 22| 1F| 23| 21| 1E|  2A | | 75| 77| 79| | 59| 5B| 5C| 4E|
        // |Tab  |  Q|  W|  E|  R|  T|  Y|  U|  I|  O|  P|  [|  ]|  \|
        // |   61| 19| 1B| 1D| 1F| 23| 21| 41| 45| 3F| 47| 43| 3D| 55|
        0x30 => Some(0x61),
        0x0C => Some(0x19),
        0x0D => Some(0x1B),
        0x0E => Some(0x1D),
        0x0F => Some(0x1F),
        0x11 => Some(0x23),
        0x10 => Some(0x21),
        0x20 => Some(0x41),
        0x22 => Some(0x45),
        0x1F => Some(0x3F),
        0x23 => Some(0x47),
        0x21 => Some(0x43),
        0x1E => Some(0x3D),
        0x2A => Some(0x55),

        // |CapsLo|  A|  S|  D|  F|  G|  H|  J|  K|  L|  ;|  '|  Return|               |  4|  5|  6|  +|
        // |  39  | 00| 01| 02| 03| 05| 04| 26| 28| 25| 29| 27|   24   |               | 56| 57| 58| 45|
        // |CapsLo|  A|  S|  D|  F|  G|  H|  J|  K|  L|  ;|  '|Return|
        // |    73| 01| 03| 05| 07| 0B| 09| 4D| 51| 4B| 53| 4F|    49|
        0x39 => Some(0x73),
        0x00 => Some(0x01),
        0x01 => Some(0x03),
        0x02 => Some(0x05),
        0x03 => Some(0x07),
        0x05 => Some(0x0B),
        0x04 => Some(0x09),
        0x26 => Some(0x4D),
        0x28 => Some(0x51),
        0x25 => Some(0x4B),
        0x29 => Some(0x53),
        0x27 => Some(0x4F),
        0x24 => Some(0x49),

        // |Shift   |  Z|  X|  C|  V|  B|  N|  M|  ,|  ,|  /|Shift     |     |Up |     |  1|  2|  3|   |
        // |   38   | 06| 07| 08| 09| 0B| 2D| 2E| 2B| 2F| 2C|    7B    |     | 3E|     | 53| 54| 55|   |
        // |Shift   |  Z|  X|  C|  V|  B|  N|  M|  ,|  .|  /|        |
        // |      71| 0D| 0F| 11| 13| 17| 5B| 5D| 57| 5F| 59|      71|
        0x38 => Some(0x71),
        0x06 => Some(0x0D),
        0x07 => Some(0x0F),
        0x08 => Some(0x11),
        0x09 => Some(0x13),
        0x0B => Some(0x17),
        0x2D => Some(0x5B),
        0x2E => Some(0x5D),
        0x2B => Some(0x57),
        0x2F => Some(0x5F),
        0x2C => Some(0x59),
        0x7B => Some(0x71),

        // |Ctrl |Opt | Cmd |        Space            | Cmd |Opt |Ctrl | |Lef|Dow|Rig| |      0|  .|   |
        // |  36 | 3A |  37 |           31            |  37 | 7C |  7D | | 3B| 3D| 3C| |    52 | 41|   |
        //      |Opt|Mac |         Space               |Enter|Opt|
        //      | 75|   6F|            63              |   69| 75|
        0x3A => Some(0x75),
        0x37 => Some(0x6F),
        0x31 => Some(0x63),
        // 0x69
        0x7C => Some(0x75),

        _ => None,
    }
}
