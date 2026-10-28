//! Metadata storage for volume files
//!
//! Probes for AppleDouble metadata (both the netatalk `.AppleDouble/`
//! directory convention and the macOS `._` sidecar convention) and
//! AppleSingle containers, reading the resource fork, the Finder
//! information and the file dates. Files without metadata fall back to
//! their host file attributes.

use std::fs;
use std::path::{Path, PathBuf};

/// AppleDouble/AppleSingle magic numbers
const AD_MAGIC_V1: [u8; 4] = [0x00, 0x05, 0x16, 0x00];
const AD_MAGIC_V2: [u8; 4] = [0x00, 0x05, 0x16, 0x07];

/// AppleDouble/AppleSingle entry identifiers
const ENTRY_DATA_FORK: u32 = 1;
const ENTRY_RSRC_FORK: u32 = 2;
const ENTRY_DATES: u32 = 8;
const ENTRY_FINDER_INFO: u32 = 9;

/// Seconds between the Unix (1970) and AFP (2000-01-01) epochs
const AFP_EPOCH_OFFSET: u64 = 946684800;

/// The metadata of a volume file
#[derive(Debug, Clone, Default)]
pub struct Meta {
    /// The 32-byte Finder information (FInfo + FXInfo or DInfo + DXInfo)
    pub finder_info: [u8; 32],
    /// AFP epoch dates (signed seconds since 2000-01-01, 0 when
    /// unknown)
    pub create_date: u32,
    pub mod_date: u32,
    pub backup_date: u32,
    /// Length of the resource fork (0 when none)
    pub rsrc_fork_len: u32,
    /// Length of the data fork
    pub data_len: u64,
    /// Offset of the data fork inside the host file (nonzero for
    /// AppleSingle containers)
    pub data_offset: u64,
    /// Where the resource fork lives: the container file and its offset
    pub rsrc_file: Option<PathBuf>,
    pub rsrc_offset: u64,
}

/// Which metadata containers the probe reads
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeFlags {
    /// The AppleDouble containers: the ._ sidecars and the
    /// .AppleDouble directories
    pub apple_double: bool,
    /// AppleSingle files
    pub apple_single: bool,
}

impl Default for ProbeFlags {
    fn default() -> Self {
        Self {
            apple_double: true,
            apple_single: true,
        }
    }
}

/// Open with every metadata container probed
pub fn open(path: &Path) -> Meta {
    open_with(path, &ProbeFlags::default())
}

/// Open, probing only the enabled metadata containers
pub fn open_with(path: &Path, flags: &ProbeFlags) -> Meta {
    open_inner(path, flags)
}

fn open_inner(path: &Path, flags: &ProbeFlags) -> Meta {
    let _ = flags;
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return Meta::default(),
    };

    if flags.apple_double {
        // macOS convention: a `._<name>` sidecar next to the file
        let mut sidecar = path.to_path_buf();
        sidecar.set_file_name(format!("._{}", name));
        if let Some(meta) = read_apple_double(&sidecar, path) {
            return meta;
        }

        // netatalk convention: `.AppleDouble/<name>` next to the file
        let mut dir = match path.parent() {
            Some(p) => p.to_path_buf(),
            None => Path::new(".").to_path_buf(),
        };
        dir.push(".AppleDouble");
        dir.push(name);
        if let Some(meta) = read_apple_double(&dir, path) {
            return meta;
        }
    }

    // AppleSingle: the file itself is the container
    if flags.apple_single
        && let Some(meta) = read_apple_single(path)
    {
        return meta;
    }

    // Plain host file: dates from the file system, no Finder information
    let mut meta = Meta::default();
    if let Ok(md) = fs::metadata(path) {
        meta.data_len = md.len();
        if let Ok(modified) = md.modified()
            && let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            meta.mod_date = (d.as_secs() as i64 - AFP_EPOCH_OFFSET as i64) as i32 as u32; // dates before 2000 stay negative
        }
    }
    // Synthesize a type/creator from the file extension, so plain host
    // files get the standard icons of the classic type they map to
    if let Some((file_type, creator)) = synthesized_finder(path) {
        meta.finder_info[0..4].copy_from_slice(file_type);
        meta.finder_info[4..8].copy_from_slice(creator);
    }
    // An unknown creation date is reported as the modification date;
    // an unknown backup date as the spec's "never" value
    if meta.create_date == 0 {
        meta.create_date = meta.mod_date;
    }
    if meta.backup_date == 0 {
        meta.backup_date = 0x8000_0000;
    }
    meta
}

