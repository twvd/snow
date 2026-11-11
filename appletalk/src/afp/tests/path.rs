//! Tests of path names, name conversion and listing

use super::*;

#[test]
fn test_dir_entry_folds_nfc_and_case() {
    let dir = crate::test_dir("snow-dir-entry");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("re\u{301}sume\u{301}.txt"), b"x").unwrap();
    let (server, _) = test_server("dir-entry");
    assert_eq!(
        server.dir_entry(&dir, "RÉSUMÉ.txt"),
        Some(dir.join("re\u{301}sume\u{301}.txt"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_enumerate_spec_layout() {
    let (mut server, dir) = test_server("enumerate");
    std::fs::write(dir.join("hello.txt"), b"hello").unwrap();
    std::fs::create_dir(dir.join("subdir")).unwrap();
    // [Enumerate][0][vol 1][dir 2][file bm 0x0340][dir bm 0x0340]
    // [count 20][start 1][max 512][path type 2][path ""]
    let data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[0..2], [0x03, 0x40]); // file bitmap
    assert_eq!(reply[2..4], [0x03, 0x40]); // directory bitmap
    assert_eq!(reply[4..6], [0, 2]); // two entries
    // A MaxReplySize too small for the first entry is ParamErr
    let mut small = data;
    small[16..18].copy_from_slice(&10u16.to_be_bytes());
    assert_eq!(result_of(server.command(1, &small)).0, ERR_PARAM);

    // First record: hello.txt, a file. The long-name offset comes
    // first (bit 6), then the file number and the data fork length.
    let rec0_len = reply[6] as usize;
    let rec0 = &reply[6..6 + rec0_len];
    assert_eq!(rec0[1], 0); // a file
    let off0 = u16::from_be_bytes([rec0[2], rec0[3]]) as usize;
    assert_eq!(off0, 10);
    assert_eq!(&rec0[2 + off0..2 + off0 + 10], b"\x09hello.txt");
    let file_num = u32::from_be_bytes(rec0[4..8].try_into().unwrap());
    // The file number is the file's ID from the identity table
    // (minted, stable), not a name hash
    assert_ne!(file_num, 0);
    assert_eq!(file_num, server.ids.id_of(&dir.join("hello.txt")).unwrap());
    let data_len = u32::from_be_bytes(rec0[8..12].try_into().unwrap());
    assert_eq!(data_len, 5);

    // Second record: subdir, a directory. Its own Directory ID
    // (not the root's), offspring count and the long name.
    let rec1 = &reply[6 + rec0_len..];
    assert_eq!(rec1.len(), 18);
    assert_eq!(rec1[1], DIR_FLAG);
    let sub_id = server.dir_id(&dir.join("subdir"));
    assert_eq!(rec1[4..8], sub_id.to_be_bytes()); // directory ID
    assert_ne!(sub_id, ROOT_DIR_ID);
    let off1 = u16::from_be_bytes([rec1[2], rec1[3]]) as usize;
    assert_eq!(off1, 8);
    assert_eq!(&rec1[2 + off1..2 + off1 + 7], b"\x06subdir");
}

#[test]
fn test_enumerate_subdirectory_by_id() {
    // A directory opened by its Directory ID enumerates its own
    // offspring, not the root's
    let (mut server, dir) = test_server("enumerate-sub");
    std::fs::write(dir.join("top.txt"), b"t").unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub").join("inner.txt"), b"i").unwrap();
    let sub_id = server.dir_id(&dir.join("sub"));

    // [Enumerate][0][vol 1][dir = sub_id][file bm 0x0340]
    // [dir bm 0x0340][count 20][start 1][max 512][ptype 2][path ""]
    let mut data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    data[4..8].copy_from_slice(&sub_id.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[4..6], [0, 1]); // one entry
    let rec_len = reply[6] as usize;
    assert_eq!(reply[7], 0); // a file
    let off = u16::from_be_bytes([reply[8], reply[9]]) as usize;
    assert_eq!(&reply[6 + 2 + off..6 + 2 + off + 10], b"\x09inner.txt");
    let _ = rec_len;

    // A GetFileDirParms on the same ID resolves the directory too
    // (null bitmaps: just the bitmaps and the flag come back)
    let mut g = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 0];
    g[4..8].copy_from_slice(&sub_id.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &g));
    assert_eq!(result, 0);
    assert_eq!(reply[4], 0x80); // a directory
}

