use sdl2::keyboard::Keycode;
use snow_core::keymap::Scancode;

/// Maps an SDL keycode to 'Snow universal'
pub fn map_sdl_keycode(kc: Keycode) -> Option<Scancode> {
    match kc {
        // ,---.   .---------------. ,---------------. ,---------------. ,-----------.             ,---.
        // |Esc|   |F1 |F2 |F3 |F4 | |F5 |F6 |F7 |F8 | |F9 |F10|F11|F12| |PrS|ScL|Pau|             |Pwr|
        // | 35|   | 7A| 78| 63| 76| | 60| 61| 62| 64| | 65| 6D| 67| 6F| | 69| 6B| 71|             | 7F|
        // `---'   `---------------' `---------------' `---------------' `-----------'             `---'
        Keycode::Escape => Some(0x35),
        Keycode::F1 => Some(0x7A),
        Keycode::F2 => Some(0x78),
        Keycode::F3 => Some(0x63),
        Keycode::F4 => Some(0x76),
        Keycode::F5 => Some(0x60),
        Keycode::F6 => Some(0x61),
        Keycode::F7 => Some(0x62),
        Keycode::F8 => Some(0x64),
        Keycode::F9 => Some(0x65),
        Keycode::F10 => Some(0x6D),
        Keycode::F11 => Some(0x67),
        Keycode::F12 => Some(0x6F),
        Keycode::PrintScreen => Some(0x69),
        Keycode::ScrollLock => Some(0x6B),
        Keycode::Pause => Some(0x71),

        // ,-----------------------------------------------------------. ,-----------. ,---------------.
        // |  `|  1|  2|  3|  4|  5|  6|  7|  8|  9|  0|  -|  =|Backspa| |Ins|Hom|PgU| |NmL|  =|  /|  *|
        // | 32| 12| 13| 14| 15| 17| 16| 1A| 1C| 19| 1D| 1B| 18|   33  | | 72| 73| 74| | 47| 51| 4B| 43|
        // |-----------------------------------------------------------| |-----------| |---------------|
        Keycode::Backquote => Some(0x32),
        Keycode::Num1 => Some(0x12),
        Keycode::Num2 => Some(0x13),
        Keycode::Num3 => Some(0x14),
        Keycode::Num4 => Some(0x15),
        Keycode::Num5 => Some(0x17),
        Keycode::Num6 => Some(0x16),
        Keycode::Num7 => Some(0x1A),
        Keycode::Num8 => Some(0x1C),
        Keycode::Num9 => Some(0x19),
        Keycode::Num0 => Some(0x1D),
        Keycode::Minus => Some(0x1B),
        Keycode::Equals => Some(0x18),
        Keycode::Backspace => Some(0x33),

        Keycode::Insert => Some(0x72),
        Keycode::Home => Some(0x73),
        Keycode::PageUp => Some(0x74),

        Keycode::NumLockClear => Some(0x47),
        // = 0x51
        // / 0x4B
        // * 0x43

        // |Tab  |  Q|  W|  E|  R|  T|  Y|  U|  I|  O|  P|  [|  ]|  \  | |Del|End|PgD| |  7|  8|  9|  -|
        // |  30 | 0C| 0D| 0E| 0F| 11| 10| 20| 22| 1F| 23| 21| 1E|  2A | | 75| 77| 79| | 59| 5B| 5C| 4E|
        // |-----------------------------------------------------------| `-----------' |---------------|
        Keycode::Tab => Some(0x30),
        Keycode::Q => Some(0x0C),
        Keycode::W => Some(0x0D),
        Keycode::E => Some(0x0E),
        Keycode::R => Some(0x0F),
        Keycode::T => Some(0x11),
        Keycode::Y => Some(0x10),
        Keycode::U => Some(0x20),
        Keycode::I => Some(0x22),
        Keycode::O => Some(0x1F),
        Keycode::P => Some(0x23),
        Keycode::LeftBracket => Some(0x21),
        Keycode::RightBracket => Some(0x1E),
        Keycode::Backslash => Some(0x2A),

        Keycode::Delete => Some(0x75),
        Keycode::End => Some(0x77),
        Keycode::PageDown => Some(0x79),

        Keycode::Kp7 => Some(0x59),
        Keycode::Kp8 => Some(0x5B),
        Keycode::Kp9 => Some(0x5C),

        // |CapsLo|  A|  S|  D|  F|  G|  H|  J|  K|  L|  ;|  '|  Return|               |  4|  5|  6|  +|
        // |  39  | 00| 01| 02| 03| 05| 04| 26| 28| 25| 29| 27|   24   |               | 56| 57| 58| 45|
        // |-----------------------------------------------------------|     ,---.     |---------------|
        Keycode::CapsLock => Some(0x39),
        Keycode::A => Some(0x00),
        Keycode::S => Some(0x01),
        Keycode::D => Some(0x02),
        Keycode::F => Some(0x03),
        Keycode::G => Some(0x05),
        Keycode::H => Some(0x04),
        Keycode::J => Some(0x26),
        Keycode::K => Some(0x28),
        Keycode::L => Some(0x25),
        Keycode::Semicolon => Some(0x29),
        Keycode::Quote => Some(0x27),
        Keycode::Return => Some(0x24),

        Keycode::Kp4 => Some(0x56),
        Keycode::Kp5 => Some(0x57),
        Keycode::Kp6 => Some(0x58),
        Keycode::KpPlus => Some(0x45),

        // |Shift   |  Z|  X|  C|  V|  B|  N|  M|  ,|  .|  /|Shift     |     |Up |     |  1|  2|  3|   |
        // |   38   | 06| 07| 08| 09| 0B| 2D| 2E| 2B| 2F| 2C|    7B    |     | 3E|     | 53| 54| 55|   |
        // |-----------------------------------------------------------| ,-----------. |-----------|Ent|
        Keycode::LShift => Some(0x38),
        Keycode::Z => Some(0x06),
        Keycode::X => Some(0x07),
        Keycode::C => Some(0x08),
        Keycode::V => Some(0x09),
        Keycode::B => Some(0x0B),
        Keycode::N => Some(0x2D),
        Keycode::M => Some(0x2E),
        Keycode::Comma => Some(0x2B),
        Keycode::Period => Some(0x2F),
        Keycode::Slash => Some(0x2C),
        Keycode::RShift => Some(0x7B),

        Keycode::Up => Some(0x3E),

        Keycode::Kp1 => Some(0x53),
        Keycode::Kp2 => Some(0x54),
        Keycode::Kp3 => Some(0x55),

        Keycode::KpEnter => Some(0x4C),

        // |Ctrl |Opt | Cmd |        Space            | Cmd |Opt |Ctrl | |Lef|Dow|Rig| |      0|  .|4C |
        // |  36 | 3A |  37 |           31            |  37 | 7C |  7D | | 3B| 3D| 3C| |    52 | 41|   |
        // `-----------------------------------------------------------' `-----------' `---------------'
        Keycode::LCtrl => Some(0x36),
        Keycode::LAlt => Some(0x3A),
        Keycode::LGui => Some(0x37),
        Keycode::Space => Some(0x31),
        Keycode::RGui => Some(0x37),
        Keycode::RAlt => Some(0x7D),
        Keycode::Left => Some(0x3B),
        Keycode::Down => Some(0x3D),
        Keycode::Right => Some(0x3C),

        Keycode::Kp0 => Some(0x52),
        Keycode::KpPeriod => Some(0x41),

        _ => None,
    }
}
