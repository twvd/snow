//! Metadata storage for volume files
//!
//! Probes for AppleDouble metadata (both the netatalk `.AppleDouble/`
//! directory convention and the macOS `._` sidecar convention) and
//! AppleSingle containers, reading the resource fork, the Finder
//! information and the file dates. Files without metadata fall back to
//! their host file attributes.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// AppleDouble/AppleSingle magic numbers
const AD_MAGIC_V1: [u8; 4] = [0x00, 0x05, 0x16, 0x00];
const AD_MAGIC_V2: [u8; 4] = [0x00, 0x05, 0x16, 0x07];

/// AppleDouble/AppleSingle entry identifiers
const ENTRY_DATA_FORK: u32 = 1;
const ENTRY_RSRC_FORK: u32 = 2;
const ENTRY_DATES: u32 = 8;
const ENTRY_FINDER_INFO: u32 = 9;
/// A private entry holding the AFP file attributes (invisible, locked,
/// ...): the host has no equivalent, and the Finder Info a client
/// writes must not overwrite them, so they do not live in Finder Info
/// flags
const ENTRY_ATTRS: u32 = 100;

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
    /// The AFP file attributes (invisible, locked, ...), 0 for a plain
    /// visible file
    pub attrs: u16,
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

/// Where new metadata is written. The write side is a pluggable
/// backend: a future real-HFS or xattr backend is a new variant plus
/// its write paths (the read side already probes every format).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Backend {
    /// `.AppleDouble/<name>` containers next to the file (the netatalk
    /// convention); the data fork is the host file itself
    #[default]
    AppleDouble,
    /// The file itself is the container; the first metadata write
    /// converts a plain file in place (slower, self-contained)
    AppleSingle,
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
/// holding the metadata of the file at `file_path`.
///
/// Only the header, the entry table and the small entries are read: a
/// container can hold megabytes of resource-fork data that a directory
/// listing never needs.
fn read_apple_double(container: &Path, file_path: &Path) -> Option<Meta> {
    let mut f = fs::File::open(container).ok()?;
    let mut head = [0u8; 26];
    f.read_exact(&mut head).ok()?;
    if !(head.starts_with(&AD_MAGIC_V1) || head.starts_with(&AD_MAGIC_V2)) {
        return None;
    }
    let num_entries = usize::from(u16::from_be_bytes([head[24], head[25]]));
    let mut entries = Vec::with_capacity(num_entries);
    for _ in 0..num_entries {
        let mut entry = [0u8; 12];
        f.read_exact(&mut entry).ok()?;
        entries.push((
            u32::from_be_bytes(entry[..4].try_into().ok()?),
            u32::from_be_bytes(entry[4..8].try_into().ok()?),
            u32::from_be_bytes(entry[8..12].try_into().ok()?),
        ));
    }
    let mut meta = base_meta(file_path);
    for (id, offset, len) in &entries {
        match *id {
            ENTRY_FINDER_INFO => {
                let len = (*len as usize).min(32);
                let mut body = [0u8; 32];
                f.seek(SeekFrom::Start(u64::from(*offset))).ok()?;
                f.read_exact(&mut body[..len]).ok()?;
                meta.finder_info[..len].copy_from_slice(&body[..len]);
            }
            ENTRY_DATES if *len >= 16 => {
                let mut body = [0u8; 16];
                f.seek(SeekFrom::Start(u64::from(*offset))).ok()?;
                f.read_exact(&mut body).ok()?;
                meta.create_date = u32::from_be_bytes(body[..4].try_into().ok()?);
                meta.mod_date = u32::from_be_bytes(body[4..8].try_into().ok()?);
                meta.backup_date = u32::from_be_bytes(body[8..12].try_into().ok()?);
            }
            ENTRY_RSRC_FORK => {
                // The body stays unread: only its size and offset
                // matter until a fork opens
                meta.rsrc_fork_len = *len;
                meta.rsrc_offset = u64::from(*offset);
            }
            ENTRY_ATTRS if *len >= 2 => {
                let mut body = [0u8; 2];
                f.seek(SeekFrom::Start(u64::from(*offset))).ok()?;
                f.read_exact(&mut body).ok()?;
                meta.attrs = u16::from_be_bytes(body);
            }
            _ => {}
        }
    }
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
    if let Some(&(_, offset, len)) = entries.iter().find(|(id, ..)| *id == ENTRY_DATA_FORK) {
        meta.data_offset = u64::from(offset);
        meta.data_len = u64::from(len);
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

/// Parse the entries of an AppleDouble or AppleSingle container:
/// the (id, offset, length) table in its on-disk order, so callers
/// can use the real body offsets
fn parse_container(data: &[u8]) -> Option<Vec<(u32, u32, u32)>> {
    if data.len() < 26 || !(data.starts_with(&AD_MAGIC_V1) || data.starts_with(&AD_MAGIC_V2)) {
        return None;
    }
    let num_entries = u16::from_be_bytes([data[24], data[25]]) as usize;
    let mut entries = Vec::with_capacity(num_entries);
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
        entries.push((id, offset, len));
        pos += 12;
    }
    Some(entries)
}

/// Apply the container entries to the metadata
fn apply_entries(meta: &mut Meta, data: &[u8], entries: &[(u32, u32, u32)]) {
    if let Some(&(_, offset, len)) = entries.iter().find(|(id, ..)| *id == ENTRY_FINDER_INFO) {
        let len = (len as usize).min(32);
        let start = offset as usize;
        if data.len() >= start + len {
            meta.finder_info[..len].copy_from_slice(&data[start..start + len]);
        }
    }
    // The AppleDouble v2 File Dates entry (ID 8) holds the same
    // seconds-since-2000 values AFP uses on the wire
    if let Some(&(_, offset, len)) = entries.iter().find(|(id, ..)| *id == ENTRY_DATES)
        && len >= 16
    {
        let start = offset as usize;
        if data.len() >= start + 16 {
            meta.create_date = u32::from_be_bytes(data[start..start + 4].try_into().unwrap());
            meta.mod_date = u32::from_be_bytes(data[start + 4..start + 8].try_into().unwrap());
            meta.backup_date = u32::from_be_bytes(data[start + 8..start + 12].try_into().unwrap());
        }
    }
    if let Some(&(_, offset, len)) = entries.iter().find(|(id, ..)| *id == ENTRY_RSRC_FORK) {
        meta.rsrc_fork_len = len;
        meta.rsrc_offset = u64::from(offset);
    }
    if let Some(&(_, offset, len)) = entries.iter().find(|(id, ..)| *id == ENTRY_ATTRS)
        && len >= 2
    {
        let start = offset as usize;
        if data.len() >= start + 2 {
            meta.attrs = u16::from_be_bytes(data[start..start + 2].try_into().unwrap());
        }
    }
}

// ---------------------------------------------------------------------
// The write side: metadata containers
// ---------------------------------------------------------------------

use std::io::{self, Read, Seek, SeekFrom, Write};

/// An AppleDouble or AppleSingle container loaded into memory
struct Container {
    magic: [u8; 4],
    /// The entries in table order with their real on-disk body offsets
    /// (foreign containers do not pack the bodies right after the
    /// table, so the offsets must be honored)
    entries: Vec<(u32, u64, Vec<u8>)>,
}

impl Container {
    /// Parse a container, or None when the data is not one
    fn load(data: &[u8]) -> Option<Self> {
        let entries = parse_container(data)?;
        let mut out = Vec::with_capacity(entries.len());
        for (id, offset, len) in entries {
            let body = data.get(offset as usize..(offset + len) as usize)?.to_vec();
            out.push((id, u64::from(offset), body));
        }
        Some(Self {
            magic: data[..4].try_into().ok()?,
            entries: out,
        })
    }

    fn get(&self, id: u32) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(eid, ..)| *eid == id)
            .map(|(_, _, body)| body.as_slice())
    }

    /// Replace the entry body, appending the entry when absent
    fn set(&mut self, id: u32, body: &[u8]) {
        if let Some((_, _, old)) = self.entries.iter_mut().find(|(eid, ..)| *eid == id) {
            *old = body.to_vec();
        } else {
            self.entries.push((id, 0, body.to_vec()));
        }
    }

    /// Whether the entry's body is the last one in the container (no
    /// other non-empty body starts at or after its offset), so it can
    /// grow in place
    fn grows_in_place(&self, id: u32) -> bool {
        let Some(&(_, off, _)) = self.entries.iter().find(|(eid, ..)| *eid == id) else {
            return false;
        };
        !self
            .entries
            .iter()
            .any(|(eid, o, b)| *eid != id && *o >= off && !b.is_empty())
    }

    /// The entry's real body offset in the loaded container
    fn body_offset(&self, id: u32) -> Option<u64> {
        self.entries
            .iter()
            .find(|(eid, ..)| *eid == id)
            .map(|(_, off, _)| *off)
    }

    fn to_bytes(&self) -> Vec<u8> {
        let version: u32 = if self.magic == AD_MAGIC_V2 {
            0x00020000
        } else {
            0x00010000
        };
        let mut out = Vec::new();
        out.extend_from_slice(&self.magic);
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&(self.entries.len() as u16).to_be_bytes());
        let mut body = 26 + 12 * self.entries.len();
        for (id, _, bytes) in &self.entries {
            out.extend_from_slice(&id.to_be_bytes());
            out.extend_from_slice(&(body as u32).to_be_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            body += (bytes.len() + 3) & !3;
        }
        for (_, _, bytes) in &self.entries {
            out.extend_from_slice(bytes);
            let pad = (4 - bytes.len() % 4) % 4;
            out.extend(std::iter::repeat_n(0, pad));
        }
        out
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        fs::write(path, self.to_bytes())
    }

    /// Patch one entry's length field in a container on disk, in place
    fn patch_len(path: &Path, index: usize, len: u32) -> io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).open(path)?;
        f.seek(SeekFrom::Start((26 + index * 12 + 8) as u64))?;
        f.write_all(&len.to_be_bytes())?;
        Ok(())
    }
}

