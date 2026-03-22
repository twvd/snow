use crate::workspace::CmdKeyMapping;
use eframe::egui;
use snow_core::keymap::Scancode;

/// Maps an egui keycode to 'Snow universal'
#[allow(dead_code)]
pub fn map_egui_keycode(kc: egui::Key) -> Option<Scancode> {
    match kc {
        // ,---.   .---------------. ,---------------. ,---------------. ,-----------.             ,---.
        // |Esc|   |F1 |F2 |F3 |F4 | |F5 |F6 |F7 |F8 | |F9 |F10|F11|F12| |PrS|ScL|Pau|             |Pwr|
        // | 35|   | 7A| 78| 63| 76| | 60| 61| 62| 64| | 65| 6D| 67| 6F| | 69| 6B| 71|             | 7F|
        // `---'   `---------------' `---------------' `---------------' `-----------'             `---'
        egui::Key::Escape => Some(0x35),
        egui::Key::F1 => Some(0x7A),
        egui::Key::F2 => Some(0x78),
        egui::Key::F3 => Some(0x63),
        egui::Key::F4 => Some(0x76),
        egui::Key::F5 => Some(0x60),
        egui::Key::F6 => Some(0x61),
        egui::Key::F7 => Some(0x62),
        egui::Key::F8 => Some(0x64),
        egui::Key::F9 => Some(0x65),
        egui::Key::F10 => Some(0x6D),
        egui::Key::F11 => Some(0x67),
        egui::Key::F12 => Some(0x6F),
        //egui::Key::PrintScreen => Some(0x69),
        //egui::Key::ScrollLock => Some(0x6B),
        //egui::Key::Pause => Some(0x71),

        // ,-----------------------------------------------------------. ,-----------. ,---------------.
        // |  `|  1|  2|  3|  4|  5|  6|  7|  8|  9|  0|  -|  =|Backspa| |Ins|Hom|PgU| |NmL|  =|  /|  *|
        // | 32| 12| 13| 14| 15| 17| 16| 1A| 1C| 19| 1D| 1B| 18|   33  | | 72| 73| 74| | 47| 51| 4B| 43|
        // |-----------------------------------------------------------| |-----------| |---------------|
        egui::Key::Backtick => Some(0x32),
        egui::Key::Num1 => Some(0x12),
        egui::Key::Num2 => Some(0x13),
        egui::Key::Num3 => Some(0x14),
        egui::Key::Num4 => Some(0x15),
        egui::Key::Num5 => Some(0x17),
        egui::Key::Num6 => Some(0x16),
        egui::Key::Num7 => Some(0x1A),
        egui::Key::Num8 => Some(0x1C),
        egui::Key::Num9 => Some(0x19),
        egui::Key::Num0 => Some(0x1D),
        egui::Key::Minus => Some(0x1B),
        egui::Key::Equals => Some(0x18),
        egui::Key::Backspace => Some(0x33),

        egui::Key::Insert => Some(0x72),
        egui::Key::Home => Some(0x73),
        egui::Key::PageUp => Some(0x74),

        //egui::Key::NumLockClear => Some(0x47),
        // = 0x51
        // / 0x4B
        // * 0x43

        // |Tab  |  Q|  W|  E|  R|  T|  Y|  U|  I|  O|  P|  [|  ]|  \  | |Del|End|PgD| |  7|  8|  9|  -|
        // |  30 | 0C| 0D| 0E| 0F| 11| 10| 20| 22| 1F| 23| 21| 1E|  2A | | 75| 77| 79| | 59| 5B| 5C| 4E|
        // |-----------------------------------------------------------| `-----------' |---------------|
        egui::Key::Tab => Some(0x30),
        egui::Key::Q => Some(0x0C),
        egui::Key::W => Some(0x0D),
        egui::Key::E => Some(0x0E),
        egui::Key::R => Some(0x0F),
        egui::Key::T => Some(0x11),
        egui::Key::Y => Some(0x10),
        egui::Key::U => Some(0x20),
        egui::Key::I => Some(0x22),
        egui::Key::O => Some(0x1F),
        egui::Key::P => Some(0x23),
        egui::Key::OpenBracket => Some(0x21),
        egui::Key::CloseBracket => Some(0x1E),
        egui::Key::Backslash => Some(0x2A),

        egui::Key::Delete => Some(0x75),
        egui::Key::End => Some(0x77),
        egui::Key::PageDown => Some(0x79),

        //egui::Key::Kp7 => Some(0x59),
        //egui::Key::Kp8 => Some(0x5B),
        //egui::Key::Kp9 => Some(0x5C),

        // |CapsLo|  A|  S|  D|  F|  G|  H|  J|  K|  L|  ;|  '|  Return|               |  4|  5|  6|  +|
        // |  39  | 00| 01| 02| 03| 05| 04| 26| 28| 25| 29| 27|   24   |               | 56| 57| 58| 45|
        // |-----------------------------------------------------------|     ,---.     |---------------|
        //egui::Key::CapsLock => Some(0x39),
        egui::Key::A => Some(0x00),
        egui::Key::S => Some(0x01),
        egui::Key::D => Some(0x02),
        egui::Key::F => Some(0x03),
        egui::Key::G => Some(0x05),
        egui::Key::H => Some(0x04),
        egui::Key::J => Some(0x26),
        egui::Key::K => Some(0x28),
        egui::Key::L => Some(0x25),
        egui::Key::Semicolon => Some(0x29),
        egui::Key::Quote => Some(0x27),
        egui::Key::Enter => Some(0x24),

        //egui::Key::Kp4 => Some(0x56),
        //egui::Key::Kp5 => Some(0x57),
        //egui::Key::Kp6 => Some(0x58),
        //egui::Key::KpPlus => Some(0x45),

        // |Shift   |  Z|  X|  C|  V|  B|  N|  M|  ,|  .|  /|Shift     |     |Up |     |  1|  2|  3|   |
        // |   38   | 06| 07| 08| 09| 0B| 2D| 2E| 2B| 2F| 2C|    7B    |     | 3E|     | 53| 54| 55|   |
        // |-----------------------------------------------------------| ,-----------. |-----------|Ent|
        //egui::Key::LShift => Some(0x38),
        egui::Key::Z => Some(0x06),
        egui::Key::X => Some(0x07),
        egui::Key::C => Some(0x08),
        egui::Key::V => Some(0x09),
        egui::Key::B => Some(0x0B),
        egui::Key::N => Some(0x2D),
        egui::Key::M => Some(0x2E),
        egui::Key::Comma => Some(0x2B),
        egui::Key::Period => Some(0x2F),
        egui::Key::Slash => Some(0x2C),
        //egui::Key::RShift => Some(0x7B),
        egui::Key::ArrowUp => Some(0x3E),

        //egui::Key::Kp1 => Some(0x53),
        //egui::Key::Kp2 => Some(0x54),
        //egui::Key::Kp3 => Some(0x55),

        //egui::Key::KpEnter => Some(0x4C),

        // |Ctrl |Opt | Cmd |        Space            | Cmd |Opt |Ctrl | |Lef|Dow|Rig| |      0|  .|4C |
        // |  36 | 3A |  37 |           31            |  37 | 7C |  7D | | 3B| 3D| 3C| |    52 | 41|   |
        // `-----------------------------------------------------------' `-----------' `---------------'
        //egui::Key::LCtrl => Some(0x36),
        //egui::Key::LAlt => Some(0x3A),
        //egui::Key::LGui => Some(0x37),
        egui::Key::Space => Some(0x31),
        //egui::Key::RGui => Some(0x37),
        //egui::Key::RAlt => Some(0x7D),
        egui::Key::ArrowLeft => Some(0x3B),
        egui::Key::ArrowDown => Some(0x3D),
        egui::Key::ArrowRight => Some(0x3C),

        //egui::Key::Kp0 => Some(0x52),
        //egui::Key::KpPeriod => Some(0x41),
        _ => None,
    }
}

