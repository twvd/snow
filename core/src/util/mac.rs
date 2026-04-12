//! Macintosh text encoding helpers.

use std::collections::HashMap;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

/// Converts a MacRoman-encoded byte slice to a UTF-8 string.
pub fn macroman_to_utf8(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| match byte {
            0x0D => '\n',
            0x00..=0x7F => byte as char,
            _ => MACROMAN_HIGH[(byte - 0x80) as usize],
        })
        .collect()
}

/// Converts a UTF-8 string into MacRoman bytes, replacing unsupported
/// characters with `?`.
pub fn utf8_to_macroman(text: &str) -> Vec<u8> {
    let reverse_map = macroman_reverse_map();
    text.nfc()
        .map(|ch| match ch {
            '\n' => 0x0D,
            '\0'..='\u{7F}' => ch as u8,
            _ => reverse_map.get(&ch).copied().unwrap_or(b'?'),
        })
        .collect()
}

fn macroman_reverse_map() -> &'static HashMap<char, u8> {
    static REVERSE_MAP: OnceLock<HashMap<char, u8>> = OnceLock::new();
    REVERSE_MAP.get_or_init(|| {
        MACROMAN_HIGH
            .iter()
            .enumerate()
            .map(|(idx, ch)| (*ch, idx as u8 + 0x80))
            .collect()
    })
}

#[rustfmt::skip]
const MACROMAN_HIGH: [char; 128] = [
    '\u{00C4}', '\u{00C5}', '\u{00C7}', '\u{00C9}', '\u{00D1}', '\u{00D6}', '\u{00DC}', '\u{00E1}',
    '\u{00E0}', '\u{00E2}', '\u{00E4}', '\u{00E3}', '\u{00E5}', '\u{00E7}', '\u{00E9}', '\u{00E8}',
    '\u{00EA}', '\u{00EB}', '\u{00ED}', '\u{00EC}', '\u{00EE}', '\u{00EF}', '\u{00F1}', '\u{00F3}',
    '\u{00F2}', '\u{00F4}', '\u{00F6}', '\u{00F5}', '\u{00FA}', '\u{00F9}', '\u{00FB}', '\u{00FC}',
    '\u{2020}', '\u{00B0}', '\u{00A2}', '\u{00A3}', '\u{00A7}', '\u{2022}', '\u{00B6}', '\u{00DF}',
    '\u{00AE}', '\u{00A9}', '\u{2122}', '\u{00B4}', '\u{00A8}', '\u{2260}', '\u{00C6}', '\u{00D8}',
    '\u{221E}', '\u{00B1}', '\u{2264}', '\u{2265}', '\u{00A5}', '\u{00B5}', '\u{2202}', '\u{2211}',
    '\u{220F}', '\u{03C0}', '\u{222B}', '\u{00AA}', '\u{00BA}', '\u{03A9}', '\u{00E6}', '\u{00F8}',
    '\u{00BF}', '\u{00A1}', '\u{00AC}', '\u{221A}', '\u{0192}', '\u{2248}', '\u{2206}', '\u{00AB}',
    '\u{00BB}', '\u{2026}', '\u{00A0}', '\u{00C0}', '\u{00C3}', '\u{00D5}', '\u{0152}', '\u{0153}',
    '\u{2013}', '\u{2014}', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}', '\u{00F7}', '\u{25CA}',
    '\u{00FF}', '\u{0178}', '\u{2044}', '\u{20AC}', '\u{2039}', '\u{203A}', '\u{FB01}', '\u{FB02}',
    '\u{2021}', '\u{00B7}', '\u{201A}', '\u{201E}', '\u{2030}', '\u{00C2}', '\u{00CA}', '\u{00C1}',
    '\u{00CB}', '\u{00C8}', '\u{00CD}', '\u{00CE}', '\u{00CF}', '\u{00CC}', '\u{00D3}', '\u{00D4}',
    '\u{F8FF}', '\u{00D2}', '\u{00DA}', '\u{00DB}', '\u{00D9}', '\u{0131}', '\u{02C6}', '\u{02DC}',
    '\u{00AF}', '\u{02D8}', '\u{02D9}', '\u{02DA}', '\u{00B8}', '\u{02DD}', '\u{02DB}', '\u{02C7}',
];