#[cfg(unix)]
#[test]
fn test_enumerate_pages_past_what_it_cannot_list() {
    let (mut server, dir) = test_server("enumerate-unlistable");
    std::fs::write(dir.join("a"), b"a").unwrap();
    // A dangling symbolic link and a FIFO sort between the files
    std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("b")).unwrap();
    let fifo = std::ffi::CString::new(dir.join("c").to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    std::fs::write(dir.join("d"), b"d").unwrap();
    std::fs::write(dir.join("e"), b"e").unwrap();
    // Each file comes once, whatever the page size
    assert_eq!(enumerate_in_pages(&mut server, 1), ["a", "d", "e"]);
    assert_eq!(enumerate_in_pages(&mut server, 20), ["a", "d", "e"]);
    // The offspring count agrees:
    // [GetFileDirParms][0][vol 1][dir 2][file bm 0][dir bm 0x0200][type 2][""]
    let data = [34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0x02, 0x00, 2, 0];
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[6..8], [0, 3]);
}

#[test]
fn every_page_size_lists_each_entry_once() {
    let (mut server, dir) = test_server("enumerate-pages");
    for name in ["f1", "f2", "f3", "f4"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
    std::fs::create_dir(dir.join("d1")).unwrap();
    std::fs::create_dir(dir.join("d2")).unwrap();
    std::os::unix::fs::symlink(dir.join("f1"), dir.join("l1")).unwrap();
    std::os::unix::fs::symlink(dir.join("d1"), dir.join("l2")).unwrap();
    std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("broken")).unwrap();
    std::fs::write(dir.join(".hidden"), b"x").unwrap();
    std::fs::write(dir.join("._f1"), b"x").unwrap();
    let all = enumerate_in_pages(&mut server, 100);
    assert_eq!(all, ["d1", "d2", "f1", "f2", "f3", "f4", "l1", "l2"]);
    // The offspring count GetFileDirParms reports agrees
    let data = [34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0x02, 0x00, 2, 0];
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(
        usize::from(u16::from_be_bytes([reply[6], reply[7]])),
        all.len()
    );
    // Pages of every size add up to the same list
    for count in 1..=all.len() as u16 {
        assert_eq!(
            enumerate_in_pages(&mut server, count),
            all,
            "pages of {count}"
        );
    }
    // A change between two pages is seen by the next page
    let first = enumerate_in_pages(&mut server, 1);
    std::fs::write(dir.join("a0"), b"x").unwrap();
    assert_eq!(first.len(), all.len());
    assert_eq!(enumerate_in_pages(&mut server, 3)[0], "a0");
}

