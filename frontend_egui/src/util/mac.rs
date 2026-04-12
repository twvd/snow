//! Macintosh helper functions

use snow_core::util::mac::macroman_to_utf8;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Reads the TEXT scrap from a Classic Mac OS Scrap Manager RAM image.
/// Returns `None` if the scrap is not in memory, empty, or contains no TEXT entry.
pub fn read_scrap_text(mem: &[u8]) -> Option<String> {
    if mem.len() < 0x0980 {
        return None;
    }

    let read_u32 = |addr: usize| -> Option<u32> {
        mem.get(addr..addr + 4)
            .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
    };
    let read_i16 = |addr: usize| -> Option<i16> {
        mem.get(addr..addr + 2)
            .map(|s| i16::from_be_bytes(s.try_into().unwrap()))
    };

    // scrapState: negative means scrap is on disk, not in RAM
    if read_i16(0x096A)? < 0 {
        return None;
    }

    let scrap_size = read_u32(0x0960)? as usize;
    if scrap_size == 0 || scrap_size > 1024 * 1024 {
        return None;
    }

    let handle = read_u32(0x0964)? as usize;
    if handle == 0 || handle + 4 > mem.len() {
        return None;
    }

    // Dereference Handle: handle - master pointer - scrap data
    let master_ptr = read_u32(handle)? as usize;
    if master_ptr == 0 {
        return None;
    }
    if master_ptr
        .checked_add(scrap_size)
        .is_none_or(|end| end > mem.len())
    {
        return None;
    }

    // Walk scrap entries looking for 'TEXT'
    let mut offset = 0usize;
    while offset + 8 <= scrap_size {
        let addr = master_ptr.checked_add(offset)?;
        if addr + 8 > mem.len() {
            return None;
        }

        let type_code = &mem[addr..addr + 4];
        let data_len = read_u32(addr + 4)? as usize;
        if data_len > scrap_size - offset - 8 {
            return None;
        }

        if type_code == b"TEXT" {
            let data_addr = addr + 8;
            if data_addr + data_len > mem.len() {
                return None;
            }
            return Some(macroman_to_utf8(&mem[data_addr..data_addr + data_len]));
        }

        // Next entry: 4 (type) + 4 (length) + data padded to even
        offset += 8 + ((data_len + 1) & !1);
    }
    None
}

/// Checks the sanity of a hard drive image and can return a warning
/// as string. Will silently fail if file fails to open.
pub fn hdd_sanitycheck<P: AsRef<Path>>(p: P) -> Option<&'static str> {
    let Ok(mut f) = File::open(p) else {
        return None;
    };

    if f.seek(SeekFrom::Start(0x400)).is_err() {
        return None;
    }

    let mut hfs_header = [0u8; 2];
    let _ = f.read_exact(&mut hfs_header);
    if hfs_header == *b"BD" {
        return Some(
            "The image you are loading looks like a volume image, which is not supported.\n\
            Snow requires device images. See the documentation for more info and \
            conversion instructions.
            ",
        );
    }

    None
}
