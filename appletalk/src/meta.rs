//! Metadata storage for volume files
//!
//! Probes for AppleDouble metadata (both the netatalk `.AppleDouble/`
//! directory convention and the macOS `._` sidecar convention) and
//! AppleSingle containers, reading the resource fork, the Finder
//! information and the file dates. Files without metadata fall back to
//! their host file attributes. New metadata is written as AppleDouble, in
//! `.AppleDouble/`; AppleSingle files are only read.

use std::fs;
use std::path::{Path, PathBuf};

/// The magic numbers of the two formats
const APPLE_SINGLE_MAGIC: [u8; 4] = [0x00, 0x05, 0x16, 0x00];
const APPLE_DOUBLE_MAGIC: [u8; 4] = [0x00, 0x05, 0x16, 0x07];

/// AppleDouble/AppleSingle entry identifiers
const ENTRY_DATA_FORK: u32 = 1;
const ENTRY_RSRC_FORK: u32 = 2;
const ENTRY_DATES: u32 = 8;
const ENTRY_FINDER_INFO: u32 = 9;
/// AFP File Info: the file attributes as a long, the entry netatalk uses
const ENTRY_AFP_INFO: u32 = 14;
/// A private entry earlier versions of this server wrote the attributes
/// to, read when there is no AFP File Info
const ENTRY_PRIVATE_ATTRS: u32 = 100;

/// The WriteInhibit bit of the AFP file attributes
const ATTR_WRITE_INHIBIT: u16 = 0x0020;
/// The DAlreadyOpen and RAlreadyOpen bits of the AFP file attributes
const ATTR_ALREADY_OPEN: u16 = 0x0018;

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
    /// Whether the file is an AppleSingle container: the data fork lives
    /// inside it, so it is read-only here (WriteInhibit is set in attrs)
    pub single: bool,
}

/// Which metadata containers the probe reads. AppleSingle is always
/// recognised: its magic is cheap to peek, and treating a container as a
/// plain file would serve its header as file data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeFlags {
    /// The AppleDouble containers: the ._ sidecars and the
    /// .AppleDouble directories
    pub apple_double: bool,
    /// Whether a directory's metadata may be looked for in its parent
    /// directory. Off for the volume root: its parent is outside the
    /// share.
    pub parent_side: bool,
}