#[test]
fn test_nul_separated_path_grammar() {
    let (mut server, dir) = test_server("nul-paths");
    std::fs::create_dir(dir.join("a")).unwrap();
    std::fs::create_dir(dir.join("a").join("b")).unwrap();
    std::fs::write(dir.join("a").join("b").join("f.txt"), b"x").unwrap();
    std::fs::write(dir.join("a").join("f.txt"), b"y").unwrap();
    // A single leading NUL is ignored: the path stays relative to
    // the Directory ID (ch13's rule, not "from the root")
    let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 10];
    data.extend_from_slice(b"\0a\0b\0f.txt");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    // Two consecutive NULs ascend one level: from a/b, "\0\0f.txt"
    // is a/f.txt
    let b_id = server.dir_id(&dir.join("a").join("b"));
    let mut data = vec![34, 0, 0, 1];
    data.extend_from_slice(&b_id.to_be_bytes());
    data.extend_from_slice(&[0, 0, 0, 0, 2, 7]);
    data.extend_from_slice(b"\0\0f.txt");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    // Ascending above the root answers ObjectNotFound
    let a_id = server.dir_id(&dir.join("a"));
    let mut data = vec![34, 0, 0, 1];
    data.extend_from_slice(&a_id.to_be_bytes());
    data.extend_from_slice(&[0, 0, 0, 0, 2, 8]);
    data.extend_from_slice(b"\0\0\0x.txt");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, ERR_OBJECT_NOT_FOUND);
    // A run between names or at the end ascends too (ch13's sixth
    // and seventh examples), and "\0\0" alone is the parent: the
    // Finder's New Folder asks for it by the new folder's ID
    let dir_id_of = |server: &mut Afp, dir_id: u32, path: &[u8]| {
        let mut data = vec![34, 0, 0, 1];
        data.extend_from_slice(&dir_id.to_be_bytes());
        data.extend_from_slice(&[0, 0, 1, 0, 2, path.len() as u8]);
        data.extend_from_slice(path);
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0, "path {path:?}");
        u32::from_be_bytes(reply[6..10].try_into().unwrap())
    };
    let a_id = server.dir_id(&dir.join("a"));
    assert_eq!(dir_id_of(&mut server, 2, b"a\0b\0\0b"), b_id);
    assert_eq!(dir_id_of(&mut server, 2, b"a\0b\0\0"), a_id);
    assert_eq!(dir_id_of(&mut server, 2, b"a\0b\0\0\0"), ROOT_DIR_ID);
    assert_eq!(dir_id_of(&mut server, b_id, b"\0\0"), a_id);
    assert_eq!(dir_id_of(&mut server, b_id, b"\0"), b_id);
    assert_eq!(dir_id_of(&mut server, 2, b"a\0b\0"), b_id);
    // Directory ID 1 plus the volume name as the first element
    // addresses the root
    let mut data = vec![34, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 2, 12];
    data.extend_from_slice(b"Snow\0a\0f.txt");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    // A colon is not part of the path syntax: one name, not a
    // separator
    let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 9];
    data.extend_from_slice(b"a:b:f.txt");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, ERR_OBJECT_NOT_FOUND);
    // Parent traversal as names stays rejected
    let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 9];
    data.extend_from_slice(b"a\0..\0x.txt");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, ERR_OBJECT_NOT_FOUND);
}

#[test]
fn test_metadata_store_hidden_in_every_mode() {
    // Even with dotfiles shown, the server's own metadata is not
    // addressable or enumerable
    let dir = crate::test_dir("snow-afp-meta-hidden");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".secret"), b"s").unwrap();
    std::fs::create_dir(dir.join(".AppleDouble")).unwrap();
    std::fs::write(dir.join(".AppleDouble").join("x"), b"m").unwrap();
    let mut server = Afp::new_with(
        dir.clone(),
        None,
        MaxVersion::default(),
        AfpSettings {
            hidden_files: HiddenFiles::Show,
            ..AfpSettings::default()
        },
    );
    // The dotfile is addressable in Show mode, the metadata is not
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 7];
    data.extend_from_slice(b".secret");
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 12];
    data.extend_from_slice(b".AppleDouble");
    assert_eq!(result_of(server.command(1, &data)).0, ERR_OBJECT_NOT_FOUND);
    // Only the dotfile enumerates
    let data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    assert_eq!(reply[4..6], [0, 1]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_root_parent_is_one() {
    let (mut server, dir) = test_server("parent-chain");
    std::fs::create_dir(dir.join("child")).unwrap();
    // [GetFileDirParms][0][vol 1][dir 2][file bm 0][dir bm 0x0002
    // (parent ID)][path type 2][path ""] — the root's parent is the
    // reserved ID 1, where the client's parent walk terminates
    let data = [34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0x00, 0x02, 2, 0];
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[4], 0x80); // a directory
    assert_eq!(reply[6..10], [0, 0, 0, 1]); // parent = 1
    // A first-level directory's parent is the root (ID 2)
    let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0x00, 0x02, 2, 5];
    data.extend_from_slice(b"child");
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[6..10], [0, 0, 0, 2]);
}

