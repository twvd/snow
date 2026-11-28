//! Key names and the US layout, as Snow 'universal' scancodes (the Apple
//! Extended Keyboard's, see snow_core::keymap::aekm0115). The core translates
//! them for the machine's own keyboard.

use snow_core::keymap::Scancode;

pub const SHIFT: Scancode = 0x38;

/// Scancode of a key name (case does not matter)
pub fn by_name(name: &str) -> Option<Scancode> {
    let n = name.to_ascii_lowercase();
    if n.chars().count() == 1 {
        let c = n.chars().next()?;
        return by_char(c).map(|(sc, _)| sc);
    }
    Some(match n.as_str() {
        "cmd" | "command" | "apple" | "meta" | "super" => 0x37,
        "shift" => SHIFT,
        "option" | "opt" | "alt" => 0x3A,
        "control" | "ctrl" => 0x36,
        "capslock" | "caps" => 0x39,
        "return" | "ret" | "cr" => 0x24,
        "enter" | "kpenter" | "kp-enter" => 0x4C,
        "tab" => 0x30,
        "space" | "spacebar" => 0x31,
        "delete" | "backspace" | "bs" => 0x33,
        "forwarddelete" | "fwddelete" | "del" => 0x75,
        "escape" | "esc" => 0x35,
        "left" => 0x3B,
        "right" => 0x3C,
        "down" => 0x3D,
        "up" => 0x3E,
        "help" | "insert" | "ins" => 0x72,
        "home" => 0x73,
        "end" => 0x77,
        "pageup" | "pgup" => 0x74,
        "pagedown" | "pgdn" => 0x79,
        "power" => 0x7F,
        "f1" => 0x7A,
        "f2" => 0x78,
        "f3" => 0x63,
        "f4" => 0x76,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6D,
        "f11" => 0x67,
        "f12" => 0x6F,
        "f13" | "printscreen" => 0x69,
        "f14" | "scrolllock" => 0x6B,
        "f15" | "pause" => 0x71,
        "clear" | "numlock" => 0x47,
        "kp0" => 0x52,
        "kp1" => 0x53,
        "kp2" => 0x54,
        "kp3" => 0x55,
        "kp4" => 0x56,
        "kp5" => 0x57,
        "kp6" => 0x58,
        "kp7" => 0x59,
        "kp8" => 0x5B,
        "kp9" => 0x5C,
        "kp." | "kpdot" | "kpdecimal" => 0x41,
        "kp*" | "kpmultiply" => 0x43,
        "kp+" | "kpplus" => 0x45,
        "kp/" | "kpdivide" => 0x4B,
        "kp-" | "kpminus" => 0x4E,
        "kp=" | "kpequals" => 0x51,
        "minus" => 0x1B,
        "equals" | "equal" => 0x18,
        "plus" => 0x18,
        "comma" => 0x2B,
        "period" | "dot" => 0x2F,
        "slash" => 0x2C,
        "backslash" => 0x2A,
        "semicolon" => 0x29,
        "quote" | "apostrophe" => 0x27,
        "backtick" | "grave" => 0x32,
        "leftbracket" => 0x21,
        "rightbracket" => 0x1E,
        _ => return None,
    })
}

/// Scancode of a character on the US layout, and whether it needs Shift
pub fn by_char(c: char) -> Option<(Scancode, bool)> {
    const PLAIN: &[(char, Scancode)] = &[
        ('a', 0x00),
        ('s', 0x01),
        ('d', 0x02),
        ('f', 0x03),
        ('h', 0x04),
        ('g', 0x05),
        ('z', 0x06),
        ('x', 0x07),
        ('c', 0x08),
        ('v', 0x09),
        ('b', 0x0B),
        ('q', 0x0C),
        ('w', 0x0D),
        ('e', 0x0E),
        ('r', 0x0F),
        ('y', 0x10),
        ('t', 0x11),
        ('1', 0x12),
        ('2', 0x13),
        ('3', 0x14),
        ('4', 0x15),
        ('6', 0x16),
        ('5', 0x17),
        ('=', 0x18),
        ('9', 0x19),
        ('7', 0x1A),
        ('-', 0x1B),
        ('8', 0x1C),
        ('0', 0x1D),
        (']', 0x1E),
        ('o', 0x1F),
        ('u', 0x20),
        ('[', 0x21),
        ('i', 0x22),
        ('p', 0x23),
        ('\r', 0x24),
        ('\n', 0x24),
        ('l', 0x25),
        ('j', 0x26),
        ('\'', 0x27),
        ('k', 0x28),
        (';', 0x29),
        ('\\', 0x2A),
        (',', 0x2B),
        ('/', 0x2C),
        ('n', 0x2D),
        ('m', 0x2E),
        ('.', 0x2F),
        ('\t', 0x30),
        (' ', 0x31),
        ('`', 0x32),
        ('\x08', 0x33),
        ('\x1b', 0x35),
    ];
    const SHIFTED: &[(char, char)] = &[
        ('!', '1'),
        ('@', '2'),
        ('#', '3'),
        ('$', '4'),
        ('%', '5'),
        ('^', '6'),
        ('&', '7'),
        ('*', '8'),
        ('(', '9'),
        (')', '0'),
        ('_', '-'),
        ('+', '='),
        ('{', '['),
        ('}', ']'),
        ('|', '\\'),
        (':', ';'),
        ('"', '\''),
        ('<', ','),
        ('>', '.'),
        ('?', '/'),
        ('~', '`'),
    ];
    let plain = |c: char| PLAIN.iter().find(|(k, _)| *k == c).map(|(_, sc)| *sc);
    if c.is_ascii_uppercase() {
        return plain(c.to_ascii_lowercase()).map(|sc| (sc, true));
    }
    if let Some(sc) = plain(c) {
        return Some((sc, false));
    }
    SHIFTED
        .iter()
        .find(|(k, _)| *k == c)
        .and_then(|(_, base)| plain(*base))
        .map(|sc| (sc, true))
}

/// Parses a combination such as "cmd+shift+s" or "option+e": the modifiers
/// in order, then the key
pub fn combo(s: &str) -> Result<Vec<Scancode>, String> {
    // A lone "+" or a combination that ends in "+" means the plus key
    let parts: Vec<&str> = if s == "+" {
        vec!["+"]
    } else if let Some(head) = s.strip_suffix("++") {
        head.split('+').chain(std::iter::once("+")).collect()
    } else {
        s.split('+').collect()
    };
    let mut out = Vec::new();
    for p in parts {
        let p = p.trim();
        if p.is_empty() {
            return Err(format!("Bad key combination '{}'", s));
        }
        // A shifted character such as "?" needs Shift as well
        if p.chars().count() == 1
            && let Some((sc, shift)) = by_char(p.chars().next().unwrap())
        {
            if shift && !out.contains(&SHIFT) {
                out.push(SHIFT);
            }
            out.push(sc);
            continue;
        }
        out.push(by_name(p).ok_or_else(|| format!("Unknown key '{}' in '{}'", p, s))?);
    }
    Ok(out)
}
