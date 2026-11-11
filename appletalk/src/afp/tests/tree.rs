//! Tests of create, delete, rename, move, the file IDs and FPExchangeFiles

use super::*;

#[test]
fn test_write_commands_are_locked_when_read_only() {
    let (mut server, dir) = test_server("write-locked");
    // Create a file on the host so the lookups can succeed
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
    // FPCreateFile -> afpVolLocked
    let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("new.txt"));
    let (r, _) = result_of(server.command(1, &data));
    assert_eq!(r, -5031);
    // FPDelete -> afpVolLocked
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("a.txt"));
    let (r, _) = result_of(server.command(1, &data));
    assert_eq!(r, -5031);
    // Opening a fork for writing -> afpVolLocked
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 5];
    data.extend_from_slice(b"a.txt");
    let (r, _) = result_of(server.command(1, &data));
    assert_eq!(r, -5031);
    // Desktop database writes -> afpVolLocked
    let (r, _) = result_of(server.command(1, &[53, 0]));
    assert_eq!(r, -5031);
    // The file is untouched
    assert!(dir.join("a.txt").exists());
    assert!(!dir.join("new.txt").exists());
}

#[test]
fn test_create_enumerate_delete() {
    let (mut server, dir) = test_server_writable("write-create");
    // Create a file and a directory
    let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("hello.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    let mut data = vec![6, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("folder"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(dir.join("hello.txt").is_file());
    assert!(dir.join("folder").is_dir());
    // Both enumerate
    let data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    // The server adds nothing of its own to the share
    assert_eq!(reply[4..6], [0, 2]);
    // Delete both
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("hello.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("folder"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(!dir.join("hello.txt").exists());
    assert!(!dir.join("folder").exists());
    // Deleting a missing file -> afpObjectNotFound
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("gone.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, -5018);
}

#[cfg(unix)]
#[test]
fn test_a_failed_folder_delete_keeps_the_folder_metadata() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid() has no preconditions
    if unsafe { libc::geteuid() } == 0 {
        return; // root ignores the permissions this test relies on
    }
    let (mut server, dir) = test_server_writable("write-delkeep");
    std::fs::create_dir_all(dir.join("p").join("d")).unwrap();
    crate::meta::set_finder_info(&dir.join("p").join("d"), &[5; 32]).unwrap();
    // The folder cannot leave a parent without write permission
    std::fs::set_permissions(dir.join("p"), std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("p\0d"));
    let r = result_of(server.command(1, &data)).0;
    std::fs::set_permissions(dir.join("p"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(r, ERR_ACCESS_DENIED);
    assert_eq!(
        crate::meta::open(&dir.join("p").join("d")).finder_info,
        [5; 32]
    );
}

#[test]
fn test_delete_inhibit_busy_and_metadata_cleanup() {
    let (mut server, dir) = test_server_writable("write-delrules");
    // DeleteInhibit (attribute bit 8) answers afpObjectLocked
    std::fs::write(dir.join("locked.txt"), b"x").unwrap();
    crate::meta::set_attrs(&dir.join("locked.txt"), 0x0100).unwrap();
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("locked.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, -5032);
    assert!(dir.join("locked.txt").exists());
    // An open fork answers afpFileBusy and stays on disk
    std::fs::write(dir.join("open.txt"), b"x").unwrap();
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 8];
    data.extend_from_slice(b"open.txt");
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("open.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, -5010);
    assert!(dir.join("open.txt").exists());
    // A folder that only holds the server's own metadata deletes
    // (its `.AppleDouble` subtree and `._` sidecar are removed
    // first)
    std::fs::create_dir(dir.join("metafolder")).unwrap();
    std::fs::create_dir(dir.join("metafolder").join(".AppleDouble")).unwrap();
    std::fs::write(
        dir.join("metafolder").join(".AppleDouble").join("orphan"),
        b"stale",
    )
    .unwrap();
    std::fs::write(dir.join("._metafolder"), b"sidecar").unwrap();
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("metafolder"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(!dir.join("metafolder").exists());
    assert!(!dir.join("._metafolder").exists());
    // A folder holding other hidden files still answers
    // afpDirNotEmpty, and its own metadata survives the failed
    // delete
    std::fs::create_dir(dir.join("dsstore")).unwrap();
    std::fs::write(dir.join("dsstore").join(".DS_Store"), b"x").unwrap();
    std::fs::write(dir.join("._dsstore"), b"sidecar").unwrap();
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("dsstore"));
    assert_eq!(result_of(server.command(1, &data)).0, -5007);
    assert!(dir.join("dsstore").exists());
    assert!(dir.join("._dsstore").exists());
    // A folder holding only `._*` orphans (a macOS copy) deletes:
    // the emptiness check ignores the server's metadata
    std::fs::create_dir(dir.join("orphans")).unwrap();
    std::fs::write(dir.join("orphans").join("._x"), b"o").unwrap();
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("orphans"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(!dir.join("orphans").exists());
    // RenameInhibit (attribute bit 7) answers afpObjectLocked
    std::fs::write(dir.join("renlocked.txt"), b"x").unwrap();
    crate::meta::set_attrs(&dir.join("renlocked.txt"), 0x0080).unwrap();
    let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("renlocked.txt"));
    data.extend_from_slice(&long_path("other.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, -5032);
    assert!(dir.join("renlocked.txt").exists());
}

#[test]
fn test_create_duplicate_and_dir_not_empty() {
    let (mut server, dir) = test_server_writable("write-errors");
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
    std::fs::create_dir(dir.join("full")).unwrap();
    std::fs::write(dir.join("full").join("child"), b"c").unwrap();
    // A duplicate create -> afpObjectExists
    let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("a.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, -5017);
    // A non-empty directory -> afpDirNotEmpty
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("full"));
    assert_eq!(result_of(server.command(1, &data)).0, -5007);
}

#[test]
fn test_create_does_not_inherit_old_metadata() {
    let (mut server, dir) = test_server_writable("write-stale");
    // What deleting a locked application on the host leaves behind:
    // its container, with Finder information, the WriteInhibit
    // attribute and a resource fork
    let mut finder_info = [0u8; 32];
    finder_info[..8].copy_from_slice(b"APPLtest");
    std::fs::create_dir(dir.join(".AppleDouble")).unwrap();
    std::fs::write(
        dir.join(".AppleDouble").join("x"),
        apple_double(&[(9, &finder_info), (14, &[0, 0, 0, 0x20]), (2, &[0xAA; 300])]),
    )
    .unwrap();
    // The same for a directory, as a sidecar
    std::fs::write(dir.join("._y"), apple_double(&[(9, &finder_info)])).unwrap();

    let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("x"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    let mut data = vec![6, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("y"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(!dir.join(".AppleDouble").join("x").exists());
    assert!(!dir.join("._y").exists());

    // ch13, FPCreateFile: "Finder information is set to 0, and all
    // file attributes are initially cleared", and both forks are empty.
    // [GetFileDirParms][0][vol 1][dir 2][file bm: attributes, Finder
    // info, resource fork length][dir bm 0]
    let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x04, 0x21, 0, 0];
    data.extend_from_slice(&long_path("x"));
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    // [file bm][dir bm][flag][0][attributes 2][Finder info 32][length 4]
    assert_eq!(reply[6..], [0; 38]);
}

#[test]
fn test_rename_and_move() {
    let (mut server, dir) = test_server_writable("write-rename");
    std::fs::write(dir.join("old.txt"), b"data").unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    // Rename within the root
    let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("old.txt"));
    data.extend_from_slice(&long_path("new.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(!dir.join("old.txt").exists());
    assert!(dir.join("new.txt").exists());
    // Renaming onto an existing name answers afpObjectExists and
    // replaces nothing
    std::fs::write(dir.join("keep.txt"), b"keep").unwrap();
    let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("new.txt"));
    data.extend_from_slice(&long_path("keep.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, -5017);
    assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"keep");
    assert!(dir.join("new.txt").exists());
    // The sub directory's ID, straight from the server's table
    let sub_id = server.dir_id(&dir.join("sub"));
    assert_ne!(sub_id, ROOT_DIR_ID);
    // FPMoveAndRename:
    // [vol][src dir 2][dst dir sub][src path "new.txt"]
    // [dst path ""][new name "moved.txt"]
    let mut data = vec![23, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&sub_id.to_be_bytes());
    data.extend_from_slice(&long_path("new.txt"));
    data.extend_from_slice(&long_path(""));
    data.extend_from_slice(&long_path("moved.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(dir.join("sub").join("moved.txt").exists());
    assert!(!dir.join("new.txt").exists());
    // An empty new name keeps the source's: move it back to the
    // root from the subdirectory
    let mut data = vec![23, 0, 0, 1];
    data.extend_from_slice(&sub_id.to_be_bytes()); // src dir
    data.extend_from_slice(&2u32.to_be_bytes()); // dst dir
    data.extend_from_slice(&long_path("moved.txt"));
    data.extend_from_slice(&long_path(""));
    data.extend_from_slice(&long_path(""));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(dir.join("moved.txt").exists());
    // Moving a directory into itself is afpCantMove
    let mut data = vec![23, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&sub_id.to_be_bytes());
    data.extend_from_slice(&long_path("sub"));
    data.extend_from_slice(&long_path(""));
    data.extend_from_slice(&long_path(""));
    assert_eq!(result_of(server.command(1, &data)).0, -5005);
}

#[test]
fn test_create_dir_id_ancestors_and_hard_create() {
    let (mut server, dir) = test_server_writable("write-hard");
    // FPCreateDir replies with the New Directory ID
    let mut data = vec![6, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("newfolder"));
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    assert_eq!(reply.len(), 4);
    let new_id = u32::from_be_bytes(reply.try_into().unwrap());
    assert_eq!(new_id, server.dir_id(&dir.join("newfolder")));
    // The ID opens the folder and enumerates its child
    std::fs::write(dir.join("newfolder").join("child.txt"), b"c").unwrap();
    let mut data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    data[4..8].copy_from_slice(&new_id.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    assert_eq!(reply[4..6], [0, 1]);
    let off = u16::from_be_bytes([reply[8], reply[9]]) as usize;
    assert_eq!(&reply[8 + off..8 + off + 10], b"\x09child.txt");
    // The path may name ancestors ("sub\0inner.txt")
    let mut data = vec![6, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("sub"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("sub\0inner.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(dir.join("sub").join("inner.txt").is_file());
    // A hard create (CreateFlag bit 7) replaces a closed file
    std::fs::write(dir.join("sub").join("inner.txt"), b"payload").unwrap();
    let mut data = vec![7, 0x80, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("sub\0inner.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert_eq!(
        std::fs::read(dir.join("sub").join("inner.txt")).unwrap(),
        b""
    );
    // A hard create onto a directory answers afpObjectTypeErr
    let mut data = vec![7, 0x80, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("sub"));
    assert_eq!(result_of(server.command(1, &data)).0, -5025);
    // A hard create onto an open file answers afpFileBusy
    std::fs::write(dir.join("open.txt"), b"x").unwrap();
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 8];
    data.extend_from_slice(b"open.txt");
    let (r, _) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    let mut data = vec![7, 0x80, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("open.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, -5010);
}

#[test]
fn test_move_by_own_dir_id_and_mangled_names() {
    let (mut server, dir) = test_server_writable("write-move-null");
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::create_dir(dir.join("folder")).unwrap();
    // MoveAndRename with a null Source Pathname: the directory is
    // addressed by its own Directory ID
    let folder_id = server.dir_id(&dir.join("folder"));
    let sub_id = server.dir_id(&dir.join("sub"));
    let mut data = vec![23, 0, 0, 1];
    data.extend_from_slice(&folder_id.to_be_bytes()); // src dir = itself
    data.extend_from_slice(&sub_id.to_be_bytes()); // dst dir
    data.extend_from_slice(&long_path("")); // null source path
    data.extend_from_slice(&long_path("")); // null destination path
    data.extend_from_slice(&long_path("moved")); // the new name
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(dir.join("sub").join("moved").is_dir());
    assert!(!dir.join("folder").exists());
    // Moving a file with a mangled name and an empty NewName keeps
    // its host name (the wire name would rename it on the host)
    std::fs::write(dir.join("你好.txt"), b"hi").unwrap();
    let mangled = server.mac_name_of(&dir.join("你好.txt"));
    assert!(mangled.starts_with('#'));
    let mut data = vec![23, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&sub_id.to_be_bytes());
    data.extend_from_slice(&long_path(&mangled));
    data.extend_from_slice(&long_path(""));
    data.extend_from_slice(&long_path(""));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(dir.join("sub").join("你好.txt").is_file());
    assert!(!dir.join("你好.txt").exists());
    // So does a NewName equal to the mangled name, in a move and in
    // a plain rename
    let mut data = vec![23, 0, 0, 1];
    data.extend_from_slice(&sub_id.to_be_bytes());
    data.extend_from_slice(&2u32.to_be_bytes());
    data.extend_from_slice(&long_path(&mangled));
    data.extend_from_slice(&long_path(""));
    data.extend_from_slice(&long_path(&mangled));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(dir.join("你好.txt").is_file());
    let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path(&mangled));
    data.extend_from_slice(&long_path(&mangled));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(dir.join("你好.txt").is_file());
}

#[test]
fn test_rename_folder_keeps_dir_id_usable() {
    let (mut server, dir) = test_server_writable("write-rename-dir");
    std::fs::create_dir(dir.join("folder")).unwrap();
    std::fs::write(dir.join("folder").join("child.txt"), b"c").unwrap();
    // Mint the folder's Directory ID
    let folder_id = server.dir_id(&dir.join("folder"));
    assert_ne!(folder_id, ROOT_DIR_ID);
    // Rename the folder
    let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("folder"));
    data.extend_from_slice(&long_path("renamed"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    // The same Directory ID still opens it and enumerates the child
    let mut data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    data[4..8].copy_from_slice(&folder_id.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    assert_eq!(reply[4..6], [0, 1]); // the child
    let off = u16::from_be_bytes([reply[8], reply[9]]) as usize;
    assert_eq!(&reply[8 + off..8 + off + 10], b"\x09child.txt");
    // The renamed ID resolves the child's path for a delete too
    let mut data = vec![8, 0, 0, 1];
    data.extend_from_slice(&folder_id.to_be_bytes());
    data.extend_from_slice(&long_path("child.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    // Delete the now-empty folder through the root
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path("renamed"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    // The Directory ID no longer resolves
    let mut data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    data[4..8].copy_from_slice(&folder_id.to_be_bytes());
    let (r, _) = result_of(server.command(1, &data));
    assert_eq!(r, ERR_DIR_NOT_FOUND);
}

#[test]
fn test_mangled_name_rename_and_delete() {
    let (mut server, dir) = test_server_writable("write-mangled");
    let long = format!("{}verylong.txt", "x".repeat(40));
    std::fs::write(dir.join(&long), b"data").unwrap();
    // Enumerating registers the mangled name
    let data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    let mangled = crate::name::to_mac(&long).0;
    // Rename through the mangled wire name
    let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path(&mangled));
    data.extend_from_slice(&long_path("short.txt"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(!dir.join(&long).exists());
    assert!(dir.join("short.txt").exists());
    // Delete through a mangled name again
    let long2 = format!("{}verylong.txt", "y".repeat(40));
    std::fs::write(dir.join(&long2), b"data").unwrap();
    let data = [
        9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
    ];
    let _ = result_of(server.command(1, &data));
    let mangled2 = crate::name::to_mac(&long2).0;
    let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&long_path(&mangled2));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert!(!dir.join(&long2).exists());
    let _ = reply;
}

#[test]
fn create_case_variant_is_object_exists() {
    let (mut server, dir) = test_server_writable("case");
    std::fs::write(dir.join("foo"), b"x").unwrap();
    let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&mac_path("Foo"));
    let r = result_of(server.command(1, &data)).0;
    assert_eq!(r, ERR_OBJECT_EXISTS);
}

#[test]
fn case_only_rename_never_overwrites_the_other_spelling() {
    let (mut server, dir) = test_server_writable("casedup");
    // A case-sensitive host holding both spellings: the lower-case
    // one is created first, so read_dir lists it first on file
    // systems that keep creation order (btrfs)
    let mut clobbered = Vec::new();
    for i in 0..16 {
        let lower = format!("name{i}x");
        let upper = format!("NAME{i}X");
        std::fs::write(dir.join(&lower), b"mine").unwrap();
        std::fs::write(dir.join(&upper), b"OTHER").unwrap();
        let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&mac_path(&lower));
        data.extend_from_slice(&mac_path(&upper));
        let r = result_of(server.command(1, &data)).0;
        if std::fs::read(dir.join(&upper)).unwrap() != b"OTHER" {
            clobbered.push((i, r));
        }
    }
    assert!(
        clobbered.is_empty(),
        "renames that overwrote the other spelling: {clobbered:?}"
    );
}

#[test]
#[cfg(unix)]
fn rename_onto_a_hard_link_is_object_exists() {
    let (mut server, dir) = test_server_writable("hardlink");
    std::fs::write(dir.join("a"), b"data").unwrap();
    std::fs::hard_link(dir.join("a"), dir.join("b")).unwrap();
    // Renaming a onto its own hard link must not answer noErr and
    // move the bookkeeping to a name the file never took
    let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&mac_path("a"));
    data.extend_from_slice(&mac_path("b"));
    assert_eq!(result_of(server.command(1, &data)).0, ERR_OBJECT_EXISTS);
    // The source keeps its name and both links stay in place
    assert!(dir.join("a").exists());
    assert!(dir.join("b").exists());
}

#[test]
fn case_only_rename_of_a_single_spelling_stays_allowed() {
    let (mut server, dir) = test_server_writable("casesolo");
    std::fs::write(dir.join("foo"), b"mine").unwrap();
    let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
    data.extend_from_slice(&mac_path("foo"));
    data.extend_from_slice(&mac_path("Foo"));
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    assert_eq!(std::fs::read(dir.join("Foo")).unwrap(), b"mine");
    assert!(!dir.join("foo").exists());
}

#[test]
fn a_hard_create_replaces_a_case_variant() {
    let (mut s, dir) = test_server_writable("hardcase");
    std::fs::write(dir.join("readme"), b"old").unwrap();
    let mut d = vec![7u8, 0x80, 0, 1, 0, 0, 0, 2];
    d.extend(mac_path("README"));
    assert_eq!(result_of(s.command(1, &d)).0, 0);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.eq_ignore_ascii_case("readme"))
        .collect();
    assert_eq!(names, ["README"]);
    assert_eq!(std::fs::read(dir.join("README")).unwrap(), b"");
    // A soft create still refuses it
    d[1] = 0;
    d.truncate(8);
    d.extend(mac_path("Readme"));
    assert_eq!(result_of(s.command(1, &d)).0, ERR_OBJECT_EXISTS);
}

#[test]
fn a_delete_leaves_a_same_named_file_elsewhere_reachable() {
    let long = "a".repeat(40) + ".txt";
    let (mut s, dir) = test_server_writable("mangle-sibling");
    for d in ["one", "two"] {
        std::fs::create_dir(dir.join(d)).unwrap();
        std::fs::write(dir.join(d).join(&long), b"x").unwrap();
    }
    let (mac, _) = crate::name::to_mac(&long);
    assert_eq!(gfdp(&mut s, &format!("one\0{mac}"), 0x0040, 0).0, 0);
    assert_eq!(gfdp(&mut s, &format!("two\0{mac}"), 0x0040, 0).0, 0);
    assert_eq!(
        result_of(s.command(1, &simple(8, &format!("one\0{mac}")))).0,
        0
    );
    assert!(!dir.join("one").join(&long).exists());
    assert_eq!(gfdp(&mut s, &format!("two\0{mac}"), 0x0040, 0).0, 0);
}

#[cfg(unix)]
#[test]
fn deleting_a_symlink_keeps_its_target() {
    let (mut s, dir) = test_server_writable("del-link");
    std::fs::write(dir.join("real.txt"), b"precious").unwrap();
    std::os::unix::fs::symlink("real.txt", dir.join("link.txt")).unwrap();
    let id = s.ids.id_of(&dir.canonicalize().unwrap().join("real.txt"));
    // Reached through the link first, so the ID is recorded under it
    assert_eq!(gfdp(&mut s, "link.txt", 0x0100, 0).0, 0);
    assert_eq!(result_of(s.command(1, &simple(8, "link.txt"))).0, 0);
    assert_eq!(std::fs::read(dir.join("real.txt")).unwrap(), b"precious");
    assert!(dir.join("link.txt").symlink_metadata().is_err());
    // The file keeps its ID
    assert_eq!(
        s.ids.id_of(&dir.canonicalize().unwrap().join("real.txt")),
        id
    );
}

#[cfg(unix)]
#[test]
fn deleting_a_symlink_to_a_folder_keeps_the_folder() {
    let (mut s, dir) = test_server_writable("del-dirlink");
    std::fs::create_dir(dir.join("real")).unwrap();
    std::fs::write(dir.join("real/inside"), b"x").unwrap();
    std::os::unix::fs::symlink("real", dir.join("link")).unwrap();
    assert_eq!(result_of(s.command(1, &simple(8, "link"))).0, 0);
    assert!(dir.join("real/inside").exists());
    assert!(dir.join("link").symlink_metadata().is_err());
}

#[cfg(unix)]
#[test]
fn renaming_a_symlink_renames_the_link() {
    let (mut s, dir) = test_server_writable("ren-link");
    std::fs::write(dir.join("real.txt"), b"x").unwrap();
    std::os::unix::fs::symlink("real.txt", dir.join("link.txt")).unwrap();
    let mut d = simple(28, "link.txt");
    d.extend(mac_path("new.txt"));
    assert_eq!(result_of(s.command(1, &d)).0, 0);
    assert!(dir.join("real.txt").exists());
    assert!(
        dir.join("new.txt")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_folder_with_a_parent_file_stays_undeletable_as_data() {
    // A real host file named .Parent is data: not listed, not removed
    let (mut s, dir) = test_server_writable("parent-file");
    std::fs::create_dir(dir.join("d")).unwrap();
    std::fs::write(dir.join("d/.Parent"), b"data").unwrap();
    let mut del = vec![8u8, 0, 0, 1, 0, 0, 0, 2];
    del.extend(mac_path("d"));
    assert_eq!(result_of(s.command(1, &del)).0, ERR_DIR_NOT_EMPTY);
    assert!(dir.join("d/.Parent").exists());
}

#[test]
fn deleting_a_folder_takes_its_metadata() {
    let (mut s, dir) = test_server_writable("del-folder");
    std::fs::create_dir(dir.join("d")).unwrap();
    assert_eq!(set_parms(&mut s, "d", 0x0020, &[1; 32]), 0);
    assert!(dir.join("d/.AppleDouble/.Parent").is_file());
    let mut del = vec![8u8, 0, 0, 1, 0, 0, 0, 2];
    del.extend(mac_path("d"));
    assert_eq!(result_of(s.command(1, &del)).0, 0);
    assert!(!dir.join("d").exists());
}

#[test]
fn a_renamed_folder_keeps_its_metadata() {
    let (mut s, dir) = test_server_writable("ren-folder");
    std::fs::create_dir(dir.join("d")).unwrap();
    assert_eq!(set_parms(&mut s, "d", 0x0020, &[1; 32]), 0);
    let mut ren = vec![28u8, 0, 0, 1, 0, 0, 0, 2];
    ren.extend(mac_path("d"));
    ren.extend(mac_path("e"));
    assert_eq!(result_of(s.command(1, &ren)).0, 0);
    assert_eq!(gfdp(&mut s, "e", 0, 0x0020).1[..32], [1; 32]);
}