/// Synthesize a classic (type, creator) pair from a host file's
/// extension, for files without real Finder information. The types are
/// the standard ones the client's own desktop database has icons for,
/// so plain Unix files show the right icons and open with the right
/// application when one is present. Unknown extensions get nothing
/// (the client shows the generic document).
fn synthesized_finder(path: &Path) -> Option<(&'static [u8; 4], &'static [u8; 4])> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let (file_type, creator) = match ext.as_str() {
        // Text and source code all read as TEXT (TeachText-compatible)
        "txt" | "text" | "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "m" | "mm" | "s" | "asm"
        | "rs" | "py" | "pl" | "sh" | "js" | "ts" | "java" | "go" | "lua" | "rb" | "swift"
        | "kt" | "tex" | "csv" | "tsv" | "log" | "ini" | "cfg" | "conf" | "toml" | "yml"
        | "yaml" | "xml" | "json" | "md" | "diff" | "patch" | "make" | "cmake" => {
            (b"TEXT", b"ttxt")
        }
        "html" | "htm" | "css" => (b"TEXT", b"ttxt"),
        // Images
        "jpg" | "jpeg" => (b"JPEG", b"????"),
        "gif" => (b"GIFf", b"????"),
        "png" => (b"PNGf", b"????"),
        "tif" | "tiff" => (b"TIFF", b"????"),
        "bmp" => (b"BMPp", b"????"),
        "pict" | "pct" => (b"PICT", b"????"),
        "psd" => (b"8BPS", b"????"),
        "eps" | "ps" => (b"EPSF", b"????"),
        "tga" => (b"TPIC", b"????"),
        "icns" => (b"icns", b"????"),
        // Documents and archives
        "pdf" => (b"PDF ", b"CARO"),
        "rtf" => (b"RTF ", b"MSWD"),
        "sit" => (b"SIT!", b"SIT!"),
        "hqx" => (b"TEXT", b"BnHq"),
        "zip" => (b"ZIP ", b"????"),
        "gz" => (b"Gzip", b"????"),
        "tgz" => (b"Gzip", b"????"),
        "bz2" => (b"Bzip", b"????"),
        // Audio and video
        "wav" => (b"WAVE", b"????"),
        "aif" | "aiff" => (b"AIFF", b"????"),
        "mov" => (b"MooV", b"????"),
        "avi" => (b"VfW ", b"????"),
        _ => return None,
    };
    Some((file_type, creator))
}

/// Read an AppleDouble container (a sidecar or a `.AppleDouble` entry)
/// holding the metadata of the file at `file_path`
fn read_apple_double(container: &Path, file_path: &Path) -> Option<Meta> {
    let data = fs::read(container).ok()?;
    let entries = parse_container(&data)?;
    let mut meta = base_meta(file_path);
    apply_entries(&mut meta, &data, &entries);
    if meta.rsrc_fork_len != 0 {
        meta.rsrc_file = Some(container.to_path_buf());
    }
    // A host-side change since the container was written (the file's
    // mtime moved past the container's own mtime) surfaces: the stored
    // dates describe the file as the Mac last saw it, and a Finder
    // copy keeps its stored (older) date while the host mtime matches
    // the container's
    if let (Ok(fm), Ok(cm)) = (fs::metadata(file_path), fs::metadata(container))
        && let (Ok(f_modified), Ok(c_modified)) = (fm.modified(), cm.modified())
        && f_modified > c_modified
        && let Ok(d) = f_modified.duration_since(std::time::UNIX_EPOCH)
    {
        meta.mod_date = (d.as_secs() as i64 - AFP_EPOCH_OFFSET as i64) as i32 as u32;
    }
    Some(meta)
}