/// The `._<name>` sidecar path next to a file (the macOS metadata
/// convention the read side probes first)
pub fn sidecar_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let mut sidecar = path.to_path_buf();
    sidecar.set_file_name(format!("._{name}"));
    Some(sidecar)
}

/// Remove a file's `._<name>` sidecar, if there is one
pub fn remove_sidecar(path: &Path) {
    if let Some(sidecar) = sidecar_path(path) {
        let _ = fs::remove_file(&sidecar);
    }
}

/// The `.AppleDouble/<name>` container path for a file
fn double_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let mut dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    dir.push(".AppleDouble");
    dir.push(name);
    Some(dir)
}

/// Load the AppleDouble container for a file, or a fresh empty one
/// An AppleDouble container loaded for writing, plus the migrated
/// `._` sidecar when the load came from one
struct LoadedDouble {
    /// The container's path
    path: PathBuf,
    /// The container
    container: Container,
    /// The sidecar the load migrated: it goes away only once the
    /// container holding its entries is safely saved (removing it
    /// earlier loses the metadata whenever the save never happens)
    migrated_sidecar: Option<PathBuf>,
}

impl LoadedDouble {
    fn remove_migrated_sidecar(&self) {
        if let Some(sidecar) = &self.migrated_sidecar {
            let _ = fs::remove_file(sidecar);
        }
    }
}

fn load_double(path: &Path) -> io::Result<LoadedDouble> {
    let container_path = double_path(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 file name"))?;
    let (container, migrated_sidecar) = match fs::read(&container_path) {
        Ok(data) => (
            Container::load(&data).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "corrupt AppleDouble container")
            })?,
            None,
        ),
        Err(_) => {
            // A fresh container. When a macOS `._` sidecar exists, its
            // entries migrate into the container (reads keep preferring
            // the sidecar otherwise, which would make every write look
            // like a no-op). Otherwise the container is seeded with the
            // file's current (possibly synthesized) Finder information,
            // so writing only dates or attributes does not lose the
            // type.
            let mut container = Container {
                magic: AD_MAGIC_V2,
                entries: Vec::new(),
            };
            let mut migrated_sidecar = None;
            let migrated = match sidecar_path(path) {
                Some(sidecar) if sidecar.exists() => {
                    if let Some(side_meta) = read_apple_double(&sidecar, path) {
                        if side_meta.finder_info != [0; 32] {
                            container.set(ENTRY_FINDER_INFO, &side_meta.finder_info);
                        }
                        if side_meta.rsrc_fork_len != 0 {
                            let side_data = fs::read(&sidecar)?;
                            let start = side_meta.rsrc_offset as usize;
                            let len = side_meta.rsrc_fork_len as usize;
                            let body = side_data.get(start..start + len).ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "corrupt AppleDouble sidecar",
                                )
                            })?;
                            container.set(ENTRY_RSRC_FORK, body);
                        }
                        let create = if side_meta.create_date != 0 {
                            side_meta.create_date
                        } else {
                            side_meta.mod_date
                        };
                        container.set(
                            ENTRY_DATES,
                            &dates_body(create, side_meta.mod_date, side_meta.backup_date),
                        );
                        migrated_sidecar = Some(sidecar);
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if !migrated {
                let info = open_with(path, &ProbeFlags::default()).finder_info;
                if info != [0; 32] {
                    container.set(ENTRY_FINDER_INFO, &info);
                }
            }
            (container, migrated_sidecar)
        }
    };
    Ok(LoadedDouble {
        path: container_path,
        container,
        migrated_sidecar,
    })
}

