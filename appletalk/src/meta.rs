//! Metadata storage for volume files
//!
//! Probes for AppleDouble metadata (both the netatalk `.AppleDouble/`
//! directory convention and the macOS `._` sidecar convention) and
//! AppleSingle containers, reading the resource fork, the Finder
//! information and the file dates. Files without metadata fall back to
//! their host file attributes.

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

/// The WriteInhibit bit of the AFP file attributes
const ATTR_WRITE_INHIBIT: u16 = 0x0020;
/// The DAlreadyOpen and RAlreadyOpen bits of the AFP file attributes
const ATTR_ALREADY_OPEN: u16 = 0x0018;

/// Seconds between the Unix (1970) and AFP (2000-01-01) epochs
const AFP_EPOCH_OFFSET: i64 = 946_684_800;

/// A host time as an AFP date: signed seconds since 2000-01-01 (ch13),
/// negative before 2000
pub(crate) fn afp_date(time: std::time::SystemTime) -> u32 {
    let unix = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    (unix - AFP_EPOCH_OFFSET).clamp(i32::MIN.into(), i32::MAX.into()) as i32 as u32
}

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

/// Open with every metadata container probed
pub fn open(path: &Path) -> Meta {
    open_with(path, true)
}

/// Open, reading the AppleDouble containers only when `apple_double` is set
///
/// AppleSingle is always recognised: its magic is cheap to peek, and
/// treating a container as a plain file would serve its header as file
/// data.
pub fn open_with(path: &Path, apple_double: bool) -> Meta {
    // AppleSingle first: a stale container next to the file must not
    // misdescribe where its data fork is
    if let Some(meta) = read_apple_single(path) {
        return meta;
    }
    if apple_double {
        for container in containers(path) {
            if let Some(meta) = read_apple_double(&container, path) {
                return meta;
            }
        }
    }
    base_meta(path)
}

/// The places a file's or a directory's AppleDouble container can be, in
/// the order they are read
///
/// A file's is the macOS `._<name>` sidecar, then
/// netatalk's `.AppleDouble/<name>` next to it. A directory's is netatalk's
/// `.AppleDouble/.Parent` inside it, and only that: the volume root's
/// sidecar would be outside the share.
fn containers(path: &Path) -> Vec<PathBuf> {
    if fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        return vec![parent_container(path)];
    }
    let (Some(sidecar), Some(name)) = (sidecar_path(path), path.file_name()) else {
        return Vec::new();
    };
    vec![sidecar, path.with_file_name(".AppleDouble").join(name)]
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
    {
        meta.mod_date = afp_date(f_modified);
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
            _ => {}
        }
    }
    Some(())
}

/// Read an AppleSingle container: the data fork lives inside the file
/// itself. Only the magic is read from anything else, so a large plain
/// file costs nothing.
fn read_apple_single(path: &Path) -> Option<Meta> {
    // Only a regular file can be one, and opening a FIFO would wait for
    // its writer
    if !fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
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
        if let Ok(modified) = md.modified() {
            meta.mod_date = afp_date(modified);
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

use std::io::{Read, Seek, SeekFrom};

/// The `._<name>` sidecar path next to a file (the macOS metadata
/// convention the read side probes first)
pub fn sidecar_path(path: &Path) -> Option<PathBuf> {
    // By bytes: a host name need not be UTF-8
    let mut name = std::ffi::OsString::from("._");
    name.push(path.file_name()?);
    Some(path.with_file_name(name))
}

/// The container of a directory: inside the directory itself, so the
/// volume root's stays inside the share (netatalk's convention)
fn parent_container(dir: &Path) -> PathBuf {
    dir.join(".AppleDouble").join(".Parent")
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
        let dir = crate::test_dir("snow-meta-test");
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
        let dir = crate::test_dir("snow-meta-single");
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

    #[cfg(unix)]
    #[test]
    fn test_open_does_not_wait_on_a_fifo() {
        let dir = crate::test_dir("snow-meta-fifo");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a valid NUL-terminated path
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);

        // Nothing ever writes to the FIFO: an open of it would never return
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(open(&fifo)));
        let meta = rx.recv_timeout(std::time::Duration::from_secs(5));
        assert!(meta.is_ok_and(|m| !m.single));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_directory_reads_its_parent_container() {
        let dir = crate::test_dir("snow-meta-parent");
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

    #[test]
    fn test_an_already_open_bit_in_a_container_is_not_believed() {
        let dir = temp_dir("attrs-entries");
        let file = dir.join("f");
        fs::write(&file, b"x").unwrap();
        fs::create_dir(dir.join(".AppleDouble")).unwrap();
        let container = dir.join(".AppleDouble/f");
        write_container(
            &container,
            &APPLE_DOUBLE_MAGIC,
            &[(ENTRY_AFP_INFO, &0x0000_0029u32.to_be_bytes())],
        );
        assert_eq!(open(&file).attrs, 0x0021);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_plain_file() {
        let dir = crate::test_dir("snow-meta-plain");
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
    fn test_afp_dates_at_the_edges() {
        use std::time::{Duration, UNIX_EPOCH};
        let at = |unix: i64| {
            let d = Duration::from_secs(unix.unsigned_abs());
            if unix >= 0 {
                UNIX_EPOCH + d
            } else {
                UNIX_EPOCH - d
            }
        };
        // 2000 is 0, 1970 and before are negative
        for unix in [AFP_EPOCH_OFFSET, 0, -86_400, 2_000_000_000] {
            let afp = afp_date(at(unix));
            assert_eq!(i64::from(afp as i32), unix - AFP_EPOCH_OFFSET);
        }
        // Past the signed 32-bit range (2068) the date stays at its end
        assert_eq!(afp_date(at(4_000_000_000)), i32::MAX as u32);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = crate::test_dir(format!("snow-meta-{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