/// Read an AppleSingle container: the data fork lives inside the file
/// itself
fn read_apple_single(path: &Path) -> Option<Meta> {
    use std::io::Read;
    // Peek the magic first: reading a whole file (a disk image can be
    // gigabytes) just to reject it dominates a scan of a large share
    let mut f = fs::File::open(path).ok()?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    if magic != AD_MAGIC_V1 {
        return None;
    }
    let data = fs::read(path).ok()?;
    let entries = parse_container(&data)?;
    let mut meta = Meta::default();
    apply_entries(&mut meta, &data, &entries);
    if meta.create_date == 0 {
        meta.create_date = meta.mod_date;
    }
    if meta.backup_date == 0 {
        meta.backup_date = 0x8000_0000;
    }
    // The data fork is an entry inside the container
    if let Some((offset, len)) = entries.get(&ENTRY_DATA_FORK) {
        meta.data_offset = *offset as u64;
        meta.data_len = *len as u64;
    }
    if meta.rsrc_fork_len != 0 {
        meta.rsrc_file = Some(path.to_path_buf());
    }
    Some(meta)
}

/// The metadata of a plain host file: its dates and data fork length
fn base_meta(path: &Path) -> Meta {
    let mut meta = Meta::default();
    if let Ok(md) = fs::metadata(path) {
        meta.data_len = md.len();
        if let Ok(modified) = md.modified()
            && let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            meta.mod_date = (d.as_secs() as i64 - AFP_EPOCH_OFFSET as i64) as i32 as u32; // dates before 2000 stay negative
        }
    }
    // An unknown creation date is reported as the modification date;
    // an unknown backup date as the spec's "never" value
    if meta.create_date == 0 {
        meta.create_date = meta.mod_date;
    }
    if meta.backup_date == 0 {
        meta.backup_date = 0x8000_0000;
    }
    meta
}

/// Parse the entries of an AppleDouble or AppleSingle container
fn parse_container(data: &[u8]) -> Option<std::collections::HashMap<u32, (u32, u32)>> {
    if data.len() < 26 || !(data.starts_with(&AD_MAGIC_V1) || data.starts_with(&AD_MAGIC_V2)) {
        return None;
    }
    let num_entries = u16::from_be_bytes([data[24], data[25]]) as usize;
    let mut entries = std::collections::HashMap::new();
    let mut pos = 26;
    for _ in 0..num_entries {
        if data.len() < pos + 12 {
            return None;
        }
        let id = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        let offset =
            u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]);
        let len =
            u32::from_be_bytes([data[pos + 8], data[pos + 9], data[pos + 10], data[pos + 11]]);
        entries.insert(id, (offset, len));
        pos += 12;
    }
    Some(entries)
}