/// Write bytes at an offset in an open file
fn pwrite(file: &mut fs::File, offset: u64, data: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)
}

/// The AppleSingle entry table (id, offset, length), or None when the
/// file is not an AppleSingle container
fn single_table(path: &Path) -> io::Result<Option<Vec<(u32, u64, u64)>>> {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut head = [0u8; 26];
    if f.read_exact(&mut head).is_err() || head[..4] != AD_MAGIC_V1 {
        return Ok(None);
    }
    let count = u16::from_be_bytes([head[24], head[25]]) as usize;
    let mut table = Vec::with_capacity(count);
    for _ in 0..count {
        let mut entry = [0u8; 12];
        f.read_exact(&mut entry)?;
        let id = u32::from_be_bytes(entry[..4].try_into().unwrap());
        let offset = u32::from_be_bytes(entry[4..8].try_into().unwrap()) as u64;
        let len = u32::from_be_bytes(entry[8..12].try_into().unwrap()) as u64;
        table.push((id, offset, len));
    }
    Ok(Some(table))
}

/// Replace one entry of an AppleSingle container, streaming the other
/// bodies so a large data fork never has to be read into memory. The
/// entry order is preserved; a new entry is inserted ahead of the data
/// fork (which stays last, so it can keep growing in place).
fn single_rewrite_entry(path: &Path, entry_id: u32, new_body: &[u8]) -> io::Result<()> {
    let table = single_table(path)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "not an AppleSingle container")
    })?;
    let src = fs::File::open(path)?;
    // A dot-prefixed temporary name in the same directory (a plain
    // "tmpPID" extension could clobber a real file of that name)
    let tmp = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".snowtmp.{}.{}",
            std::process::id(),
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
    {
        // The new entry order and body sources: the replaced entry
        // comes from the new body, the rest stream from the old file
        let mut order: Vec<(u32, u64, Option<u64>)> = Vec::with_capacity(table.len() + 1);
        let mut placed = false;
        for (id, offset, len) in &table {
            if *id == entry_id {
                order.push((*id, new_body.len() as u64, None));
                placed = true;
            } else {
                order.push((*id, *len, Some(*offset)));
            }
        }
        if !placed {
            let at = table
                .iter()
                .position(|(id, _, _)| *id == ENTRY_DATA_FORK)
                .unwrap_or(order.len());
            order.insert(at, (entry_id, new_body.len() as u64, None));
        }

        let mut out = fs::File::create(&tmp)?;
        out.write_all(&AD_MAGIC_V1)?;
        out.write_all(&0x00010000u32.to_be_bytes())?;
        out.write_all(&[0; 16])?;
        out.write_all(&(order.len() as u16).to_be_bytes())?;
        let mut body = 26 + 12 * order.len();
        let mut new_table = Vec::with_capacity(order.len());
        for (id, len, _) in &order {
            new_table.push((*id, body as u64, *len));
            body += (*len as usize + 3) & !3;
        }
        for (id, offset, len) in &new_table {
            out.write_all(&id.to_be_bytes())?;
            out.write_all(&(*offset as u32).to_be_bytes())?;
            out.write_all(&(*len as u32).to_be_bytes())?;
        }
        // The bodies
        let mut src = src;
        for (id, _, source) in &order {
            match source {
                None => {
                    out.write_all(new_body)?;
                    let pad = (4 - new_body.len() % 4) % 4;
                    out.write_all(&[0u8; 4][..pad])?;
                }
                Some(old_offset) => {
                    let (_, _, old_len) =
                        table.iter().find(|(tid, _, _)| tid == id).copied().unwrap();
                    let mut remaining = old_len;
                    let mut buf = [0u8; 64 * 1024];
                    src.seek(SeekFrom::Start(*old_offset))?;
                    while remaining > 0 {
                        let n = remaining.min(buf.len() as u64) as usize;
                        src.read_exact(&mut buf[..n])?;
                        out.write_all(&buf[..n])?;
                        remaining -= n as u64;
                    }
                    let pad = ((4 - (old_len as usize % 4)) % 4) as u64;
                    out.write_all(&[0u8; 4][..pad as usize])?;
                }
            }
        }
        out.flush()?;
    }
    fs::rename(tmp, path)?;
    Ok(())
}

/// Set a host file's modification time from an AFP date (0 = leave it)
#[cfg(unix)]
fn set_host_mtime(path: &Path, afp_seconds: u32) {
    if afp_seconds == 0 {
        return;
    }
    let Ok(c_path) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else {
        return;
    };
    // The AFP date is signed seconds since 2000-01-01 (pre-2000
    // dates arrive as negative values)
    let secs = i64::from(afp_seconds as i32) + AFP_EPOCH_OFFSET as i64;
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        libc::timespec {
            tv_sec: secs as libc::time_t,
            tv_nsec: 0,
        },
    ];
    // SAFETY: c_path is a valid C string, times a valid timespec array
    unsafe {
        libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0);
    }
}

#[cfg(not(unix))]
fn set_host_mtime(_path: &Path, _afp_seconds: u32) {}

/// The current dates of a file (create = modify = the host mtime,
/// backup = never), for seeding a fresh container
fn current_dates(path: &Path) -> (u32, u32, u32) {
    let modify = if let Ok(md) = fs::metadata(path)
        && let Ok(modified) = md.modified()
        && let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH)
    {
        (d.as_secs() as i64 - AFP_EPOCH_OFFSET as i64) as i32 as u32
    } else {
        0
    };
    (modify, modify, 0x8000_0000)
}

/// The 16-byte File Dates entry body (create, modify, backup, access)
fn dates_body(create: u32, modify: u32, backup: u32) -> Vec<u8> {
    let mut dates = Vec::with_capacity(16);
    dates.extend_from_slice(&create.to_be_bytes());
    dates.extend_from_slice(&modify.to_be_bytes());
    dates.extend_from_slice(&backup.to_be_bytes());
    dates.extend_from_slice(&0u32.to_be_bytes()); // access date
    dates
}

/// The file's (device, inode) pair, where the platform exposes one:
/// a container rewrite replaces the file, which changes the identity
/// (on platforms without inodes the identity is path-derived — see
/// ids.rs — and a rewrite does not change it)
#[cfg(unix)]
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let md = fs::metadata(path).ok()?;
    Some((md.dev(), md.ino()))
}

#[cfg(not(unix))]
fn file_identity(_path: &Path) -> Option<(u64, u64)> {
    None
}

