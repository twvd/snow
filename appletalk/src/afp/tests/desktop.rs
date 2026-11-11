//! Tests of the desktop database calls

use super::*;

#[test]
fn test_desktop_database_commands() {
    let (mut server, dir) = test_server("desktop-db");
    // An APPL file whose AppleDouble sidecar carries its Finder
    // info and a resource fork with a BNDL/FREF/ICN# bundle
    std::fs::write(dir.join("App"), b"x").unwrap();
    std::fs::create_dir(dir.join(".AppleDouble")).unwrap();
    let mut finder = Vec::new();
    finder.extend_from_slice(b"APPL"); // file type
    finder.extend_from_slice(b"TEST"); // creator
    finder.extend_from_slice(&[0u8; 24]);
    let fork = crate::rfork::test_support::fork(
        &crate::rfork::test_support::bndl_with_fref(),
        Some(&crate::rfork::test_support::fref_entries()),
    );
    let container = apple_double(&[(2, &fork), (9, &finder)]);
    std::fs::write(dir.join(".AppleDouble").join("App"), container).unwrap();

    // FPOpenDT hands out the desktop database reference number
    let (r, reply) = result_of(server.command(1, &[48, 0, 0, 1]));
    assert_eq!(r, 0);
    assert_eq!(reply, [0, 1]);

    // FPGetFileDirParms on the application registers it in the
    // desktop database: the tables are fed by the files the client
    // asks about
    let mut gfd = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 3];
    gfd.extend_from_slice(b"App");
    let (r, _) = result_of(server.command(1, &gfd));
    assert_eq!(r, 0);

    // FPGetAPPL: creator 'TEST', index 0, bitmap 0x0340 (long name,
    // file number, data fork length)
    let mut cmd = vec![55, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(&[0, 0]);
    cmd.extend_from_slice(&0x0340u16.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    assert_eq!(reply[0..2], 0x0340u16.to_be_bytes());
    assert_eq!(reply[6..8], [0, 10]); // the long name offset
    assert_eq!(&reply[16..20], &[3, b'A', b'p', b'p']);
    assert_eq!(reply[12..16], [0, 0, 0, 1]); // the data fork length

    // FPGetIconInfo: creator 'TEST', index 0 — the APPL icon
    let mut cmd = vec![52, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(&[0, 0]);
    let (r, reply) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    assert_eq!(&reply[0..4], b"APPL"); // the icon tag
    assert_eq!(&reply[4..8], b"APPL"); // the file type
    assert_eq!(reply[8], 3); // large 8-bit color, the best available
    assert_eq!(reply[9], 0); // reserved
    assert_eq!(reply[10..12], [0x04, 0x00]); // 1024 bytes

    // FPGetIcon: 'TEST'/'APPL', IconType 1 — the icon bitmap
    let mut cmd = vec![51, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(b"APPL");
    cmd.extend_from_slice(&[1, 0]);
    cmd.extend_from_slice(&256u16.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    assert_eq!(reply.len(), 256);
    assert_eq!(reply[0], 0x11);

    // A request for only the first 128 bytes of the icon
    let mut cmd = vec![51, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(b"APPL");
    cmd.extend_from_slice(&[1, 0]);
    cmd.extend_from_slice(&128u16.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    assert_eq!(reply.len(), 128);

    // A zero length only tests for the icon's presence
    let mut cmd = vec![51, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(b"APPL");
    cmd.extend_from_slice(&[1, 0, 0, 0]);
    let (r, reply) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    assert!(reply.is_empty());

    // FPGetIcon for the 8-bit color icon (IconType 3)
    let mut cmd = vec![51, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(b"APPL");
    cmd.extend_from_slice(&[3, 0]);
    cmd.extend_from_slice(&1024u16.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    assert_eq!(reply.len(), 1024);
    assert_eq!(reply[0], 0x33);

    // FPGetIcon for the 4-bit color icon (IconType 2)
    let mut cmd = vec![51, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(b"APPL");
    cmd.extend_from_slice(&[2, 0]);
    cmd.extend_from_slice(&512u16.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    assert_eq!(reply.len(), 512);
    assert_eq!(reply[0], 0x55);

    // FPGetIcon for the small 4-bit icon (IconType 4)
    let mut cmd = vec![51, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(b"APPL");
    cmd.extend_from_slice(&[4, 0]);
    cmd.extend_from_slice(&128u16.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    assert_eq!(reply.len(), 128);
    assert_eq!(reply[0], 0x99);

    // FPGetIcon for the small monochrome icon (IconType 5)
    let mut cmd = vec![51, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(b"APPL");
    cmd.extend_from_slice(&[5, 0]);
    cmd.extend_from_slice(&64u16.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    assert_eq!(reply.len(), 64);
    assert_eq!(reply[0], 0xbb);

    // An IconType the server has nothing for
    let mut cmd = vec![51, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(b"APPL");
    cmd.extend_from_slice(&[9, 0]);
    cmd.extend_from_slice(&64u16.to_be_bytes());
    let (r, _) = result_of(server.command(1, &cmd));
    assert_eq!(r, ERR_ITEM_NOT_FOUND);

    // FPGetAPPL past the end of the mappings
    let mut cmd = vec![55, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(&[0, 2]);
    cmd.extend_from_slice(&0x0340u16.to_be_bytes());
    let (r, _) = result_of(server.command(1, &cmd));
    assert_eq!(r, ERR_ITEM_NOT_FOUND);

    // FPGetIconInfo past the end
    let mut cmd = vec![52, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(&[0, 9]);
    let (r, _) = result_of(server.command(1, &cmd));
    assert_eq!(r, ERR_ITEM_NOT_FOUND);

    // FPGetIcon for a creator with no icons
    let mut cmd = vec![51, 0, 0, 1];
    cmd.extend_from_slice(b"NOPE");
    cmd.extend_from_slice(b"APPL");
    cmd.extend_from_slice(&[1, 0]);
    cmd.extend_from_slice(&256u16.to_be_bytes());
    let (r, _) = result_of(server.command(1, &cmd));
    assert_eq!(r, ERR_ITEM_NOT_FOUND);

    // FPAddAPPL is refused on the read-only volume
    let mut cmd = vec![53, 0, 0, 1, 0, 0, 0, 2];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(&[0, 0, 0, 0, 2, 0]);
    let (r, _) = result_of(server.command(1, &cmd));
    assert_eq!(r, ERR_VOL_LOCKED);

    // A bad DTRefNum is a parameter error
    let (r, _) = result_of(server.command(1, &[52, 0, 0, 9, b'T', b'E', b'S', b'T', 0, 0]));
    assert_eq!(r, ERR_PARAM);

    // A volume the server does not have cannot be opened
    let (r, _) = result_of(server.command(1, &[48, 0, 0, 9]));
    assert_eq!(r, ERR_OBJECT_NOT_FOUND);
}

#[test]
fn test_desktop_scan_merges_in_background() {
    let (mut server, dir) = test_server("desktop-scan");
    std::fs::create_dir(dir.join(".AppleDouble")).unwrap();
    let mut finder = Vec::new();
    finder.extend_from_slice(b"APPL");
    finder.extend_from_slice(b"TEST");
    finder.extend_from_slice(&[0u8; 24]);
    for i in 0..5 {
        let name = format!("app{i}");
        std::fs::write(dir.join(&name), b"x").unwrap();
        std::fs::write(
            dir.join(".AppleDouble").join(&name),
            apple_double(&[(9, &finder)]),
        )
        .unwrap();
    }
    // The scan walks the tree on its own thread and the findings
    // merge as they arrive
    server.start_scan();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        server.poll_scan();
        if server
            .desktop
            .as_ref()
            .map(|db| db.application_count())
            .unwrap_or(0)
            >= 5
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the scan never merged"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // FPGetAPPL finds one of the scanned applications
    let mut cmd = vec![55, 0, 0, 1];
    cmd.extend_from_slice(b"TEST");
    cmd.extend_from_slice(&[0, 0]);
    cmd.extend_from_slice(&0u16.to_be_bytes());
    let (r, _) = result_of(server.command(1, &cmd));
    assert_eq!(r, 0);
    // Starting the scan again is a no-op
    server.start_scan();
    assert_eq!(server.desktop.as_ref().unwrap().application_count(), 5);
}

#[test]
fn test_desktop_db_writes_acknowledge_when_writable() {
    let (mut server, _) = test_server_writable("write-dtdb");
    for cmd in [53u8, 54, 56, 57, 192] {
        let (r, _) = result_of(server.command(1, &[cmd, 0]));
        assert_eq!(r, 0, "command {cmd}");
    }
}

#[test]
fn a_missing_comment_is_item_not_found() {
    let (mut s, _) = test_server_writable("comment");
    let d = [58u8, 0, 0, 1, 0, 0, 0, 2, 2, 0];
    assert_eq!(result_of(s.command(1, &d)).0, ERR_ITEM_NOT_FOUND);
}