/// Apply the container entries to the metadata
fn apply_entries(
    meta: &mut Meta,
    data: &[u8],
    entries: &std::collections::HashMap<u32, (u32, u32)>,
) {
    if let Some((offset, len)) = entries.get(&ENTRY_FINDER_INFO) {
        let len = (*len as usize).min(32);
        let start = *offset as usize;
        if data.len() >= start + len {
            meta.finder_info[..len].copy_from_slice(&data[start..start + len]);
        }
    }
    // The AppleDouble v2 File Dates entry (ID 8) holds the same
    // seconds-since-2000 values AFP uses on the wire
    if let Some((offset, len)) = entries.get(&ENTRY_DATES)
        && *len >= 16
    {
        let start = *offset as usize;
        if data.len() >= start + 16 {
            meta.create_date = u32::from_be_bytes(data[start..start + 4].try_into().unwrap());
            meta.mod_date = u32::from_be_bytes(data[start + 4..start + 8].try_into().unwrap());
            meta.backup_date = u32::from_be_bytes(data[start + 8..start + 12].try_into().unwrap());
        }
    }
    if let Some((offset, len)) = entries.get(&ENTRY_RSRC_FORK) {
        meta.rsrc_fork_len = *len;
        meta.rsrc_offset = *offset as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_container(path: &Path, magic: &[u8; 4], entries: &[(u32, &[u8])]) {
        let version = if magic[3] == 0x00 { 1u32 } else { 2u32 };
        let mut data = Vec::new();
        data.extend_from_slice(magic);
        data.extend_from_slice(&(version << 16).to_be_bytes());
        data.extend_from_slice(&[0; 16]);
        data.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        let mut body = 26 + 12 * entries.len();
        let mut table = Vec::new();
        for (id, bytes) in entries {
            table.push((*id, body, bytes.len()));
            body += bytes.len();
        }
        for (id, offset, len) in &table {
            data.extend_from_slice(&id.to_be_bytes());
            data.extend_from_slice(&(*offset as u32).to_be_bytes());
            data.extend_from_slice(&(*len as u32).to_be_bytes());
        }
        for (_, bytes) in entries {
            data.extend_from_slice(bytes);
        }
        fs::write(path, data).unwrap();
    }

    #[test]
    fn test_apple_double_sidecar() {
        let dir = std::env::temp_dir().join("snow-meta-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        fs::write(&file, b"hello").unwrap();
        let sidecar = dir.join("._test.txt");

        let mut finder = [0u8; 32];
        finder[..4].copy_from_slice(b"TEXT");
        let mut dates = Vec::new();
        dates.extend_from_slice(&1u32.to_be_bytes());
        dates.extend_from_slice(&2u32.to_be_bytes());
        dates.extend_from_slice(&3u32.to_be_bytes());
        dates.extend_from_slice(&4u32.to_be_bytes());
        write_container(
            &sidecar,
            &AD_MAGIC_V2,
            &[
                (ENTRY_FINDER_INFO, &finder),
                (ENTRY_RSRC_FORK, b"rsrc"),
                (ENTRY_DATES, &dates),
            ],
        );

        let meta = open(&file);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        assert_eq!(meta.rsrc_fork_len, 4);
        assert_eq!(meta.create_date, 1);
        assert_eq!(meta.mod_date, 2);
        assert_eq!(meta.backup_date, 3);
        assert_eq!(meta.data_len, 5);
        assert_eq!(meta.data_offset, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apple_single() {
        let dir = std::env::temp_dir().join("snow-meta-single");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");

        write_container(&file, &AD_MAGIC_V1, &[(ENTRY_DATA_FORK, b"payload")]);
        let meta = open(&file);
        assert_eq!(meta.data_offset, 26 + 12); // right after the entry table
        assert_eq!(meta.data_len, 7);
        assert_eq!(meta.finder_info, [0; 32]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_plain_file() {
        let dir = std::env::temp_dir().join("snow-meta-plain");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("plain.bin");
        fs::write(&file, b"data").unwrap();

        let meta = open(&file);
        assert_eq!(meta.data_len, 4);
        assert_eq!(meta.finder_info, [0; 32]);
        assert_eq!(meta.rsrc_fork_len, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn synthesized_types_from_extensions() {
        let t = |n: &str| synthesized_finder(Path::new(n)).map(|(ty, cr)| (*ty, *cr));
        assert_eq!(t("notes.txt"), Some((*b"TEXT", *b"ttxt")));
        assert_eq!(t("main.c"), Some((*b"TEXT", *b"ttxt")));
        assert_eq!(t("Makefile"), None); // no extension
        assert_eq!(t("photo.JPG"), Some((*b"JPEG", *b"????")));
        assert_eq!(t("pic.gif"), Some((*b"GIFf", *b"????")));
        assert_eq!(t("doc.pdf"), Some((*b"PDF ", *b"CARO")));
        assert_eq!(t("arch.sit"), Some((*b"SIT!", *b"SIT!")));
        assert_eq!(t("bin.hqx"), Some((*b"TEXT", *b"BnHq")));
        assert_eq!(t("weird.zzz"), None);
    }

    #[test]
    fn open_fills_synthesized_finder_info() {
        let dir = std::env::temp_dir().join("snow-meta-synth");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.txt");
        fs::write(&path, b"hello").unwrap();
        let meta = super::open(&path);
        assert_eq!(&meta.finder_info[0..4], b"TEXT");
        assert_eq!(&meta.finder_info[4..8], b"ttxt");
        let _ = fs::remove_dir_all(&dir);
    }
}
