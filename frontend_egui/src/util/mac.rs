//! Macintosh helper functions

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

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