impl Backend {
    /// Whether directory metadata is dropped: directories can never
    /// be AppleSingle containers, and the AppleSingle backend is
    /// self-contained (no sidecars), so their Finder info, dates and
    /// attributes have no storage and are dropped
    fn drops_directory_meta(&self, path: &Path) -> bool {
        matches!(self, Self::AppleSingle) && fs::metadata(path).is_ok_and(|m| m.is_dir())
    }

    /// Set the 32-byte Finder information, creating the metadata
    /// container on first use. The AppleSingle backend converts a
    /// plain file into a container (in place) on this first write.
    pub fn set_finder_info(&self, path: &Path, info: &[u8; 32]) -> io::Result<bool> {
        if self.drops_directory_meta(path) {
            return Ok(false);
        }
        // The AppleSingle backend replaces the file when it rewrites
        // or converts a container: the identity change is the signal
        // for the caller to re-bind IDs and reopen the handles
        let before = file_identity(path);
        match self {
            Self::AppleDouble => {
                let mut loaded = load_double(path)?;
                loaded.container.set(ENTRY_FINDER_INFO, info);
                if let Some(parent) = loaded.path.parent() {
                    fs::create_dir_all(parent)?;
                }
                loaded.container.save(&loaded.path)?;
                loaded.remove_migrated_sidecar();
            }
            Self::AppleSingle => match single_table(path)? {
                Some(table) => {
                    if let Some((_, offset, len)) =
                        table.iter().find(|(id, _, _)| *id == ENTRY_FINDER_INFO)
                        && *len >= 32
                    {
                        // Overwrite the fixed-size entry in place
                        let mut f = fs::OpenOptions::new().write(true).open(path)?;
                        pwrite(&mut f, *offset, info)?;
                    } else {
                        single_rewrite_entry(path, ENTRY_FINDER_INFO, info)?;
                    }
                }
                None => {
                    // Convert the plain file: the data fork becomes the
                    // last entry, so it can keep growing in place
                    let data = fs::read(path)?;
                    let (create, modify, backup) = current_dates(path);
                    let mut container = Container {
                        magic: AD_MAGIC_V1,
                        entries: Vec::new(),
                    };
                    container.set(ENTRY_FINDER_INFO, info);
                    container.set(ENTRY_DATES, &dates_body(create, modify, backup));
                    if !data.is_empty() {
                        container.set(ENTRY_DATA_FORK, &data);
                    }
                    container.save(path)?;
                }
            },
        }
        Ok(file_identity(path) != before)
    }

    /// Set the creation, modification and backup dates; the
    /// modification date also touches the host file so the data fork
    /// stays in sync
    pub fn set_dates(
        &self,
        path: &Path,
        create: u32,
        modify: u32,
        backup: u32,
    ) -> io::Result<bool> {
        let dates = dates_body(create, modify, backup);
        let before = file_identity(path);
        if !self.drops_directory_meta(path) {
            match self {
                Self::AppleDouble => {
                    let mut loaded = load_double(path)?;
                    loaded.container.set(ENTRY_DATES, &dates);
                    if let Some(parent) = loaded.path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    loaded.container.save(&loaded.path)?;
                    loaded.remove_migrated_sidecar();
                }
                Self::AppleSingle => match single_table(path)? {
                    Some(table) => {
                        if let Some((_, offset, len)) =
                            table.iter().find(|(id, _, _)| *id == ENTRY_DATES)
                            && *len >= 16
                        {
                            let mut f = fs::OpenOptions::new().write(true).open(path)?;
                            pwrite(&mut f, *offset, &dates)?;
                        } else {
                            single_rewrite_entry(path, ENTRY_DATES, &dates)?;
                        }
                    }
                    None => {
                        let data = fs::read(path)?;
                        let mut container = Container {
                            magic: AD_MAGIC_V1,
                            entries: Vec::new(),
                        };
                        container.set(ENTRY_DATES, &dates);
                        let info = open_with(path, &ProbeFlags::default()).finder_info;
                        if info != [0; 32] {
                            container.set(ENTRY_FINDER_INFO, &info);
                        }
                        if !data.is_empty() {
                            container.set(ENTRY_DATA_FORK, &data);
                        }
                        container.save(path)?;
                    }
                },
            }
        }
        // The host file's modification time follows the AFP date
        set_host_mtime(path, modify);
        Ok(file_identity(path) != before)
    }

    /// Set the AFP file attributes (invisible, locked, ...)
    pub fn set_attrs(&self, path: &Path, attrs: u16) -> io::Result<bool> {
        if self.drops_directory_meta(path) {
            return Ok(false);
        }
        let before = file_identity(path);
        let body = attrs.to_be_bytes();
        match self {
            Self::AppleDouble => {
                let mut loaded = load_double(path)?;
                loaded.container.set(ENTRY_ATTRS, &body);
                if let Some(parent) = loaded.path.parent() {
                    fs::create_dir_all(parent)?;
                }
                loaded.container.save(&loaded.path)?;
                loaded.remove_migrated_sidecar();
            }
            Self::AppleSingle => match single_table(path)? {
                Some(table) => {
                    if let Some((_, offset, len)) =
                        table.iter().find(|(id, _, _)| *id == ENTRY_ATTRS)
                        && *len >= 2
                    {
                        let mut f = fs::OpenOptions::new().write(true).open(path)?;
                        pwrite(&mut f, *offset, &body)?;
                    } else {
                        single_rewrite_entry(path, ENTRY_ATTRS, &body)?;
                    }
                }
                None => {
                    let data = fs::read(path)?;
                    let (create, modify, backup) = current_dates(path);
                    let mut container = Container {
                        magic: AD_MAGIC_V1,
                        entries: Vec::new(),
                    };
                    container.set(ENTRY_ATTRS, &body);
                    container.set(ENTRY_DATES, &dates_body(create, modify, backup));
                    let info = open_with(path, &ProbeFlags::default()).finder_info;
                    if info != [0; 32] {
                        container.set(ENTRY_FINDER_INFO, &info);
                    }
                    if !data.is_empty() {
                        container.set(ENTRY_DATA_FORK, &data);
                    }
                    container.save(path)?;
                }
            },
        }
        Ok(file_identity(path) != before)
    }