/// Maps a winit keycode to 'Snow universal'
pub fn map_winit_keycode(
    kc: egui_winit::winit::keyboard::KeyCode,
    cmd_key_mapping: CmdKeyMapping,
) -> Option<Scancode> {
    use egui_winit::winit::keyboard::KeyCode;

    match kc {
        // ,---.   .---------------. ,---------------. ,---------------. ,-----------.             ,---.
        // |Esc|   |F1 |F2 |F3 |F4 | |F5 |F6 |F7 |F8 | |F9 |F10|F11|F12| |PrS|ScL|Pau|             |Pwr|
        // | 35|   | 7A| 78| 63| 76| | 60| 61| 62| 64| | 65| 6D| 67| 6F| | 69| 6B| 71|             | 7F|
        // `---'   `---------------' `---------------' `---------------' `-----------'             `---'
        KeyCode::Escape => Some(0x35),
        KeyCode::F1 => Some(0x7A),
        KeyCode::F2 => Some(0x78),
        KeyCode::F3 => Some(0x63),
        KeyCode::F4 => Some(0x76),
        KeyCode::F5 => Some(0x60),
        KeyCode::F6 => Some(0x61),
        KeyCode::F7 => Some(0x62),
        KeyCode::F8 => Some(0x64),
        KeyCode::F9 => Some(0x65),
        KeyCode::F10 => Some(0x6D),
        KeyCode::F11 => Some(0x67),
        KeyCode::F12 => Some(0x6F),
        KeyCode::PrintScreen => Some(0x69),
        KeyCode::ScrollLock => Some(0x6B),
        KeyCode::Pause => Some(0x71),

        // ,-----------------------------------------------------------. ,-----------. ,---------------.
        // |  `|  1|  2|  3|  4|  5|  6|  7|  8|  9|  0|  -|  =|Backspa| |Ins|Hom|PgU| |NmL|  =|  /|  *|
        // | 32| 12| 13| 14| 15| 17| 16| 1A| 1C| 19| 1D| 1B| 18|   33  | | 72| 73| 74| | 47| 51| 4B| 43|
        // |-----------------------------------------------------------| |-----------| |---------------|
        KeyCode::Backquote => Some(0x32),
        KeyCode::Digit1 => Some(0x12),
        KeyCode::Digit2 => Some(0x13),
        KeyCode::Digit3 => Some(0x14),
        KeyCode::Digit4 => Some(0x15),
        KeyCode::Digit5 => Some(0x17),
        KeyCode::Digit6 => Some(0x16),
        KeyCode::Digit7 => Some(0x1A),
        KeyCode::Digit8 => Some(0x1C),
        KeyCode::Digit9 => Some(0x19),
        KeyCode::Digit0 => Some(0x1D),
        KeyCode::Minus => Some(0x1B),
        KeyCode::Equal => Some(0x18),
        KeyCode::Backspace => Some(0x33),

        KeyCode::Insert => Some(0x72),
        KeyCode::Home => Some(0x73),
        KeyCode::PageUp => Some(0x74),

        KeyCode::NumLock => Some(0x47),
        KeyCode::NumpadEqual => Some(0x51),
        KeyCode::NumpadDivide => Some(0x4B),
        KeyCode::NumpadMultiply => Some(0x43),

        // |Tab  |  Q|  W|  E|  R|  T|  Y|  U|  I|  O|  P|  [|  ]|  \  | |Del|End|PgD| |  7|  8|  9|  -|
        // |  30 | 0C| 0D| 0E| 0F| 11| 10| 20| 22| 1F| 23| 21| 1E|  2A | | 75| 77| 79| | 59| 5B| 5C| 4E|
        // |-----------------------------------------------------------| `-----------' |---------------|
        KeyCode::Tab => Some(0x30),
        KeyCode::KeyQ => Some(0x0C),
        KeyCode::KeyW => Some(0x0D),
        KeyCode::KeyE => Some(0x0E),
        KeyCode::KeyR => Some(0x0F),
        KeyCode::KeyT => Some(0x11),
        KeyCode::KeyY => Some(0x10),
        KeyCode::KeyU => Some(0x20),
        KeyCode::KeyI => Some(0x22),
        KeyCode::KeyO => Some(0x1F),
        KeyCode::KeyP => Some(0x23),
        KeyCode::BracketLeft => Some(0x21),
        KeyCode::BracketRight => Some(0x1E),
        KeyCode::Backslash => Some(0x2A),

        KeyCode::Delete => Some(0x75),
        KeyCode::End => Some(0x77),
        KeyCode::PageDown => Some(0x79),

        KeyCode::Numpad7 => Some(0x59),
        KeyCode::Numpad8 => Some(0x5B),
        KeyCode::Numpad9 => Some(0x5C),

        // |CapsLo|  A|  S|  D|  F|  G|  H|  J|  K|  L|  ;|  '|  Return|               |  4|  5|  6|  +|
        // |  39  | 00| 01| 02| 03| 05| 04| 26| 28| 25| 29| 27|   24   |               | 56| 57| 58| 45|
        // |-----------------------------------------------------------|     ,---.     |---------------|
        KeyCode::CapsLock => Some(0x39),
        KeyCode::KeyA => Some(0x00),
        KeyCode::KeyS => Some(0x01),
        KeyCode::KeyD => Some(0x02),
        KeyCode::KeyF => Some(0x03),
        KeyCode::KeyG => Some(0x05),
        KeyCode::KeyH => Some(0x04),
        KeyCode::KeyJ => Some(0x26),
        KeyCode::KeyK => Some(0x28),
        KeyCode::KeyL => Some(0x25),
        KeyCode::Semicolon => Some(0x29),
        KeyCode::Quote => Some(0x27),
        KeyCode::Enter => Some(0x24),

        KeyCode::Numpad4 => Some(0x56),
        KeyCode::Numpad5 => Some(0x57),
        KeyCode::Numpad6 => Some(0x58),
        KeyCode::NumpadAdd => Some(0x45),

        // |Shift   |  Z|  X|  C|  V|  B|  N|  M|  ,|  .|  /|Shift     |     |Up |     |  1|  2|  3|   |
        // |   38   | 06| 07| 08| 09| 0B| 2D| 2E| 2B| 2F| 2C|    7B    |     | 3E|     | 53| 54| 55|   |
        // |-----------------------------------------------------------| ,-----------. |-----------|Ent|
        KeyCode::ShiftLeft => Some(0x38),
        KeyCode::KeyZ => Some(0x06),
        KeyCode::KeyX => Some(0x07),
        KeyCode::KeyC => Some(0x08),
        KeyCode::KeyV => Some(0x09),
        KeyCode::KeyB => Some(0x0B),
        KeyCode::KeyN => Some(0x2D),
        KeyCode::KeyM => Some(0x2E),
        KeyCode::Comma => Some(0x2B),
        KeyCode::Period => Some(0x2F),
        KeyCode::Slash => Some(0x2C),
        KeyCode::ShiftRight => Some(0x7B),
        KeyCode::ArrowUp => Some(0x3E),

        KeyCode::Numpad1 => Some(0x53),
        KeyCode::Numpad2 => Some(0x54),
        KeyCode::Numpad3 => Some(0x55),

        KeyCode::NumpadEnter => Some(0x4C),

        // |Ctrl |Opt | Cmd |        Space            | Cmd |Opt |Ctrl | |Lef|Dow|Rig| |      0|  .|4C |
        // |  36 | 3A |  37 |           31            |  37 | 7C |  7D | | 3B| 3D| 3C| |    52 | 41|   |
        // `-----------------------------------------------------------' `-----------' `---------------'
        KeyCode::ControlLeft => Some(0x36),
        KeyCode::AltLeft => Some(0x3A),
        KeyCode::SuperLeft => Some(0x37),
        KeyCode::Space => Some(0x31),
        KeyCode::SuperRight => Some(0x37),
        KeyCode::AltRight => match cmd_key_mapping {
            CmdKeyMapping::RightAlt => Some(0x37),
            _ => Some(0x7C),
        },
        KeyCode::ControlRight => match cmd_key_mapping {
            CmdKeyMapping::RightCtrl => Some(0x37),
            _ => Some(0x7D),
        },
        KeyCode::ArrowLeft => Some(0x3B),
        KeyCode::ArrowDown => Some(0x3D),
        KeyCode::ArrowRight => Some(0x3C),

        KeyCode::Numpad0 => Some(0x52),
        KeyCode::NumpadDecimal => Some(0x41),
        _ => None,
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