impl Default for ProbeFlags {
    fn default() -> Self {
        Self {
            apple_double: true,
            parent_side: true,
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
    // AppleSingle first: a stale container next to the file must not
    // misdescribe where its data fork is
    if let Some(meta) = read_apple_single(path) {
        return meta;
    }
    let Some(name) = path.file_name() else {
        return Meta::default();
    };

    if flags.apple_double {
        if fs::metadata(path).is_ok_and(|m| m.is_dir()) {
            // A directory keeps its metadata inside itself (netatalk's
            // convention)
            if let Some(meta) = read_apple_double(&parent_container(path), path) {
                return meta;
            }
            if !flags.parent_side {
                return plain_meta(path);
            }
        }

        // macOS convention: a `._<name>` sidecar next to the file
        if let Some(sidecar) = sidecar_path(path)
            && let Some(meta) = read_apple_double(&sidecar, path)
        {
            return meta;
        }

        // netatalk convention: `.AppleDouble/<name>` next to the file. For
        // a directory this is an old container, moved in at its next write.
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

    plain_meta(path)
}

/// The metadata of a file without a container
fn plain_meta(path: &Path) -> Meta {
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
    if !(head.starts_with(&APPLE_SINGLE_MAGIC) || head.starts_with(&APPLE_DOUBLE_MAGIC)) {
        return None;
    }
    let entries = read_entry_table(&mut f, &head)?;
    let mut meta = base_meta(file_path);
    read_small_entries(&mut f, &entries, &mut meta)?;
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

/// The entry table of a container whose 26-byte header was just read
fn read_entry_table(f: &mut fs::File, head: &[u8; 26]) -> Option<Vec<(u32, u32, u32)>> {
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
    Some(entries)
}

/// Apply a container's small entries to the metadata. The fork bodies
/// stay unread: only their size and offset matter until a fork opens.
fn read_small_entries(
    f: &mut fs::File,
    entries: &[(u32, u32, u32)],
    meta: &mut Meta,
) -> Option<()> {
    for (id, offset, len) in entries {
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
                meta.rsrc_fork_len = *len;
                meta.rsrc_offset = u64::from(*offset);
            }
            ENTRY_AFP_INFO if *len >= 4 => {
                let mut body = [0u8; 4];
                f.seek(SeekFrom::Start(u64::from(*offset))).ok()?;
                f.read_exact(&mut body).ok()?;
                // The already-open bits are the server's own, never stored
                meta.attrs = u16::from_be_bytes([body[2], body[3]]) & !ATTR_ALREADY_OPEN;
            }
            ENTRY_PRIVATE_ATTRS
                if *len >= 2 && !entries.iter().any(|(id, ..)| *id == ENTRY_AFP_INFO) =>
            {
                let mut body = [0u8; 2];
                f.seek(SeekFrom::Start(u64::from(*offset))).ok()?;
                f.read_exact(&mut body).ok()?;
                meta.attrs = u16::from_be_bytes(body);
            }
            _ => {}
        }
    }
    Some(())
}

/// Read an AppleSingle container: the data fork lives inside the file
/// itself. Only the magic is read from anything else, so a large plain
/// file costs nothing.
fn read_apple_single(path: &Path) -> Option<Meta> {
    let mut f = fs::File::open(path).ok()?;
    let mut head = [0u8; 26];
    f.read_exact(&mut head).ok()?;
    if !head.starts_with(&APPLE_SINGLE_MAGIC) {
        return None;
    }
    let entries = read_entry_table(&mut f, &head)?;
    let mut meta = Meta::default();
    read_small_entries(&mut f, &entries, &mut meta)?;
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
    meta.single = true;
    meta.attrs |= ATTR_WRITE_INHIBIT;
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
    if data.len() < 26
        || !(data.starts_with(&APPLE_SINGLE_MAGIC) || data.starts_with(&APPLE_DOUBLE_MAGIC))
    {
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

// ---------------------------------------------------------------------
// The write side: metadata containers
// ---------------------------------------------------------------------

use std::io::{self, Read, Seek, SeekFrom, Write};

/// An AppleDouble or AppleSingle container loaded into memory
struct Container {
    /// The magic, the version and the filler, kept as they were: a foreign
    /// container's filler names who wrote it
    head: [u8; 24],
    /// The entries in table order with their real on-disk body offsets
    /// (foreign containers do not pack the bodies right after the
    /// table, so the offsets must be honored)
    entries: Vec<(u32, u64, Vec<u8>)>,
}

impl Container {
    /// An empty AppleDouble (version 2) container
    fn new() -> Self {
        let mut head = [0u8; 24];
        head[..4].copy_from_slice(&APPLE_DOUBLE_MAGIC);
        head[4..8].copy_from_slice(&0x0002_0000u32.to_be_bytes());
        Self {
            head,
            entries: Vec::new(),
        }
    }

    /// Parse a container, or None when the data is not one
    fn load(data: &[u8]) -> Option<Self> {
        let entries = parse_container(data)?;
        let mut out = Vec::with_capacity(entries.len());
        for (id, offset, len) in entries {
            let end = (offset as usize).checked_add(len as usize)?;
            let body = data.get(offset as usize..end)?.to_vec();
            out.push((id, u64::from(offset), body));
        }
        Some(Self {
            head: data[..24].try_into().ok()?,
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
        let mut out = Vec::new();
        out.extend_from_slice(&self.head);
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

    /// Replace the container on disk. The new one is complete before
    /// it takes the old one's place: a resource fork in it may be the
    /// only copy, and a crash or a full disk must not lose it.
    fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_file_name(format!(".snowtmp.{}", std::process::id()));
        let written = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&self.to_bytes())?;
            f.sync_all()?;
            if let Ok(md) = fs::metadata(path) {
                let _ = fs::set_permissions(&tmp, md.permissions());
            }
            fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
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
    // By bytes: a host name need not be UTF-8
    let mut name = std::ffi::OsString::from("._");
    name.push(path.file_name()?);
    Some(path.with_file_name(name))
}

/// Remove a file's `._<name>` sidecar, if there is one
pub fn remove_sidecar(path: &Path) {
    if let Some(sidecar) = sidecar_path(path) {
        let _ = fs::remove_file(&sidecar);
    }
}

/// The container of a directory: inside the directory itself, so the
/// volume root's stays inside the share (netatalk's convention)
fn parent_container(dir: &Path) -> PathBuf {
    dir.join(".AppleDouble").join(".Parent")
}

/// The container path for a file or a directory
fn double_path(path: &Path) -> Option<PathBuf> {
    if fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        return Some(parent_container(path));
    }
    let name = path.file_name()?;
    let mut dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    dir.push(".AppleDouble");
    dir.push(name);
    Some(dir)
}

/// Create the .AppleDouble directory a container goes in. Metadata is
/// not a change to the folder holding it, so that folder keeps its
/// modification date.
fn create_double_dir(container: &Path) -> io::Result<()> {
    let Some(double_dir) = container.parent() else {
        return Ok(());
    };
    if double_dir.exists() {
        return Ok(());
    }
    let owner = double_dir.parent();
    let mtime = owner.and_then(|o| fs::metadata(o).and_then(|m| m.modified()).ok());
    fs::create_dir_all(double_dir)?;
    if let (Some(owner), Some(mtime)) = (owner, mtime)
        && let Ok(f) = fs::File::open(owner)
    {
        let _ = f.set_modified(mtime);
    }
    Ok(())
}

/// An AppleDouble container loaded for writing, plus the `._` sidecar it
/// absorbed, if any
struct LoadedDouble {
    /// The container's path
    path: PathBuf,
    /// The container
    container: Container,
    /// The sidecar the load merged: it goes away only once the
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

/// Load the container for a file, or a fresh one.
///
/// A file's macOS `._` sidecar merges into it: reads prefer the sidecar, so what it holds is
/// what the client sees, and it would make every later write look like a
/// no-op. A directory's sidecar never merges: its container comes first
/// on the read side, and the volume root's sidecar is outside the share.
fn load_double(path: &Path) -> io::Result<LoadedDouble> {
    let container_path = double_path(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let is_dir = fs::metadata(path).is_ok_and(|m| m.is_dir());
    // Only a missing container is a fresh one: any other read error would
    // replace metadata that may still be there
    let mut container = match fs::read(&container_path) {
        Ok(data) => Container::load(&data).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "corrupt AppleDouble container")
        })?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Container::new(),
        Err(e) => return Err(e),
    };
    let fresh = container.entries.is_empty();
    let mut migrated_sidecar = None;
    if !is_dir
        && let Some(sidecar) = sidecar_path(path)
        && sidecar.exists()
        && let Some(side_meta) = read_apple_double(&sidecar, path)
    {
        if side_meta.finder_info != [0; 32] {
            container.set(ENTRY_FINDER_INFO, &side_meta.finder_info);
        }
        if side_meta.rsrc_fork_len != 0 {
            let side_data = fs::read(&sidecar)?;
            let start = side_meta.rsrc_offset as usize;
            let len = side_meta.rsrc_fork_len as usize;
            let body = side_data.get(start..start + len).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "corrupt AppleDouble sidecar")
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
    } else if fresh {
        // Seed with the current (possibly synthesized) Finder
        // information, so writing only dates or attributes does not lose
        // the type
        let info = open_with(
            path,
            &ProbeFlags {
                apple_double: true,
                parent_side: !is_dir,
            },
        )
        .finder_info;
        if info != [0; 32] {
            container.set(ENTRY_FINDER_INFO, &info);
        }
    }
    Ok(LoadedDouble {
        path: container_path,
        container,
        migrated_sidecar,
    })
}

/// Overwrite an existing entry where it is, when the new body fits.
///
/// Most metadata writes are this one: the other entries are not touched,
/// a foreign container (netatalk's fixed layout) keeps its shape, and a
/// failure cannot take a resource fork with it. False when the container
/// or a big enough entry is not there.
fn patch_entry(container: &Path, id: u32, body: &[u8]) -> io::Result<bool> {
    let mut f = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(container)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut head = [0u8; 26];
    if f.read_exact(&mut head).is_err() || !head.starts_with(&APPLE_DOUBLE_MAGIC) {
        return Ok(false);
    }
    let Some(entries) = read_entry_table(&mut f, &head) else {
        return Ok(false);
    };
    let Some(&(_, offset, _)) = entries
        .iter()
        .find(|(eid, _, len)| *eid == id && *len as usize >= body.len())
    else {
        return Ok(false);
    };
    pwrite(&mut f, u64::from(offset), body)?;
    Ok(true)
}

/// Set one small metadata entry
fn set_entry(path: &Path, id: u32, body: &[u8]) -> io::Result<()> {
    // A file's sidecar has to merge first, which takes a full load
    let has_sidecar = !fs::metadata(path).is_ok_and(|m| m.is_dir())
        && sidecar_path(path).is_some_and(|s| s.exists());
    if !has_sidecar
        && let Some(container) = double_path(path)
        && patch_entry(&container, id, body)?
    {
        return Ok(());
    }
    let mut loaded = load_double(path)?;
    loaded.container.set(id, body);
    save_double(&loaded)
}

/// Bring a directory's metadata inside it.
///
/// A container this server wrote to `<parent>/.AppleDouble/<name>` earlier moves in (one already inside
/// wins, and the old one goes), and a `._` sidecar's contents are copied
/// in so its Finder information survives the first write. The caller
/// never passes the volume root: its parent is outside the share.
pub fn migrate_dir_container(dir: &Path) -> io::Result<()> {
    let Some(name) = dir.file_name() else {
        return Ok(());
    };
    let Some(parent) = dir.parent() else {
        return Ok(());
    };
    let inside = parent_container(dir);
    let outside_dir = parent.join(".AppleDouble");
    let outside = outside_dir.join(name);
    if outside.is_file() {
        if inside.exists() {
            fs::remove_file(&outside)?;
        } else {
            create_double_dir(&inside)?;
            fs::rename(&outside, &inside)?;
        }
        // Remove the .AppleDouble directory when it is now empty
        let _ = fs::remove_dir(&outside_dir);
    } else if !inside.exists()
        && let Some(sidecar) = sidecar_path(dir)
        && sidecar.is_file()
    {
        create_double_dir(&inside)?;
        fs::copy(&sidecar, &inside)?;
    }
    Ok(())
}

/// Write bytes at an offset in an open file
fn pwrite(file: &mut fs::File, offset: u64, data: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)
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

/// The 16-byte File Dates entry body (create, modify, backup, access)
fn dates_body(create: u32, modify: u32, backup: u32) -> Vec<u8> {
    let mut dates = Vec::with_capacity(16);
    dates.extend_from_slice(&create.to_be_bytes());
    dates.extend_from_slice(&modify.to_be_bytes());
    dates.extend_from_slice(&backup.to_be_bytes());
    dates.extend_from_slice(&0u32.to_be_bytes()); // access date
    dates
}

/// Save a loaded container, creating the .AppleDouble directory on first
/// use; a sidecar it was migrated from goes away only now
fn save_double(loaded: &LoadedDouble) -> io::Result<()> {
    create_double_dir(&loaded.path)?;
    loaded.container.save(&loaded.path)?;
    loaded.remove_migrated_sidecar();
    Ok(())
}

/// Set the 32-byte Finder information, creating the metadata container
/// on first use
pub fn set_finder_info(path: &Path, info: &[u8; 32]) -> io::Result<()> {
    set_entry(path, ENTRY_FINDER_INFO, info)
}

/// Set the creation, modification and backup dates; the modification
/// date also touches the host file so the data fork stays in sync
pub fn set_dates(path: &Path, create: u32, modify: u32, backup: u32) -> io::Result<()> {
    set_entry(path, ENTRY_DATES, &dates_body(create, modify, backup))?;
    set_host_mtime(path, modify);
    Ok(())
}

/// Set the AFP file attributes (invisible, locked, ...)
pub fn set_attrs(path: &Path, attrs: u16) -> io::Result<()> {
    set_entry(path, ENTRY_AFP_INFO, &u32::from(attrs).to_be_bytes())
}

/// Write into a resource fork that can take it where it is: its body is
/// the last in the container (the shape we create, and netatalk's), and the
/// write leaves no gap. Only the entry table is read: a large fork arrives
/// in thousands of small writes. None when a rebuild is needed.
fn write_rsrc_in_place(container: &Path, offset: u64, data: &[u8]) -> io::Result<Option<u64>> {
    let mut f = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(container)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut head = [0u8; 26];
    if f.read_exact(&mut head).is_err() || !head.starts_with(&APPLE_DOUBLE_MAGIC) {
        return Ok(None);
    }
    let Some(entries) = read_entry_table(&mut f, &head) else {
        return Ok(None);
    };
    let Some(index) = entries.iter().position(|(id, ..)| *id == ENTRY_RSRC_FORK) else {
        return Ok(None);
    };
    let (_, body, len) = entries[index];
    let last = !entries
        .iter()
        .any(|(id, off, l)| *id != ENTRY_RSRC_FORK && *off >= body && *l != 0);
    if !last || offset > u64::from(len) {
        return Ok(None);
    }
    pwrite(&mut f, u64::from(body) + offset, data)?;
    let new_len = offset.saturating_add(data.len() as u64).max(u64::from(len));
    if new_len > u64::from(len) {
        drop(f);
        Container::patch_len(container, index, new_len as u32)?;
    }
    Ok(Some(new_len))
}

/// Write into the resource fork at the given fork offset (the fork
/// extends when the write runs past its end), returning the new fork
/// length
pub fn write_rsrc(path: &Path, offset: u64, data: &[u8]) -> io::Result<u64> {
    // A sidecar has to merge first, which takes a full load
    if !sidecar_path(path).is_some_and(|s| s.exists())
        && let Some(container) = double_path(path)
        && let Some(new_len) = write_rsrc_in_place(&container, offset, data)?
    {
        return Ok(new_len);
    }
    let mut loaded = load_double(path)?;
    let container_path = loaded.path.clone();
    let container = &mut loaded.container;
    create_double_dir(&container_path)?;
    let old_len = container
        .get(ENTRY_RSRC_FORK)
        .map(|body| body.len() as u64)
        .unwrap_or(0);
    let new_len = offset.saturating_add(data.len() as u64).max(old_len);
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

/// Set the resource fork's length: truncate it, or extend it with zeros,
/// returning the new fork length
pub fn set_rsrc_len(path: &Path, len: u64) -> io::Result<u64> {
    let mut loaded = load_double(path)?;
    let container_path = loaded.path.clone();
    let container = &mut loaded.container;
    let Some(index) = container
        .entries
        .iter()
        .position(|(id, ..)| *id == ENTRY_RSRC_FORK)
    else {
        if len > 0 {
            // A new fork of that length
            return write_rsrc(path, len, &[]);
        }
        // A migrated sidecar must still persist: its Finder
        // info and dates landed in the fresh container
        if loaded.migrated_sidecar.is_some() {
            create_double_dir(&container_path)?;
            container.save(&container_path)?;
            loaded.remove_migrated_sidecar();
        }
        return Ok(0);
    };
    // A migrated container is a rewrite, never in place
    if loaded.migrated_sidecar.is_none() && container.grows_in_place(ENTRY_RSRC_FORK) {
        let body_offset = container.body_offset(ENTRY_RSRC_FORK).unwrap_or(0);
        let f = fs::OpenOptions::new().write(true).open(&container_path)?;
        f.set_len(body_offset + len)?;
        Container::patch_len(&container_path, index, len as u32)?;
    } else {
        let mut body = container
            .get(ENTRY_RSRC_FORK)
            .map(|b| b.to_vec())
            .unwrap_or_default();
        body.resize(len as usize, 0);
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

/// Swap two host paths. Atomic where the host has a call for it; the three
/// renames otherwise, on a file system or platform without one.
pub fn exchange_paths(a: &Path, b: &Path) -> io::Result<()> {
    if exchange_atomically(a, b)? {
        return Ok(());
    }
    let tmp = a.with_file_name(format!(".snowswap.{}", std::process::id()));
    fs::rename(a, &tmp)?;
    if let Err(e) = fs::rename(b, a) {
        let _ = fs::rename(&tmp, a);
        return Err(e);
    }
    fs::rename(&tmp, b)
}

/// The host's atomic swap. False when it cannot do it here, which is not an
/// error yet.
#[cfg(target_os = "linux")]
fn exchange_atomically(a: &Path, b: &Path) -> io::Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: valid NUL-terminated paths and constants
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    swap_result(rc == 0)
}

#[cfg(target_os = "macos")]
fn exchange_atomically(a: &Path, b: &Path) -> io::Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: valid NUL-terminated paths and constants
    let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
    swap_result(rc == 0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exchange_atomically(_a: &Path, _b: &Path) -> io::Result<bool> {
    Ok(false)
}

/// The result of a swap call, read from errno on failure
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn swap_result(done: bool) -> io::Result<bool> {
    if done {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EINVAL | libc::ENOSYS | libc::ENOTSUP) => Ok(false),
        _ => Err(e),
    }
}

/// Swap two files' metadata containers along with the files. Containers
/// are found by name, so what has one and what has none swap too. A sidecar
/// that is not a pair simply moves.
pub fn exchange_meta(a: &Path, b: &Path) -> io::Result<()> {
    for (from, to) in [
        (sidecar_path(a), sidecar_path(b)),
        (double_path(a), double_path(b)),
    ] {
        let (Some(from), Some(to)) = (from, to) else {
            continue;
        };
        match (from.exists(), to.exists()) {
            (true, true) => exchange_paths(&from, &to)?,
            (true, false) => {
                create_double_dir(&to)?;
                fs::rename(&from, &to)?;
            }
            (false, true) => {
                create_double_dir(&from)?;
                fs::rename(&to, &from)?;
            }
            (false, false) => {}
        }
    }
    Ok(())
}

/// Remove a file's metadata containers (the caller deletes the file
/// itself)
pub fn remove_meta(path: &Path) {
    remove_sidecar(path);
    if let Some(container_path) = double_path(path)
        && fs::remove_file(&container_path).is_ok()
        && let Some(dir) = container_path.parent()
    {
        // Remove the .AppleDouble directory when it is now empty
        let _ = fs::remove_dir(dir);
    }
}

/// Move a file's metadata containers along with the file
pub fn move_meta(from: &Path, to: &Path) -> io::Result<()> {
    // The macOS `._` sidecar moves with the file (reads prefer it;
    // a stale one would take the renamed file's metadata)
    if let (Some(from_side), Some(to_side)) = (sidecar_path(from), sidecar_path(to))
        && from_side.exists()
    {
        fs::rename(&from_side, &to_side)?;
    }
    // A directory's own container moved with it; what is left to move is
    // an old one in its parent, which must not replace that
    let (Some(from), Some(to_container)) = (double_path(from), double_path(to)) else {
        return Ok(());
    };
    if !from.exists() {
        return Ok(());
    }
    if fs::metadata(to).is_ok_and(|m| m.is_dir()) && to_container.exists() {
        fs::remove_file(&from)?;
    } else {
        create_double_dir(&to_container)?;
        fs::rename(&from, &to_container)?;
    }
    if let Some(dir) = from.parent() {
        let _ = fs::remove_dir(dir);
    }
    Ok(())
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
            &APPLE_DOUBLE_MAGIC,
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

        write_container(&file, &APPLE_SINGLE_MAGIC, &[(ENTRY_DATA_FORK, b"payload")]);
        let meta = open(&file);
        assert_eq!(meta.data_offset, 26 + 12); // right after the entry table
        assert_eq!(meta.data_len, 7);
        assert_eq!(meta.finder_info, [0; 32]);
        // The data fork lives inside: it is read-only here
        assert!(meta.single);
        assert_eq!(meta.attrs, ATTR_WRITE_INHIBIT);

        // A stale container next to it does not change that
        let stale = dir.join(".AppleDouble");
        fs::create_dir_all(&stale).unwrap();
        write_container(
            &stale.join("test.txt"),
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_FINDER_INFO, &[1; 32])],
        );
        let meta = open(&file);
        assert_eq!(meta.data_offset, 26 + 12);
        assert_eq!(meta.finder_info, [0; 32]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_directory_reads_its_parent_container() {
        let dir = std::env::temp_dir().join("snow-meta-parent");
        let _ = fs::remove_dir_all(&dir);
        let sub = dir.join("sub");
        fs::create_dir_all(sub.join(".AppleDouble")).unwrap();
        let mut finder = [0u8; 32];
        finder[..8].copy_from_slice(&[0, 0xBB, 0, 0x8C, 1, 0xAA, 2, 0x38]);
        write_container(
            &sub.join(".AppleDouble/.Parent"),
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_FINDER_INFO, &finder)],
        );
        assert_eq!(open(&sub).finder_info, finder);

        let _ = fs::remove_dir_all(&dir);
    }

    /// A netatalk-shaped container: fixed offsets, with the empty resource
    /// fork last, at 741
    fn netatalk_container(path: &Path, finder: &[u8; 32]) {
        let mut data = vec![0u8; 741];
        data[..4].copy_from_slice(&APPLE_DOUBLE_MAGIC);
        data[4..8].copy_from_slice(&0x00020000u32.to_be_bytes());
        data[24..26].copy_from_slice(&4u16.to_be_bytes());
        let table: [(u32, u32, u32); 4] = [
            (ENTRY_RSRC_FORK, 741, 0),
            (ENTRY_DATES, 637, 16),
            (ENTRY_FINDER_INFO, 653, 32),
            (14, 689, 4),
        ];
        for (i, (id, off, len)) in table.iter().enumerate() {
            let at = 26 + i * 12;
            data[at..at + 4].copy_from_slice(&id.to_be_bytes());
            data[at + 4..at + 8].copy_from_slice(&off.to_be_bytes());
            data[at + 8..at + 12].copy_from_slice(&len.to_be_bytes());
        }
        data[653..685].copy_from_slice(finder);
        data[689..693].copy_from_slice(&[0, 0, 1, 0xA1]);
        fs::write(path, data).unwrap();
    }

    #[test]
    fn test_directory_metadata_lives_inside_the_directory() {
        let dir = temp_dir("dir-inside");
        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        let mut info = [0u8; 32];
        info[8] = 0x40;
        set_finder_info(&sub, &info).unwrap();
        assert!(sub.join(".AppleDouble/.Parent").is_file());
        assert!(!dir.join(".AppleDouble").exists());
        assert_eq!(open(&sub).finder_info, info);
        // A file next to it keeps the usual place
        let file = dir.join("file");
        fs::write(&file, b"x").unwrap();
        set_finder_info(&file, &info).unwrap();
        assert!(dir.join(".AppleDouble/file").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_netatalk_parent_reads_and_keeps_its_layout() {
        let dir = temp_dir("dir-netatalk");
        let sub = dir.join("sub");
        fs::create_dir_all(sub.join(".AppleDouble")).unwrap();
        let parent = sub.join(".AppleDouble/.Parent");
        let mut info = [0u8; 32];
        info[..8].copy_from_slice(&[0, 0xBB, 0, 0x8C, 1, 0xAA, 2, 0x38]);
        netatalk_container(&parent, &info);
        assert_eq!(open(&sub).finder_info, info);
        // A Finder info write stays in place: every other byte is kept
        let before = fs::read(&parent).unwrap();
        info[0] = 0x55;
        set_finder_info(&sub, &info).unwrap();
        let after = fs::read(&parent).unwrap();
        assert_eq!(after.len(), 741);
        assert_eq!(after[653], 0x55);
        assert_eq!(before[..653], after[..653]);
        assert_eq!(before[654..], after[654..]);
        assert_eq!(open(&sub).finder_info, info);
        // The attributes are netatalk's AFP File Info entry
        assert_eq!(open(&sub).attrs, 0x01A1);
        set_attrs(&sub, 0x0001).unwrap();
        let after = fs::read(&parent).unwrap();
        assert_eq!(after.len(), 741);
        assert_eq!(&after[689..693], &[0, 0, 0, 1]);
        assert_eq!(open(&sub).attrs, 0x0001);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_writing_a_folder_keeps_its_modification_date() {
        let dir = temp_dir("dir-mtime");
        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs::File::open(&sub).unwrap().set_modified(old).unwrap();
        set_finder_info(&sub, &[1; 32]).unwrap();
        assert_eq!(fs::metadata(&sub).unwrap().modified().unwrap(), old);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_afp_info_wins_and_the_private_entry_still_reads() {
        let dir = temp_dir("attrs-entries");
        let file = dir.join("f");
        fs::write(&file, b"x").unwrap();
        fs::create_dir(dir.join(".AppleDouble")).unwrap();
        let container = dir.join(".AppleDouble/f");
        // What this server wrote before
        write_container(
            &container,
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_PRIVATE_ATTRS, &0x0001u16.to_be_bytes())],
        );
        assert_eq!(open(&file).attrs, 0x0001);
        // A write adds the standard entry, which wins from then on
        set_attrs(&file, 0x0100).unwrap();
        assert_eq!(open(&file).attrs, 0x0100);
        // An already-open bit in a foreign entry is not believed
        write_container(
            &container,
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_AFP_INFO, &0x0000_0029u32.to_be_bytes())],
        );
        assert_eq!(open(&file).attrs, 0x0021);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_a_rewrite_keeps_a_foreign_filler() {
        let dir = temp_dir("filler");
        let file = dir.join("f");
        fs::write(&file, b"x").unwrap();
        fs::create_dir(dir.join(".AppleDouble")).unwrap();
        let container = dir.join(".AppleDouble/f");
        write_container(
            &container,
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_FINDER_INFO, &[1; 32])],
        );
        let mut data = fs::read(&container).unwrap();
        data[8..24].copy_from_slice(b"Netatalk        ");
        fs::write(&container, data).unwrap();
        // A new entry forces a rewrite
        set_dates(&file, 1, 2, 3).unwrap();
        assert_eq!(&fs::read(&container).unwrap()[8..24], b"Netatalk        ");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_a_non_utf8_host_name_keeps_its_metadata() {
        use std::os::unix::ffi::OsStrExt;
        let dir = temp_dir("non-utf8");
        // MacRoman bytes, as vintage trees often have
        let file = dir.join(std::ffi::OsStr::from_bytes(b"caf\x8E.txt"));
        fs::write(&file, vec![1u8; 1000]).unwrap();
        let meta = open(&file);
        assert_eq!(meta.data_len, 1000);
        assert_ne!(meta.mod_date, 0);
        set_finder_info(&file, &[1; 32]).unwrap();
        assert_eq!(open(&file).finder_info, [1; 32]);
        assert_eq!(open(&file).data_len, 1000);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_a_netatalk_container_takes_a_resource_fork_in_place() {
        let dir = temp_dir("rsrc-netatalk");
        let file = dir.join("f");
        fs::write(&file, b"x").unwrap();
        fs::create_dir(dir.join(".AppleDouble")).unwrap();
        let container = dir.join(".AppleDouble/f");
        netatalk_container(&container, &[1; 32]);
        let before = fs::read(&container).unwrap();
        assert_eq!(write_rsrc(&file, 0, b"RSRC").unwrap(), 4);
        assert_eq!(write_rsrc(&file, 4, b"MORE").unwrap(), 8);
        let after = fs::read(&container).unwrap();
        // Everything but the fork's length field stays where it was
        assert_eq!(after.len(), 741 + 8);
        assert_eq!(&after[741..], b"RSRCMORE");
        assert_eq!(before[..34], after[..34]);
        assert_eq!(before[38..741], after[38..741]);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 8);
        assert_eq!(meta.rsrc_offset, 741);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_old_directory_container_moves_inside() {
        let dir = temp_dir("dir-migrate");
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir(dir.join(".AppleDouble")).unwrap();
        let outside = dir.join(".AppleDouble/sub");
        write_container(
            &outside,
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_FINDER_INFO, &[7; 32])],
        );
        // Still read where it is
        assert_eq!(open(&sub).finder_info, [7; 32]);
        migrate_dir_container(&sub).unwrap();
        assert!(!outside.exists());
        assert!(!dir.join(".AppleDouble").exists());
        assert_eq!(open(&sub).finder_info, [7; 32]);
        // One already inside wins
        fs::create_dir(dir.join(".AppleDouble")).unwrap();
        write_container(
            &outside,
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_FINDER_INFO, &[8; 32])],
        );
        migrate_dir_container(&sub).unwrap();
        assert!(!outside.exists());
        assert_eq!(open(&sub).finder_info, [7; 32]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_root_flags_stay_out_of_the_parent() {
        let dir = temp_dir("dir-root");
        let root = dir.join("root");
        fs::create_dir(&root).unwrap();
        write_container(
            &dir.join("._root"),
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_FINDER_INFO, &[9; 32])],
        );
        let flags = ProbeFlags {
            apple_double: true,
            parent_side: false,
        };
        assert_eq!(open_with(&root, &flags).finder_info, [0; 32]);
        // A write never takes the sidecar with it
        set_finder_info(&root, &[1; 32]).unwrap();
        assert!(dir.join("._root").exists());
        assert!(!dir.join(".AppleDouble").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sidecar_merges_into_an_existing_container() {
        let dir = temp_dir("both");
        let file = dir.join("f");
        fs::write(&file, b"x").unwrap();
        set_finder_info(&file, &[1; 32]).unwrap();
        write_container(
            &dir.join("._f"),
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_FINDER_INFO, &[2; 32])],
        );
        // The sidecar wins on the read side, as ever
        assert_eq!(open(&file).finder_info, [2; 32]);
        // A write lands, and the sidecar is gone
        set_finder_info(&file, &[3; 32]).unwrap();
        assert!(!dir.join("._f").exists());
        assert_eq!(open(&file).finder_info, [3; 32]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_a_failed_read_does_not_replace_the_container() {
        let dir = temp_dir("readerr");
        let file = dir.join("f");
        fs::write(&file, b"x").unwrap();
        // .AppleDouble is a file: the container cannot be read, and that
        // is an error rather than a fresh start
        fs::write(dir.join(".AppleDouble"), b"not a directory").unwrap();
        assert!(set_finder_info(&file, &[1; 32]).is_err());
        assert_eq!(
            fs::read(dir.join(".AppleDouble")).unwrap(),
            b"not a directory"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resource_fork_length_grows_and_shrinks() {
        let dir = temp_dir("rsrc-len");
        let file = dir.join("f");
        fs::write(&file, b"").unwrap();
        // No fork yet: a new one of that length, all zeros
        assert_eq!(set_rsrc_len(&file, 100).unwrap(), 100);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 100);
        // The Finder's copy: extend, write, shrink to the real size
        assert_eq!(write_rsrc(&file, 0, b"RSRC").unwrap(), 100);
        assert_eq!(set_rsrc_len(&file, 200).unwrap(), 200);
        assert_eq!(set_rsrc_len(&file, 4).unwrap(), 4);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 4);
        let data = fs::read(meta.rsrc_file.unwrap()).unwrap();
        let at = meta.rsrc_offset as usize;
        assert_eq!(&data[at..at + 4], b"RSRC");
        // Growing again reads zeros, not what was cut off
        assert_eq!(write_rsrc(&file, 4, b"XY").unwrap(), 6);
        assert_eq!(set_rsrc_len(&file, 4).unwrap(), 4);
        assert_eq!(set_rsrc_len(&file, 6).unwrap(), 6);
        let meta = open(&file);
        let data = fs::read(meta.rsrc_file.unwrap()).unwrap();
        let at = meta.rsrc_offset as usize;
        assert_eq!(&data[at + 4..at + 6], &[0, 0]);
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
            &APPLE_DOUBLE_MAGIC,
            &[(9, &finder), (2, b"rsrc!"), (8, &dates)],
        );
        // The first write migrates the sidecar into the .AppleDouble
        // container and removes it
        set_attrs(&file, 0x0001).unwrap();
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
        write_container(&dir.join("._doc.txt"), &APPLE_DOUBLE_MAGIC, &[(9, &finder)]);
        // A truncate-to-0 on a fork-less file (what FPSetForkParms
        // does): the migrated Finder info must still persist, and the
        // sidecar goes only once the container is saved
        let _ = set_rsrc_len(&file, 0).unwrap();
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
        write_container(&dir.join("._mv.txt"), &APPLE_DOUBLE_MAGIC, &[(9, &finder)]);
        // Moving the file moves the sidecar (no container was ever
        // written, so the read side still prefers it)
        let renamed = dir.join("renamed.txt");
        move_meta(&file, &renamed).unwrap();
        assert!(!dir.join("._mv.txt").exists());
        assert!(dir.join("._renamed.txt").exists());
        let meta = open(&renamed);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        // Deleting removes the sidecar too
        remove_meta(&renamed);
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
        data.extend_from_slice(&APPLE_DOUBLE_MAGIC);
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

        // Chunked writes: the first rebuilds (the fork is not the last
        // body), moving the fork to the end; the rest grow in place
        for chunk in 0..8u64 {
            let chunk_data = vec![0x22 + chunk as u8; 100];
            let new_len = write_rsrc(&file, chunk * 100, &chunk_data).unwrap();
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
        let (off, len) = get(ENTRY_PRIVATE_ATTRS);
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
        set_dates(&file, afp_1995, afp_1995, 0x8000_0000).unwrap();
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
        set_finder_info(&file, &text_finder()).unwrap();
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
        set_finder_info(&file, &other).unwrap();
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
        let len = write_rsrc(&file, 0, b"resource").unwrap();
        assert_eq!(len, 8);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 8);
        assert_eq!(meta.data_len, 4);
        // A write inside the fork is in place
        let len = write_rsrc(&file, 4, b"XXXX").unwrap();
        assert_eq!(len, 8);
        // A write past the end extends the fork, zero-filling the gap
        let len = write_rsrc(&file, 10, b"YY").unwrap();
        assert_eq!(len, 12);
        let meta = open(&file);
        assert_eq!(meta.rsrc_fork_len, 12);
        assert_eq!(meta.data_len, 4);
        let rsrc = fs::read(meta.rsrc_file.as_ref().unwrap()).unwrap();
        let offset = meta.rsrc_offset as usize;
        assert_eq!(&rsrc[offset..offset + 12], b"resoXXXX\0\0YY");
        // Truncate
        let len = set_rsrc_len(&file, 6).unwrap();
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
        set_dates(&file, 1000, 2000, 0x8000_0000).unwrap();
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
        set_attrs(&file, 0x8001).unwrap();
        let meta = open(&file);
        assert_eq!(meta.attrs, 0x8001);
        // The Finder info a client writes does not clobber the attrs
        set_finder_info(&file, &text_finder()).unwrap();
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
        set_finder_info(&file, &text_finder()).unwrap();
        let container = sub.join(".AppleDouble").join("doc.txt");
        assert!(container.exists());
        // Move
        let moved = dir.join("renamed.txt");
        move_meta(&file, &moved).unwrap();
        assert!(!container.exists());
        assert!(dir.join(".AppleDouble").join("renamed.txt").exists());
        // Remove
        remove_meta(&moved);
        assert!(!dir.join(".AppleDouble").join("renamed.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_containers_keep_the_synthesized_finder_info() {
        let dir = temp_dir("write-seed");
        let file = dir.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        // A date-only write creates the container; the synthesized
        // TEXT/ttxt Finder info must survive
        set_dates(&file, 1, 2, 3).unwrap();
        let meta = open(&file);
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        assert_eq!(meta.create_date, 1);
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