#[test]
fn test_enumerate_past_end_returns_object_not_found() {
    let (mut server, dir) = test_server("enumerate-end");
    std::fs::write(dir.join("one.txt"), b"1").unwrap();
    // [Enumerate][0][vol 1][dir 2][file bm 0x0340][dir bm 0x0340]
    // [count 20][start 2 (past the single entry)][max 512]
    // [path type 2][path ""]
    let data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 2, 0x02, 0x00, 2, 0,
    ];
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, ERR_OBJECT_NOT_FOUND);
}

#[test]
fn test_enumerate_zero_count_and_file_target() {
    let (mut server, dir) = test_server("enumerate-zero");
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
    std::fs::write(dir.join("b.txt"), b"b").unwrap();
    // A request for zero entries answers an empty enumeration
    let data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 0, 0, 1, 0x02, 0x00, 2, 0,
    ];
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[4..6], [0, 0]);
    // Enumerating a file answers afpObjectTypeErr
    let mut data = vec![
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 5,
    ];
    data.extend_from_slice(b"a.txt");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, ERR_OBJECT_TYPE);
}

#[test]
fn test_enumerate_empty_dir_returns_object_not_found() {
    let (mut server, dir) = test_server("enumerate-empty");
    std::fs::create_dir(dir.join("sub")).unwrap();
    // Enumerate the empty "sub" directory from the start
    let mut data = vec![
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 3,
    ];
    data.extend_from_slice(b"sub");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, ERR_OBJECT_NOT_FOUND);
}

#[test]
fn test_enumerate_hides_metadata() {
    let (mut server, dir) = test_server("hide");
    std::fs::write(dir.join("visible.txt"), b"v").unwrap();
    std::fs::write(dir.join(".secret"), b"s").unwrap();
    std::fs::write(dir.join("._visible.txt"), b"sidecar").unwrap();
    std::fs::create_dir_all(dir.join(".AppleDouble")).unwrap();
    let (result, reply) = enumerate(&mut server, 0x0040);
    assert_eq!(result, 0);
    assert_eq!(reply[4..6], [0, 1]); // only visible.txt
    // The record's name is visible.txt
    let rec0_len = reply[6] as usize;
    let rec0 = &reply[6..6 + rec0_len];
    let off = u16::from_be_bytes([rec0[2], rec0[3]]) as usize;
    assert_eq!(&rec0[2 + off..], b"\x0bvisible.txt");
}

#[test]
fn test_open_mangled_name_via_table() {
    let (mut server, dir) = test_server("mangle");
    std::fs::write(dir.join("你好.txt"), b"mangled!").unwrap();
    let (result, _) = enumerate(&mut server, 0x0040);
    assert_eq!(result, 0);
    let mangled = crate::name::to_mac("你好.txt").0;
    assert!(mangled.starts_with('#') && mangled.ends_with(".txt"));

    let (result, reply) = open_fork(&mut server, &mangled, false);
    assert_eq!(result, 0, "reply: {reply:?}");
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &read));
    assert_eq!(result, 0);
    assert_eq!(reply, b"mangled!");
}

#[test]
fn test_open_colon_bijection() {
    let (mut server, dir) = test_server("colon");
    std::fs::write(dir.join("a:b.txt"), b"slash").unwrap();
    // The wire name uses '/'
    let (result, reply) = open_fork(&mut server, "a/b.txt", false);
    assert_eq!(result, 0);
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &read));
    assert_eq!(result, 0);
    assert_eq!(reply, b"slash");
    // The OpenFork reply reports the wire name
    assert_eq!(reply, b"slash");
}

