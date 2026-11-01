//! Macintosh text encoding helpers.

use crate::bus::Address;

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

/// Reads the TEXT scrap of the Classic Mac OS Scrap Manager.
///
/// `read` reads a byte of memory at a logical address. Returns `None` if the
/// scrap is not in memory, empty, or contains no TEXT entry, or if it can't be
/// read.
pub fn read_scrap_text(mut read: impl FnMut(Address) -> Option<u8>) -> Option<String> {
    fn read_u32(read: &mut impl FnMut(Address) -> Option<u8>, addr: Address) -> Option<u32> {
        let mut value = 0;
        for i in 0..4 {
            value = (value << 8) | u32::from(read(addr.wrapping_add(i))?);
        }
        Some(value)
    }

    // scrapState: negative means scrap is on disk, not in RAM
    if read_u32(&mut read, 0x096A)? & 0x8000_0000 != 0 {
        return None;
    }

    let scrap_size = read_u32(&mut read, 0x0960)?;
    if scrap_size == 0 || scrap_size > 1024 * 1024 {
        return None;
    }

    let handle = read_u32(&mut read, 0x0964)?;
    if handle == 0 {
        return None;
    }

    // Dereference Handle: handle - master pointer - scrap data
    let master_ptr = read_u32(&mut read, handle)?;
    if master_ptr == 0 {
        return None;
    }

    // Walk scrap entries looking for 'TEXT'
    let mut offset = 0;
    while offset + 8 <= scrap_size {
        let addr = master_ptr.wrapping_add(offset);
        let type_code = read_u32(&mut read, addr)?;
        let data_len = read_u32(&mut read, addr.wrapping_add(4))?;
        if data_len > scrap_size - offset - 8 {
            return None;
        }

        if type_code == u32::from_be_bytes(*b"TEXT") {
            let data_addr = addr.wrapping_add(8);
            let mut text = Vec::with_capacity(data_len as usize);
            for i in 0..data_len {
                text.push(read(data_addr.wrapping_add(i))?);
            }
            return Some(macroman_to_utf8(&text));
        }

        // Next entry: 4 (type) + 4 (length) + data padded to even
        offset += 8 + ((data_len + 1) & !1);
    }
    None
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

#[cfg(test)]
mod scrap_tests {
    use super::read_scrap_text;
    use std::collections::HashMap;

    fn put(mem: &mut HashMap<u32, u8>, addr: u32, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            mem.insert(addr + i as u32, b);
        }
    }

    /// Memory with a scrap of a PICT entry of 3 bytes and a TEXT entry
    fn memory() -> HashMap<u32, u8> {
        let mut mem = HashMap::new();
        put(&mut mem, 0x0960, &[0, 0, 0, 25]);
        put(&mut mem, 0x0964, &0x1000_u32.to_be_bytes());
        put(&mut mem, 0x096A, &[0, 0, 0, 0]);
        put(&mut mem, 0x1000, &0x2000_u32.to_be_bytes());
        put(&mut mem, 0x2000, b"PICT\0\0\0\x03abc\0");
        put(&mut mem, 0x200C, b"TEXT\0\0\0\x05Hel\rx");
        mem
    }

    fn scrap(mem: &HashMap<u32, u8>) -> Option<String> {
        read_scrap_text(|addr| mem.get(&addr).copied())
    }

    #[test]
    fn finds_the_text_entry() {
        assert_eq!(scrap(&memory()).as_deref(), Some("Hel\nx"));
    }

    #[test]
    fn scrap_on_disk() {
        let mut mem = memory();
        put(&mut mem, 0x096A, &[0xFF, 0xFF]);
        assert_eq!(scrap(&mem), None);
    }

    #[test]
    fn unreadable_text() {
        let mut mem = memory();
        mem.remove(&0x2015);
        assert_eq!(scrap(&mem), None);
    }

    #[test]
    fn no_text_entry() {
        let mut mem = memory();
        put(&mut mem, 0x200C, b"STYL");
        assert_eq!(scrap(&mem), None);
    }
}