    /// Write into the resource fork at the given fork offset (the fork
    /// extends when the write runs past its end), returning the new
    /// fork length and whether the write replaced the file (an
    /// AppleSingle rewrite or conversion: the identity changes and the
    /// open handles die)
    pub fn write_rsrc(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<(u64, bool)> {
        let before = file_identity(path);
        let new_len = self.write_rsrc_inner(path, offset, data)?;
        Ok((new_len, file_identity(path) != before))
    }

    fn write_rsrc_inner(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<u64> {
        match self {
            Self::AppleDouble => {
                let mut loaded = load_double(path)?;
                let container_path = loaded.path.clone();
                let container = &mut loaded.container;
                if let Some(parent) = container_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let old_len = container
                    .get(ENTRY_RSRC_FORK)
                    .map(|body| body.len() as u64)
                    .unwrap_or(0);
                let new_len = offset.saturating_add(data.len() as u64).max(old_len);
                // A migrated container is a rewrite, never in place
                // (its body offsets are not the on-disk layout yet)
                if let Some(index) = container
                    .entries
                    .iter()
                    .position(|(id, ..)| *id == ENTRY_RSRC_FORK)
                    && offset <= old_len
                    && loaded.migrated_sidecar.is_none()
                    && container.grows_in_place(ENTRY_RSRC_FORK)
                {
                    // Inside the fork, which is the last entry (the
                    // shape we create): grow the file in place
                    let body_offset = container.body_offset(ENTRY_RSRC_FORK).unwrap_or(0);
                    let mut f = fs::OpenOptions::new().write(true).open(&container_path)?;
                    pwrite(&mut f, body_offset + offset, data)?;
                    if new_len > old_len {
                        Container::patch_len(&container_path, index, new_len as u32)?;
                    }
                    return Ok(new_len);
                }
                // A new fork, a gap, or a foreign layout: rebuild
                let mut body = container
                    .get(ENTRY_RSRC_FORK)
                    .map(|body| body.to_vec())
                    .unwrap_or_default();
                if offset > body.len() as u64 {
                    body.extend(std::iter::repeat_n(
                        0,
                        (offset - body.len() as u64) as usize,
                    ));
                }
                let end = offset as usize + data.len();
                if end > body.len() {
                    body.resize(end, 0);
                }
                body[offset as usize..end].copy_from_slice(data);
                container.set(ENTRY_RSRC_FORK, &body);
                // Move the resource fork to the end of the table so its
                // body lands last in the rewritten container and the
                // following writes grow in place
                if let Some(pos) = container
                    .entries
                    .iter()
                    .position(|(id, ..)| *id == ENTRY_RSRC_FORK)
                {
                    let entry = container.entries.remove(pos);
                    container.entries.push(entry);
                }
                container.save(&container_path)?;
                loaded.remove_migrated_sidecar();
                Ok(new_len)
            }
            Self::AppleSingle => {
                let Some(table) = single_table(path)? else {
                    // The first resource-fork write converts the plain
                    // file into a container (the whole data fork is
                    // read in full; the self-contained format's cost)
                    let file_data = fs::read(path)?;
                    let (create, modify, backup) = current_dates(path);
                    let mut container = Container {
                        magic: AD_MAGIC_V1,
                        entries: Vec::new(),
                    };
                    container.set(ENTRY_DATES, &dates_body(create, modify, backup));
                    let info = open_with(path, &ProbeFlags::default()).finder_info;
                    if info != [0; 32] {
                        container.set(ENTRY_FINDER_INFO, &info);
                    }
                    let mut body = vec![0u8; offset as usize];
                    body.extend_from_slice(data);
                    container.set(ENTRY_RSRC_FORK, &body);
                    // The data fork goes LAST so it keeps growing in
                    // place
                    if !file_data.is_empty() {
                        container.set(ENTRY_DATA_FORK, &file_data);
                    }
                    container.save(path)?;
                    return Ok(offset.saturating_add(data.len() as u64));
                };
                let rsrc = table.iter().find(|(id, _, _)| *id == ENTRY_RSRC_FORK);
                let old_len = rsrc.map(|(_, _, len)| *len).unwrap_or(0);
                let new_len = offset.saturating_add(data.len() as u64).max(old_len);
                if let Some((_, body_offset, _)) = rsrc
                    && offset <= old_len
                    && new_len == old_len
                {
                    // Fully inside the existing fork: in place
                    let mut f = fs::OpenOptions::new().write(true).open(path)?;
                    return pwrite(&mut f, *body_offset + offset, data).map(|_| new_len);
                }
                // An extension, a gap, or a new fork: rebuild the fork
                // and rewrite the container (the data fork streams)
                let mut body = match rsrc {
                    Some((_, body_offset, old_len)) => {
                        let mut body = vec![0u8; *old_len as usize];
                        let mut f = fs::File::open(path)?;
                        f.seek(SeekFrom::Start(*body_offset))?;
                        f.read_exact(&mut body)?;
                        body
                    }
                    None => Vec::new(),
                };
                if offset > body.len() as u64 {
                    body.extend(std::iter::repeat_n(
                        0,
                        (offset - body.len() as u64) as usize,
                    ));
                }
                let end = offset as usize + data.len();
                if end > body.len() {
                    body.resize(end, 0);
                }
                body[offset as usize..end].copy_from_slice(data);
                single_rewrite_entry(path, ENTRY_RSRC_FORK, &body)?;
                Ok(new_len)
            }
        }
    }

    /// Truncate the resource fork, returning the new fork length and
    /// whether the truncate replaced the file (see write_rsrc)
    pub fn truncate_rsrc(&self, path: &Path, len: u64) -> io::Result<(u64, bool)> {
        let before = file_identity(path);
        let new_len = self.truncate_rsrc_inner(path, len)?;
        Ok((new_len, file_identity(path) != before))
    }

    fn truncate_rsrc_inner(&self, path: &Path, len: u64) -> io::Result<u64> {
        match self {
            Self::AppleDouble => {
                let mut loaded = load_double(path)?;
                let container_path = loaded.path.clone();
                let container = &mut loaded.container;
                let Some(index) = container
                    .entries
                    .iter()
                    .position(|(id, ..)| *id == ENTRY_RSRC_FORK)
                else {
                    // A migrated sidecar must still persist: its Finder
                    // info and dates landed in the fresh container
                    if loaded.migrated_sidecar.is_some() {
                        if let Some(parent) = container_path.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        container.save(&container_path)?;
                        loaded.remove_migrated_sidecar();
                    }
                    return Ok(0);
                };
                let old_len = container
                    .get(ENTRY_RSRC_FORK)
                    .map(|b| b.len() as u64)
                    .unwrap_or(0);
                let len = len.min(old_len);
                // A migrated container is a rewrite, never in place
                if loaded.migrated_sidecar.is_none() && container.grows_in_place(ENTRY_RSRC_FORK) {
                    let body_offset = container.body_offset(ENTRY_RSRC_FORK).unwrap_or(0);
                    let f = fs::OpenOptions::new().write(true).open(&container_path)?;
                    f.set_len(body_offset + len)?;
                    Container::patch_len(&container_path, index, len as u32)?;
                } else {
                    let mut body = container
                        .get(ENTRY_RSRC_FORK)
                        .map(|b| b[..len as usize].to_vec())
                        .unwrap_or_default();
                    body.truncate(len as usize);
                    container.set(ENTRY_RSRC_FORK, &body);
                    if let Some(pos) = container
                        .entries
                        .iter()
                        .position(|(id, ..)| *id == ENTRY_RSRC_FORK)
                    {
                        let entry = container.entries.remove(pos);
                        container.entries.push(entry);
                    }
                    container.save(&container_path)?;
                }
                loaded.remove_migrated_sidecar();
                Ok(len)
            }
            Self::AppleSingle => {
                let Some(table) = single_table(path)? else {
                    return Ok(0);
                };
                let Some((_, body_offset, old_len)) =
                    table.iter().find(|(id, _, _)| *id == ENTRY_RSRC_FORK)
                else {
                    return Ok(0);
                };
                let len = len.min(*old_len);
                if len == *old_len {
                    return Ok(len);
                }
                let mut body = vec![0u8; len as usize];
                let mut f = fs::File::open(path)?;
                f.seek(SeekFrom::Start(*body_offset))?;
                f.read_exact(&mut body)?;
                single_rewrite_entry(path, ENTRY_RSRC_FORK, &body)?;
                Ok(len)
            }
        }
    }

    /// Patch a data fork's length in an AppleSingle container after
    /// the fork grew in place (a no-op for AppleDouble, where the data
    /// fork is the host file itself)
    pub fn patch_data_len(&self, path: &Path, len: u64) -> io::Result<()> {
        if matches!(self, Self::AppleDouble) {
            return Ok(());
        }
        // A plain (unconverted) file is its own data fork: the file
        // length already records it
        let Some(table) = single_table(path)? else {
            return Ok(());
        };
        if let Some((index, _)) = table
            .iter()
            .enumerate()
            .find(|(_, (id, _, _))| *id == ENTRY_DATA_FORK)
        {
            Container::patch_len(path, index, len as u32)?;
        }
        Ok(())
    }

    /// Remove a file's metadata containers (the caller deletes the
    /// file itself); a no-op for AppleSingle, where the file is the
    /// container
    pub fn remove_meta(&self, path: &Path) {
        if matches!(self, Self::AppleSingle) {
            return;
        }
        remove_sidecar(path);
        if let Some(container_path) = double_path(path)
            && fs::remove_file(&container_path).is_ok()
            && let Some(dir) = container_path.parent()
        {
            // Remove the .AppleDouble directory when it is now empty
            let _ = fs::remove_dir(dir);
        }
    }

    /// Move a file's metadata containers along with the file (a no-op
    /// for AppleSingle)
    pub fn move_meta(&self, from: &Path, to: &Path) -> io::Result<()> {
        if matches!(self, Self::AppleSingle) {
            return Ok(());
        }
        // The macOS `._` sidecar moves with the file (reads prefer it;
        // a stale one would take the renamed file's metadata)
        if let (Some(from_side), Some(to_side)) = (sidecar_path(from), sidecar_path(to))
            && from_side.exists()
        {
            fs::rename(&from_side, &to_side)?;
        }
        let (Some(from), Some(to)) = (double_path(from), double_path(to)) else {
            return Ok(());
        };
        if !from.exists() {
            return Ok(());
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(from, to)?;
        Ok(())
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
    fn test_apple_double_writes_migrate_sidecars() {
        let dir = std::env::temp_dir().join("snow-meta-sidecar");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        // A macOS-style ._ sidecar: Finder info, a resource fork and
        // dates
        let mut finder = [0u8; 32];
        finder[..4].copy_from_slice(b"TEXT");
        finder[4..8].copy_from_slice(b"CARO");
        let mut dates = Vec::new();
        dates.extend_from_slice(&10u32.to_be_bytes());
        dates.extend_from_slice(&20u32.to_be_bytes());
        dates.extend_from_slice(&0x8000_0000u32.to_be_bytes());
        dates.extend_from_slice(&0u32.to_be_bytes()); // the access date
        write_container(
            &dir.join("._doc.txt"),
            &AD_MAGIC_V2,
            &[(9, &finder), (2, b"rsrc!"), (8, &dates)],
        );
        // The first write migrates the sidecar into the .AppleDouble
        // container and removes it
        Backend::AppleDouble.set_attrs(&file, 0x0001).unwrap();
        assert!(!dir.join("._doc.txt").exists());
        assert!(dir.join(".AppleDouble").join("doc.txt").exists());
        // Everything the sidecar held is now served from the container
        let meta = open(&file);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        assert_eq!(&meta.finder_info[4..8], b"CARO");
        assert_eq!(meta.rsrc_fork_len, 5);
        assert_eq!(meta.create_date, 10);
        assert_eq!(meta.mod_date, 20);
        assert_eq!(meta.attrs, 0x0001);
        let rsrc_file = meta.rsrc_file.clone().unwrap();
        let raw = fs::read(rsrc_file).unwrap();
        let start = meta.rsrc_offset as usize;
        assert_eq!(&raw[start..start + 5], b"rsrc!");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_truncate_saves_a_migrated_sidecar() {
        let dir = std::env::temp_dir().join("snow-meta-sidecar3");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        // A sidecar with Finder info and no resource fork
        let mut finder = [0u8; 32];
        finder[..4].copy_from_slice(b"TEXT");
        write_container(&dir.join("._doc.txt"), &AD_MAGIC_V2, &[(9, &finder)]);
        // A truncate-to-0 on a fork-less file (what FPSetForkParms
        // does): the migrated Finder info must still persist, and the
        // sidecar goes only once the container is saved
        let _ = Backend::AppleDouble.truncate_rsrc(&file, 0).unwrap();
        assert!(!dir.join("._doc.txt").exists());
        assert!(dir.join(".AppleDouble").join("doc.txt").exists());
        let meta = open(&file);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apple_double_moves_and_removes_sidecars() {
        let dir = std::env::temp_dir().join("snow-meta-sidecar2");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mv.txt");
        fs::write(&file, b"x").unwrap();
        let mut finder = [0u8; 32];
        finder[..4].copy_from_slice(b"TEXT");
        write_container(&dir.join("._mv.txt"), &AD_MAGIC_V2, &[(9, &finder)]);
        // Moving the file moves the sidecar (no container was ever
        // written, so the read side still prefers it)
        let renamed = dir.join("renamed.txt");
        Backend::AppleDouble.move_meta(&file, &renamed).unwrap();
        assert!(!dir.join("._mv.txt").exists());
        assert!(dir.join("._renamed.txt").exists());
        let meta = open(&renamed);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        // Deleting removes the sidecar too
        Backend::AppleDouble.remove_meta(&renamed);
        assert!(!dir.join("._renamed.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_foreign_container_layout_survives_chunked_writes() {
        let dir = std::env::temp_dir().join("snow-meta-foreign");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("foreign");
        fs::write(&file, b"hostdata").unwrap();
        let ad = dir.join(".AppleDouble");
        fs::create_dir_all(&ad).unwrap();
        let container_path = ad.join("foreign");

        // A foreign layout: Finder info, the resource fork, then a
        // trailing attrs entry whose body sits AFTER the fork, at
        // non-packed offsets (the reader must use the real offsets)
        let mut finder = [0u8; 32];
        finder[..4].copy_from_slice(b"TEXT");
        let rsrc = vec![0x11; 100];
        let attrs = 0x8000u16.to_be_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(&AD_MAGIC_V2);
        data.extend_from_slice(&0x00020000u32.to_be_bytes());
        data.extend_from_slice(&[0; 16]);
        data.extend_from_slice(&3u16.to_be_bytes());
        for (id, off, len) in [(9u32, 62u32, 32u32), (2, 164, 100), (100, 264, 2)] {
            data.extend_from_slice(&id.to_be_bytes());
            data.extend_from_slice(&off.to_be_bytes());
            data.extend_from_slice(&len.to_be_bytes());
        }
        data.resize(62, 0);
        data.extend_from_slice(&finder); // -> 94
        data.resize(164, 0);
        data.extend_from_slice(&rsrc); // -> 264
        data.resize(264, 0);
        data.extend_from_slice(&attrs); // -> 266
        fs::write(&container_path, data).unwrap();

        let backend = Backend::AppleDouble;
        // Chunked writes: the first rebuilds (the fork is not the last
        // body), moving the fork to the end; the rest grow in place
        for chunk in 0..8u64 {
            let chunk_data = vec![0x22 + chunk as u8; 100];
            let (new_len, _) = backend.write_rsrc(&file, chunk * 100, &chunk_data).unwrap();
            assert_eq!(new_len, (chunk + 1) * 100);
        }

        // The rewritten container is intact: the other entries
        // survived with their bodies, and the fork holds the last
        // chunk
        let data = fs::read(&container_path).unwrap();
        let entries = parse_container(&data).unwrap();
        let get = |id: u32| -> (usize, usize) {
            let &(_, off, len) = entries.iter().find(|(eid, ..)| *eid == id).unwrap();
            (off as usize, len as usize)
        };
        let (off, len) = get(ENTRY_ATTRS);
        assert_eq!(&data[off..off + len], &0x8000u16.to_be_bytes());
        let (off, len) = get(ENTRY_FINDER_INFO);
        assert_eq!(len, 32);
        assert_eq!(&data[off..off + 4], b"TEXT");
        let (off, len) = get(ENTRY_RSRC_FORK);
        assert_eq!(len, 800);
        // Each chunk landed at its offset (chunk 0 at the start,
        // chunk 7 at the end)
        assert_eq!(&data[off..off + 100], &[0x22; 100]);
        assert_eq!(&data[off + 700..off + 800], &[0x29; 100]);

        // The read side sees the same file
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 800);
        assert_eq!(meta.attrs, 0x8000);
        assert_eq!(&meta.finder_info[..4], b"TEXT");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_pre_2000_dates_round_trip() {
        let dir = std::env::temp_dir().join("snow-meta-old");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("old.txt");
        fs::write(&file, b"x").unwrap();
        // 1995-01-01 in AFP seconds (negative on the wire)
        let afp_1995 = (-157_785_600i32) as u32;
        Backend::AppleDouble
            .set_dates(&file, afp_1995, afp_1995, 0x8000_0000)
            .unwrap();
        let meta = open(&file);
        assert_eq!(meta.create_date as i32, -157_785_600);
        assert_eq!(meta.mod_date as i32, -157_785_600);
        // The host mtime is 1995, not ~2136 (the unsigned bug)
        let host = fs::metadata(&file)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((host - 788_918_400).abs() < 86400, "host mtime {host}");
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

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-meta-{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A test Finder info: type TEXT, creator ttxt
    fn text_finder() -> [u8; 32] {
        let mut finder = [0u8; 32];
        finder[..4].copy_from_slice(b"TEXT");
        finder[4..8].copy_from_slice(b"ttxt");
        finder
    }

    #[test]
    fn double_set_finder_info_creates_the_container() {
        let dir = temp_dir("write-finder");
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        Backend::AppleDouble
            .set_finder_info(&file, &text_finder())
            .unwrap();
        // The container exists and the data fork is untouched
        let container = dir.join(".AppleDouble").join("doc.txt");
        assert!(container.exists());
        assert_eq!(fs::read(&file).unwrap(), b"hello");
        let meta = open(&file);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        assert_eq!(&meta.finder_info[4..8], b"ttxt");
        assert_eq!(meta.data_len, 5);
        assert_eq!(meta.data_offset, 0);
        assert_eq!(meta.rsrc_fork_len, 0);
        // A second write replaces the Finder info
        let mut other = text_finder();
        other[4..8].copy_from_slice(b"CARO");
        Backend::AppleDouble.set_finder_info(&file, &other).unwrap();
        let meta = open(&file);
        assert_eq!(&meta.finder_info[4..8], b"CARO");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn double_rsrc_round_trip() {
        let dir = temp_dir("write-rsrc");
        let file = dir.join("app");
        fs::write(&file, b"data").unwrap();
        // The first write creates the fork
        let (len, _) = Backend::AppleDouble
            .write_rsrc(&file, 0, b"resource")
            .unwrap();
        assert_eq!(len, 8);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 8);
        assert_eq!(meta.data_len, 4);
        // A write inside the fork is in place
        let (len, _) = Backend::AppleDouble.write_rsrc(&file, 4, b"XXXX").unwrap();
        assert_eq!(len, 8);
        // A write past the end extends the fork, zero-filling the gap
        let (len, _) = Backend::AppleDouble.write_rsrc(&file, 10, b"YY").unwrap();
        assert_eq!(len, 12);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 12);
        assert_eq!(meta.data_len, 4);
        let rsrc = fs::read(meta.rsrc_file.as_ref().unwrap()).unwrap();
        let offset = meta.rsrc_offset as usize;
        assert_eq!(&rsrc[offset..offset + 12], b"resoXXXX\0\0YY");
        // Truncate
        let (len, _) = Backend::AppleDouble.truncate_rsrc(&file, 6).unwrap();
        assert_eq!(len, 6);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 6);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn double_dates_touch_the_container_and_the_host_file() {
        let dir = temp_dir("write-dates");
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        Backend::AppleDouble
            .set_dates(&file, 1000, 2000, 0x8000_0000)
            .unwrap();
        let meta = open(&file);
        assert_eq!(meta.create_date, 1000);
        assert_eq!(meta.mod_date, 2000);
        assert_eq!(meta.backup_date, 0x8000_0000);
        // The host mtime follows the modification date
        let md = fs::metadata(&file).unwrap();
        let mtime = md
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(mtime, 2000 + 946684800);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn double_attrs_round_trip() {
        let dir = temp_dir("write-attrs");
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        Backend::AppleDouble.set_attrs(&file, 0x8001).unwrap();
        let meta = open(&file);
        assert_eq!(meta.attrs, 0x8001);
        // The Finder info a client writes does not clobber the attrs
        Backend::AppleDouble
            .set_finder_info(&file, &text_finder())
            .unwrap();
        let meta = open(&file);
        assert_eq!(meta.attrs, 0x8001);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn double_remove_and_move_meta() {
        let dir = temp_dir("write-remove");
        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        let file = sub.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        Backend::AppleDouble
            .set_finder_info(&file, &text_finder())
            .unwrap();
        let container = sub.join(".AppleDouble").join("doc.txt");
        assert!(container.exists());
        // Move
        let moved = dir.join("renamed.txt");
        Backend::AppleDouble.move_meta(&file, &moved).unwrap();
        assert!(!container.exists());
        assert!(dir.join(".AppleDouble").join("renamed.txt").exists());
        // Remove
        Backend::AppleDouble.remove_meta(&moved);
        assert!(!dir.join(".AppleDouble").join("renamed.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_conversion_preserves_the_data_fork() {
        let dir = temp_dir("write-single");
        let file = dir.join("doc.txt");
        fs::write(&file, b"payload data").unwrap();
        Backend::AppleSingle
            .set_finder_info(&file, &text_finder())
            .unwrap();
        // The file is now a container with the data fork inside
        let meta = open(&file);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        assert_eq!(meta.data_len, 12);
        assert!(meta.data_offset > 0);
        let data = fs::read(&file).unwrap();
        let offset = meta.data_offset as usize;
        assert_eq!(&data[offset..offset + 12], b"payload data");
        // The dates survived the conversion (the host mtime)
        assert_ne!(meta.mod_date, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_rsrc_round_trip() {
        let dir = temp_dir("write-single-rsrc");
        let file = dir.join("app");
        fs::write(&file, b"data fork here").unwrap();
        Backend::AppleSingle
            .set_finder_info(&file, &text_finder())
            .unwrap();
        // The first rsrc write inserts the fork ahead of the data fork
        let (len, _) = Backend::AppleSingle.write_rsrc(&file, 0, b"icon!").unwrap();
        assert_eq!(len, 5);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 5);
        assert_eq!(meta.data_len, 14);
        // In-place write inside the fork
        let _ = Backend::AppleSingle.write_rsrc(&file, 1, b"CON").unwrap();
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 5);
        // Extending the fork rewrites the container, the data survives
        let (len, _) = Backend::AppleSingle.write_rsrc(&file, 5, b"smore").unwrap();
        assert_eq!(len, 10);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 10);
        assert_eq!(meta.data_len, 14);
        let data = fs::read(&file).unwrap();
        let offset = meta.data_offset as usize;
        assert_eq!(&data[offset..offset + 14], b"data fork here");
        // Truncate
        let (len, _) = Backend::AppleSingle.truncate_rsrc(&file, 3).unwrap();
        assert_eq!(len, 3);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_dates_on_plain_and_converted_files() {
        let dir = temp_dir("write-single-dates");
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        // On a plain file, set_dates converts it
        Backend::AppleSingle
            .set_dates(&file, 100, 200, 0x8000_0000)
            .unwrap();
        let meta = open(&file);
        assert_eq!(meta.create_date, 100);
        assert_eq!(meta.mod_date, 200);
        assert_eq!(meta.data_len, 5);
        // And again on the container, in place
        Backend::AppleSingle
            .set_dates(&file, 300, 400, 500)
            .unwrap();
        let meta = open(&file);
        assert_eq!(meta.create_date, 300);
        assert_eq!(meta.mod_date, 400);
        assert_eq!(meta.backup_date, 500);
        assert_eq!(meta.data_len, 5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_data_fork_growth_patches_the_entry() {
        let dir = temp_dir("write-single-grow");
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        Backend::AppleSingle
            .set_finder_info(&file, &text_finder())
            .unwrap();
        let meta = open(&file);
        let offset = meta.data_offset;
        // Simulate a data fork write extending the fork: the file
        // grows in place, then the entry length is patched
        let mut f = fs::OpenOptions::new().write(true).open(&file).unwrap();
        use std::io::{Seek, Write};
        f.seek(std::io::SeekFrom::Start(offset + 5)).unwrap();
        f.write_all(b" world").unwrap();
        drop(f);
        Backend::AppleSingle.patch_data_len(&file, 11).unwrap();
        let meta = open(&file);
        assert_eq!(meta.data_len, 11);
        let data = fs::read(&file).unwrap();
        assert_eq!(&data[offset as usize..offset as usize + 11], b"hello world");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backends_are_readable_after_switching() {
        let dir = temp_dir("write-interop");
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        Backend::AppleSingle
            .set_finder_info(&file, &text_finder())
            .unwrap();
        // Reading probes every format regardless of the write backend
        let meta = open(&file);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        assert_eq!(meta.data_len, 5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_containers_keep_the_synthesized_finder_info() {
        let dir = temp_dir("write-seed");
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        // A date-only write creates the container; the synthesized
        // TEXT/ttxt Finder info must survive
        Backend::AppleDouble.set_dates(&file, 1, 2, 3).unwrap();
        let meta = open(&file);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        assert_eq!(meta.create_date, 1);
        // Same for the AppleSingle conversion
        let file2 = dir.join("doc2.txt");
        fs::write(&file2, b"hello").unwrap();
        Backend::AppleSingle.set_attrs(&file2, 0x8001).unwrap();
        let meta = open(&file2);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        assert_eq!(meta.attrs, 0x8001);
        assert_eq!(meta.data_len, 5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_backend_drops_directory_metadata() {
        let dir = temp_dir("write-single-dir");
        let sub = dir.join("folder");
        fs::create_dir(&sub).unwrap();
        // The AppleSingle backend is self-contained: directories have
        // no container, so their metadata is dropped (no sidecars)
        Backend::AppleSingle
            .set_finder_info(&sub, &text_finder())
            .unwrap();
        Backend::AppleSingle.set_attrs(&sub, 0x8001).unwrap();
        assert!(sub.is_dir());
        assert!(!sub.join(".AppleDouble").exists());
        let meta = open(&sub);
        assert_eq!(meta.finder_info, [0; 32]);
        assert_eq!(meta.attrs, 0);
        // The dates have no container either; only the host mtime
        // follows the modification date
        Backend::AppleSingle.set_dates(&sub, 1, 2, 3).unwrap();
        let md = fs::metadata(&sub).unwrap();
        let mtime = md
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(mtime, 2 + 946684800);
        let _ = fs::remove_dir_all(&dir);
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