#[test]
fn test_hidden_and_traversal_are_not_addressable() {
    let (mut server, dir) = test_server("harden");
    std::fs::write(dir.join(".secret"), b"s").unwrap();
    std::fs::write(dir.join("ok.txt"), b"ok").unwrap();
    for name in [".secret", "..", "a:b", ".AppleDouble"] {
        let (result, _) = open_fork(&mut server, name, false);
        assert_eq!(result, ERR_OBJECT_NOT_FOUND, "opening {name:?}");
    }
    // A single leading NUL is ignored (the path stays relative);
    // a trailing one is lenient; a colon is not path syntax
    let (result, _) = open_fork(&mut server, "\0ok.txt", false);
    assert_eq!(result, 0);
    let (result, _) = open_fork(&mut server, "ok.txt\0", false);
    assert_eq!(result, 0);
    let (result, _) = open_fork(&mut server, ":ok.txt", false);
    assert_eq!(result, ERR_OBJECT_NOT_FOUND);
    // The plain file still opens
    let (result, _) = open_fork(&mut server, "ok.txt", false);
    assert_eq!(result, 0);
}

#[test]
fn test_v11_enumerate_has_file_flag() {
    let (mut server, dir) = test_server("versions-v11-enum");
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
    // [Enumerate][FileFlag 0][vol 1][dir 2][file bm 0x0040 (long
    // name)][dir bm 0: files only][count 20][start 1][max 512]
    // [ptype 2][path ""]
    let data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x40, 0, 0, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[4..6], [0, 1]); // the null Directory Bitmap
    // keeps the directory out
    // The first record's flag byte is 0 (a file), and the name is
    // a.txt
    assert_eq!(reply[7], 0);
    let off = u16::from_be_bytes([reply[8], reply[9]]) as usize;
    assert_eq!(&reply[6 + 2 + off..6 + 2 + off + 6], b"\x05a.txt");

    // With the FileFlag bit 7 set, only directories are returned
    let data = [
        9, 0x80, 0, 1, 0, 0, 0, 2, 0x00, 0x40, 0, 0, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[4..6], [0, 1]); // the directory only
    assert_eq!(reply[7], DIR_FLAG);
}

#[test]
fn nfd_host_name_resolves() {
    let (mut server, dir) = test_server_writable("nfd");
    std::fs::write(dir.join("re\u{301}sume\u{301}.txt"), b"x").unwrap();
    // The wire name the server serves for it is NFC
    let (wire, _) = crate::name::to_mac("re\u{301}sume\u{301}.txt");
    let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 0];
    data.extend_from_slice(&mac_path(&wire));
    let r = result_of(server.command(1, &data)).0;
    assert_eq!(r, 0, "the NFC wire name of an NFD host file must resolve");
}

#[test]
fn show_as_invisible_marks_dot_entries() {
    let dir = crate::test_dir("snow-afp-invisible");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".dotdir")).unwrap();
    std::fs::write(dir.join(".secret"), b"x").unwrap();
    std::fs::write(dir.join("plain"), b"x").unwrap();
    let settings = AfpSettings {
        hidden_files: HiddenFiles::Invisible,
        ..AfpSettings::default()
    };
    let mut s = Afp::new_with(dir, None, MaxVersion::default(), settings);
    // Attributes (bit 0) and Finder Info (bit 5)
    for (name, fbm, dbm) in [(".secret", 0x21, 0), (".dotdir", 0, 0x21)] {
        let (r, p) = gfdp(&mut s, name, fbm, dbm);
        assert_eq!(r, 0, "{name}");
        assert_eq!(p[1] & 1, 1, "{name}: attributes");
        assert_eq!(p[10] & 0x40, 0x40, "{name}: fdFlags");
    }
    let (_, p) = gfdp(&mut s, "plain", 0x21, 0);
    assert_eq!((p[1] & 1, p[10] & 0x40), (0, 0));
    // Enumerate says the same: the records come sorted by wire name
    let e = [
        9u8, 0, 0, 1, 0, 0, 0, 2, 0, 0x21, 0, 0, 0, 20, 0, 1, 0x10, 0, 2, 0,
    ];
    let (r, reply) = result_of(s.command(1, &e));
    assert_eq!(r, 0);
    let rec = &reply[6 + 2..];
    assert_eq!(rec[1] & 1, 1);
    assert_eq!(rec[10] & 0x40, 0x40);
}

