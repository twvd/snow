//! The AFP server's tests, by topic, and the helpers they share

use super::*;

mod desktop;
mod fork;
mod params;
mod path;
mod tree;

/// Build an AppleDouble container (version 2) holding the given
/// entries
pub(super) fn apple_double(entries: &[(u32, &[u8])]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&[0x00, 0x05, 0x16, 0x07]); // magic
    data.extend_from_slice(&0x00020000u32.to_be_bytes()); // version
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
    data
}

pub(super) fn enumerate(server: &mut Afp, file_bm: u16) -> (i32, Vec<u8>) {
    // [Enumerate][0][vol 1][dir 2][file bm][dir bm 0][count 20]
    // [start 1][max 512][path type 2][path ""]
    let mut data = vec![
        9,
        0,
        0,
        1,
        0,
        0,
        0,
        2,
        (file_bm >> 8) as u8,
        file_bm as u8,
        0,
        0,
        0,
        20,
        0,
        1,
        0x02,
        0x00,
        2,
        0,
    ];
    data[8..10].copy_from_slice(&file_bm.to_be_bytes());
    result_of(server.command(1, &data))
}

/// The long names FPEnumerate returns for the volume root, asked for
/// `count` at a time the way a client pages: each call starts after
/// the entries received so far, until ObjectNotFound
#[cfg(unix)]
pub(super) fn enumerate_in_pages(server: &mut Afp, count: u16) -> Vec<String> {
    let mut names = Vec::new();
    loop {
        // [Enumerate][0][vol 1][dir 2][file bm 0x0040][dir bm 0x0040]
        // [count][start][max 4000][path type 2][path ""]
        let mut data = vec![9, 0, 0, 1, 0, 0, 0, 2, 0, 0x40, 0, 0x40];
        data.extend_from_slice(&count.to_be_bytes());
        data.extend_from_slice(&(names.len() as u16 + 1).to_be_bytes());
        data.extend_from_slice(&[0x0F, 0xA0, 2, 0]);
        let (result, reply) = result_of(server.command(1, &data));
        if result != 0 {
            assert_eq!(result, ERR_OBJECT_NOT_FOUND);
            return names;
        }
        let returned = u16::from_be_bytes([reply[4], reply[5]]);
        assert_ne!(returned, 0);
        let mut record = &reply[6..];
        for _ in 0..returned {
            // [length][flag][name offset][...][name]
            let name = 2 + usize::from(u16::from_be_bytes([record[2], record[3]]));
            let len = usize::from(record[name]);
            names.push(String::from_utf8_lossy(&record[name + 1..name + 1 + len]).into_owned());
            record = &record[usize::from(record[0])..];
        }
    }
}

pub(super) fn exchange(s: &mut Afp, a: &str, b: &str) -> i32 {
    let mut d = vec![42u8, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 2];
    d.extend(mac_path(a));
    d.extend(mac_path(b));
    result_of(s.command(1, &d)).0
}

/// FPGetFileDirParms: the result and the parameters after the header
pub(super) fn gfdp(s: &mut Afp, name: &str, fbm: u16, dbm: u16) -> (i32, Vec<u8>) {
    let mut d = vec![34u8, 0, 0, 1, 0, 0, 0, 2];
    d.extend_from_slice(&fbm.to_be_bytes());
    d.extend_from_slice(&dbm.to_be_bytes());
    d.extend(mac_path(name));
    let (r, reply) = result_of(s.command(1, &d));
    (r, if r == 0 { reply[6..].to_vec() } else { reply })
}

pub(super) fn login(server: &mut Afp, session: u8, version: &str) -> i32 {
    let mut data = vec![18, version.len() as u8];
    data.extend_from_slice(version.as_bytes());
    data.push(15);
    data.extend_from_slice(b"No User Authent");
    let (result, _) = result_of(server.command(session, &data));
    result
}

pub(super) fn login_ok(s: &mut Afp, version: &str) {
    let mut d = vec![18u8, version.len() as u8];
    d.extend_from_slice(version.as_bytes());
    d.push(15);
    d.extend_from_slice(b"No User Authent");
    assert_eq!(result_of(s.command(1, &d)).0, 0);
}

/// An AFP 2.x path name: [type 2][Pascal string]
pub(super) fn long_path(name: &str) -> Vec<u8> {
    let mut out = vec![2, name.len() as u8];
    out.extend_from_slice(name.as_bytes());
    out
}

pub(super) fn mac_path(name: &str) -> Vec<u8> {
    // The wire carries MacRoman bytes
    let (b, _, _) = encoding_rs::MACINTOSH.encode(name);
    let mut v = vec![2, b.len() as u8];
    v.extend_from_slice(&b);
    v
}