#[cfg(unix)]
#[test]
fn a_non_utf8_host_file_reads_through_its_mangled_name() {
    use std::os::unix::ffi::OsStrExt;
    let (mut s, dir) = test_server_writable("non-utf8");
    let name = std::ffi::OsStr::from_bytes(b"caf\x8E.txt");
    std::fs::write(dir.join(name), vec![1u8; 1000]).unwrap();
    let e = [
        9u8, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x40, 0, 0, 0, 20, 0, 1, 0x10, 0, 2, 0,
    ];
    let (r, reply) = result_of(s.command(1, &e));
    assert_eq!(r, 0);
    // The record holds the name offset, then the data fork length
    let mut at = 6;
    let mut wire = None;
    while at < reply.len() {
        let len = reply[at] as usize;
        let rec = &reply[at + 2..at + len];
        let off = u16::from_be_bytes([rec[0], rec[1]]) as usize;
        let (name, _, _) =
            encoding_rs::MACINTOSH.decode(&rec[off + 1..off + 1 + rec[off] as usize]);
        if name.starts_with("caf") {
            assert_eq!(u32::from_be_bytes(rec[2..6].try_into().unwrap()), 1000);
            wire = Some(name.into_owned());
        }
        at += len;
    }
    let (r, f) = open(&mut s, &wire.unwrap(), false, 1);
    assert_eq!(r, 0);
    assert_eq!(read(&mut s, f, 0, 1000).1.len(), 1000);
}

#[test]
fn a_mangled_name_resolves_without_having_been_served() {
    // An alias kept across a server restart
    let long = "a".repeat(40) + ".txt";
    let (mut s, dir) = test_server_writable("mangle-cold");
    std::fs::write(dir.join(&long), b"x").unwrap();
    let (mac, _) = crate::name::to_mac(&long);
    assert_eq!(gfdp(&mut s, &mac, 0x0040, 0).0, 0);
    // In any case, as the Mac file system folds it
    assert_eq!(gfdp(&mut s, &mac.to_uppercase(), 0x0040, 0).0, 0);
}

#[test]
fn a_real_name_wins_over_a_mangled_one() {
    let long = "a".repeat(40) + ".txt";
    let (mut s, dir) = test_server_writable("mangle-real");
    let (mac, _) = crate::name::to_mac(&long);
    std::fs::write(dir.join(&long), b"long").unwrap();
    std::fs::write(dir.join(&mac), b"real").unwrap();
    let (_, f) = open(&mut s, &mac, false, 1);
    assert_eq!(read(&mut s, f, 0, 4).1, b"real");
}

#[cfg(unix)]
#[test]
fn a_symlink_reports_its_own_name_and_its_targets_metadata() {
    let (mut s, dir) = test_server_writable("link-name");
    std::fs::write(dir.join("real.txt"), b"x").unwrap();
    std::os::unix::fs::symlink("real.txt", dir.join("link.txt")).unwrap();
    let mut info = [0u8; 32];
    info[..8].copy_from_slice(b"ABCDEFGH");
    assert_eq!(set_parms(&mut s, "real.txt", 0x0020, &info), 0);
    // Finder Info (bit 5) and the long name (bit 6)
    let (r, p) = gfdp(&mut s, "link.txt", 0x0060, 0);
    assert_eq!(r, 0);
    assert_eq!(&p[..8], b"ABCDEFGH");
    let off = u16::from_be_bytes([p[32], p[33]]) as usize;
    assert_eq!(&p[off + 1..off + 1 + p[off] as usize], b"link.txt");
    // Enumerate agrees
    let mut e = vec![
        9u8, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x60, 0, 0, 0, 20, 0, 1, 0x10, 0, 2, 0,
    ];
    let (r, reply) = result_of(s.command(1, &e));
    assert_eq!(r, 0);
    e.clear();
    let mut at = 6;
    let mut seen = 0;
    while at < reply.len() {
        let len = reply[at] as usize;
        let rec = &reply[at + 2..at + len];
        let off = u16::from_be_bytes([rec[32], rec[33]]) as usize;
        let name = &rec[off + 1..off + 1 + rec[off] as usize];
        if name == b"link.txt" || name == b"real.txt" {
            assert_eq!(&rec[..8], b"ABCDEFGH", "{name:?}");
            seen += 1;
        }
        at += len;
    }
    assert_eq!(seen, 2);
}