/// A share with a directory above it, to see what lands outside
pub(super) fn nested(name: &str) -> (Afp, PathBuf) {
    let top = crate::test_dir(format!("snow-afp-{name}"));
    let _ = std::fs::remove_dir_all(&top);
    let share = top.join("share");
    std::fs::create_dir_all(&share).unwrap();
    let settings = AfpSettings {
        read_only: false,
        ..AfpSettings::default()
    };
    (
        Afp::new_with(share.clone(), None, MaxVersion::default(), settings),
        share.canonicalize().unwrap(),
    )
}

/// FPOpenFork: the result and the fork reference
pub(super) fn open(s: &mut Afp, name: &str, rsrc: bool, access: u16) -> (i32, u16) {
    let mut d = vec![26u8, if rsrc { 0x80 } else { 0 }, 0, 1, 0, 0, 0, 2, 0, 0];
    d.extend_from_slice(&access.to_be_bytes());
    d.extend(mac_path(name));
    let (r, reply) = result_of(s.command(1, &d));
    let f = if r == 0 {
        u16::from_be_bytes([reply[2], reply[3]])
    } else {
        0
    };
    (r, f)
}

pub(super) fn open_fork(server: &mut Afp, name: &str, rsrc: bool) -> (i32, Vec<u8>) {
    let mut data = vec![
        26,
        if rsrc { 0x80 } else { 0 },
        0,
        1,
        0,
        0,
        0,
        2,
        0,
        0,
        0,
        1,
        2,
        name.len() as u8,
    ];
    data.extend_from_slice(name.as_bytes());
    result_of(server.command(1, &data))
}

pub(super) fn open_rw(server: &mut Afp, name: &str, rsrc: bool) -> u16 {
    let mut data = vec![
        26,
        if rsrc { 0x80 } else { 0 },
        0,
        1,
        0,
        0,
        0,
        2,
        0x00,
        0x00,
        0x00,
        0x03,
    ];
    data.extend_from_slice(&mac_path(name));
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0, "open {name}");
    u16::from_be_bytes([reply[2], reply[3]])
}

pub(super) fn read(s: &mut Afp, f: u16, off: u32, n: u32) -> (i32, Vec<u8>) {
    let mut d = vec![27u8, 0];
    d.extend_from_slice(&f.to_be_bytes());
    d.extend_from_slice(&off.to_be_bytes());
    d.extend_from_slice(&n.to_be_bytes());
    d.extend_from_slice(&[0, 0]);
    result_of(s.command(1, &d))
}

pub(super) fn result_of(reply: (Vec<u8>, Vec<u8>)) -> (i32, Vec<u8>) {
    (i32::from_be_bytes(reply.0.try_into().unwrap()), reply.1)
}

pub(super) fn set_fork_len(s: &mut Afp, f: u16, bitmap: u16, len: u32) -> i32 {
    let mut d = vec![31u8, 0];
    d.extend_from_slice(&f.to_be_bytes());
    d.extend_from_slice(&bitmap.to_be_bytes());
    d.extend_from_slice(&len.to_be_bytes());
    result_of(s.command(1, &d)).0
}

/// FPSetFileDirParms with one bitmap and the values at the next even offset
pub(super) fn set_parms(s: &mut Afp, name: &str, bitmap: u16, vals: &[u8]) -> i32 {
    let mut d = vec![35u8, 0, 0, 1, 0, 0, 0, 2];
    d.extend_from_slice(&bitmap.to_be_bytes());
    d.extend(mac_path(name));
    if d.len() % 2 == 1 {
        d.push(0);
    }
    d.extend_from_slice(vals);
    result_of(s.command(1, &d)).0
}

pub(super) fn simple(cmd: u8, name: &str) -> Vec<u8> {
    let mut d = vec![cmd, 0, 0, 1, 0, 0, 0, 2];
    d.extend(mac_path(name));
    d
}

/// A server rooted at a fresh temporary directory
pub(super) fn test_server(name: &str) -> (Afp, PathBuf) {
    let dir = crate::test_dir(format!("snow-afp-{}", name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    (Afp::with_root(dir.clone()), dir)
}

/// A server with write access
pub(super) fn test_server_writable(name: &str) -> (Afp, PathBuf) {
    let dir = crate::test_dir(format!("snow-afp-{}", name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let settings = AfpSettings {
        read_only: false,
        ..AfpSettings::default()
    };
    (
        Afp::new_with(dir.clone(), None, MaxVersion::default(), settings),
        dir,
    )
}

pub(super) fn write(server: &mut Afp, f: u16, off: u32, bytes: &[u8]) {
    let mut w = vec![33, 0];
    w.extend_from_slice(&f.to_be_bytes());
    w.extend_from_slice(&off.to_be_bytes());
    w.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    w.extend_from_slice(bytes);
    assert_eq!(result_of(server.command(1, &w)).0, 0);
}