#[test]
fn root_metadata_stays_inside_the_share() {
    let (mut s, dir) = nested("root");
    let outside = dir.parent().unwrap();
    // What a host tool left beside the share, under the share's name
    std::fs::write(outside.join("._share"), b"keep me").unwrap();
    // The volume window's DInfo on the root
    let mut info = [0u8; 32];
    info[0] = 0x11;
    assert_eq!(set_parms(&mut s, "", 0x0020, &info), 0);
    assert_eq!(set_parms(&mut s, "", 0x000C, &[0, 0, 0, 1, 0, 0, 0, 2]), 0);
    assert!(!outside.join(".AppleDouble").exists());
    assert_eq!(std::fs::read(outside.join("._share")).unwrap(), b"keep me");
    assert!(dir.join(".AppleDouble/.Parent").is_file());
    assert_eq!(gfdp(&mut s, "", 0, 0x0020).1[..1], [0x11]);
}

#[cfg(unix)]
#[test]
fn special_files_never_hang_a_call() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (server, dir) = test_server_writable("special-files");
    let outside = crate::test_dir("snow-afp-special-outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"x").unwrap();
    let fifo = std::ffi::CString::new(dir.join("fifo").to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    let _socket = std::os::unix::net::UnixListener::bind(dir.join("sock")).unwrap();
    symlink("/dev/zero", dir.join("zero")).unwrap();
    symlink(dir.join("loop2"), dir.join("loop1")).unwrap();
    symlink(dir.join("loop1"), dir.join("loop2")).unwrap();
    symlink(&outside, dir.join("out")).unwrap();
    std::fs::create_dir(dir.join("closed")).unwrap();
    std::fs::set_permissions(dir.join("closed"), std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::write(dir.join("plain"), b"x").unwrap();

    // Every call on every entry, and the desktop scan, in a thread that
    // must finish: an open of the FIFO or of /dev/zero would never return
    let names = ["fifo", "sock", "zero", "loop1", "out", "closed", "plain"];
    let root = dir.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = server;
        let listed = enumerate_in_pages(&mut s, 3);
        for name in names {
            let _ = gfdp(&mut s, name, 0x0340, 0x0340);
            let _ = open(&mut s, name, false, 1);
            let _ = open(&mut s, name, true, 1);
            let _ = gfdp(&mut s, &format!("{name}\0secret"), 0x0340, 0x0340);
        }
        let (batches, _) = std::sync::mpsc::channel();
        let abort = std::sync::atomic::AtomicBool::new(false);
        crate::dtdb::scan_volume(root, &batches, &abort, None, AfpSettings::default());
        let escaped = gfdp(&mut s, "out\0secret", 0x0340, 0).0;
        let _ = tx.send((listed, escaped));
    });
    let listed = rx.recv_timeout(std::time::Duration::from_secs(10));
    std::fs::set_permissions(dir.join("closed"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let (listed, escaped) = listed.expect("a call on a special file did not return");
    assert_ne!(escaped, 0, "a link out of the share reached outside");
    // Nothing a Mac cannot open is listed, and nothing reaches outside
    assert!(listed.contains(&"plain".to_string()));
    for gone in ["fifo", "sock", "zero", "loop1", "loop2"] {
        assert!(!listed.contains(&gone.to_string()), "{gone} listed");
    }
    let _ = std::fs::remove_dir_all(&outside);
}
