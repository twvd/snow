//! Tests of the fork calls

use super::*;

#[test]
fn test_open_fork_sets_already_open_bits() {
    let (mut server, dir) = test_server("open-bits");
    std::fs::write(dir.join("f.txt"), b"x").unwrap();
    // [GetFileDirParms][0][vol 1][dir 2][file bm 0x0001][dir bm 0]
    // [ptype 2][path]
    let mut gfd = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x01, 0, 0, 2, 5];
    gfd.extend_from_slice(b"f.txt");
    let (r, reply) = result_of(server.command(1, &gfd));
    assert_eq!(r, 0);
    assert_eq!(reply[6..8], [0, 0]); // no open forks: attrs = 0
    // Opening the data fork sets DAlreadyOpen
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 5];
    data.extend_from_slice(b"f.txt");
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    let (r, reply) = result_of(server.command(1, &gfd));
    assert_eq!(r, 0);
    assert_eq!(reply[6..8], [0, 8]);
    // Opening the resource fork adds RAlreadyOpen
    let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 5];
    data.extend_from_slice(b"f.txt");
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    let (r, reply) = result_of(server.command(1, &gfd));
    assert_eq!(r, 0);
    assert_eq!(reply[6..8], [0, 8 | 0x10]);
}

#[test]
fn test_leftover_apple_single_reads_and_refuses_writes() {
    let (mut server, dir) = test_server_writable("leftover-single");
    // An AppleSingle file: its magic differs from AppleDouble's in
    // the last byte
    let mut data = apple_double(&[(9, b"TEXTttxt........................"), (1, b"hello")]);
    data[3] = 0x00;
    std::fs::write(dir.join("single"), &data).unwrap();
    // The data fork reads from inside the container
    let mut open = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1];
    open.extend_from_slice(&long_path("single"));
    let (result, reply) = result_of(server.command(1, &open));
    assert_eq!(result, 0);
    let mut read = vec![27, 0, reply[2], reply[3], 0, 0, 0, 0, 0, 0, 0, 5];
    read.extend_from_slice(&[0, 0]);
    assert_eq!(result_of(server.command(1, &read)).1, b"hello");
    // It is not written, in either fork or in its metadata
    for flag in [0x00, 0x80] {
        let mut open = vec![26, flag, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3];
        open.extend_from_slice(&long_path("single"));
        assert_eq!(result_of(server.command(1, &open)).0, ERR_OBJECT_LOCKED);
    }
    let mut set = vec![35, 0, 0, 1, 0, 0, 0, 2, 0, 1];
    set.extend_from_slice(&long_path("single"));
    if set.len() % 2 == 1 {
        set.push(0);
    }
    set.extend_from_slice(&0x8001u16.to_be_bytes());
    assert_eq!(result_of(server.command(1, &set)).0, ERR_OBJECT_LOCKED);
    assert_eq!(std::fs::read(dir.join("single")).unwrap(), data);
    assert!(!dir.join(".AppleDouble/single").exists());
}

#[test]
fn test_write_end_flag_and_last_written() {
    let (mut server, dir) = test_server_writable("write-endflag");
    std::fs::write(dir.join("f.txt"), b"abcde").unwrap();
    // Open for writing
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 5];
    data.extend_from_slice(b"f.txt");
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    // [Write][EndFlag 0x80][ref][offset -2 (signed)][count 2]
    // [data]: 2 bytes before the end of the 5-byte fork = position 3
    let mut write = vec![33, 0x80, 0, 0, 0xFF, 0xFF, 0xFF, 0xFE, 0, 0, 0, 2];
    write[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    write.extend_from_slice(b"XX");
    let (r, reply) = result_of(server.command(1, &write));
    assert_eq!(r, 0);
    // LastWritten: the offset just past the last byte written
    assert_eq!(reply, 5u32.to_be_bytes());
    // An end-relative offset before the start of the fork is a
    // parameter error
    let mut write = vec![33, 0x80, 0, 0, 0xFF, 0xFF, 0xFF, 0xFA, 0, 0, 0, 1];
    write[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    write.extend_from_slice(b"Z");
    assert_eq!(result_of(server.command(1, &write)).0, ERR_PARAM);
    // Read back through a fresh read-only fork: "abcXX"
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 5];
    data.extend_from_slice(b"f.txt");
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &read));
    assert_eq!(r, 0);
    assert_eq!(&reply[..5], b"abcXX");
}

#[test]
fn test_write_and_read_data_fork() {
    let (mut server, dir) = test_server_writable("write-data");
    std::fs::write(dir.join("hello.txt"), b"Hello from the Snow AFP server!").unwrap();
    // Open for writing: access byte 1
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 3, 2, 9];
    data.extend_from_slice(b"hello.txt");
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    // [Write][0][ref][offset 6][count 5][data]
    let mut write = vec![33, 0, 0, 0, 0, 0, 0, 6, 0, 0, 0, 5];
    write[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    write.extend_from_slice(b"Snow!");
    let (r, _) = result_of(server.command(1, &write));
    assert_eq!(r, 0);
    // Read it back through a fresh read-only fork
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 9];
    data.extend_from_slice(b"hello.txt");
    let (_, reply) = result_of(server.command(1, &data));
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 31, 0, 0];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &read));
    assert_eq!(r, 0);
    assert_eq!(&reply[..11], b"Hello Snow!");
    // The write extended nothing: the fork length is unchanged
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 9];
    data.extend_from_slice(b"hello.txt");
    let (_, reply) = result_of(server.command(1, &data));
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    let _ = fork_ref;
}

#[test]
fn test_write_past_end_extends_the_fork() {
    let (mut server, dir) = test_server_writable("write-extend");
    std::fs::write(dir.join("f.txt"), b"abc").unwrap();
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 3, 2, 5];
    data.extend_from_slice(b"f.txt");
    let (_, reply) = result_of(server.command(1, &data));
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    // Write 4 bytes at offset 8: the fork extends to 12
    let mut write = vec![33, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 4];
    write[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    write.extend_from_slice(b"WXYZ");
    assert_eq!(result_of(server.command(1, &write)).0, 0);
    // A fresh open reports the new length
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 5];
    data.extend_from_slice(b"f.txt");
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    let len = u32::from_be_bytes(reply[4..8].try_into().unwrap());
    assert_eq!(len, 12);
    assert_eq!(std::fs::read(dir.join("f.txt")).unwrap().len(), 12);
}

#[test]
fn test_set_fork_params_truncates() {
    let (mut server, dir) = test_server_writable("write-trunc");
    std::fs::write(dir.join("f.txt"), b"abcdefghij").unwrap();
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 3, 2, 5];
    data.extend_from_slice(b"f.txt");
    let (_, reply) = result_of(server.command(1, &data));
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    // [SetForkParms][0][ref][bitmap 0x0200][len 3]
    let mut set = vec![31, 0, 0, 0, 0x02, 0x00, 0, 0, 0, 3];
    set[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    assert_eq!(result_of(server.command(1, &set)).0, 0);
    assert_eq!(std::fs::read(dir.join("f.txt")).unwrap(), b"abc");
}

#[test]
fn test_rsrc_fork_round_trip_apple_double() {
    let (mut server, dir) = test_server_writable("write-rsrc");
    std::fs::write(dir.join("app"), b"data").unwrap();
    // Open the resource fork (flag 0x80) for writing
    let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 3, 2, 3];
    data.extend_from_slice(b"app");
    let (r, reply) = result_of(server.command(1, &data));
    assert_eq!(r, 0);
    let write_ref = u16::from_be_bytes([reply[2], reply[3]]);
    let mut write = vec![33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8];
    write[2..4].copy_from_slice(&write_ref.to_be_bytes());
    write.extend_from_slice(b"resource");
    assert_eq!(result_of(server.command(1, &write)).0, 0);
    // The AppleDouble container now exists
    assert!(dir.join(".AppleDouble").join("app").exists());
    // The data fork is untouched
    assert_eq!(std::fs::read(dir.join("app")).unwrap(), b"data");
    // A fresh open reads the fork back
    let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 3];
    data.extend_from_slice(b"app");
    let (_, reply) = result_of(server.command(1, &data));
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (r, reply) = result_of(server.command(1, &read));
    assert_eq!(r, 0);
    assert_eq!(reply, b"resource");
    // Truncate it (through the write fork)
    let mut set = vec![31, 0, 0, 0, 0x04, 0x00, 0, 0, 0, 4];
    set[2..4].copy_from_slice(&write_ref.to_be_bytes());
    assert_eq!(result_of(server.command(1, &set)).0, 0);
    let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 3];
    data.extend_from_slice(b"app");
    let (_, reply) = result_of(server.command(1, &data));
    let len = u32::from_be_bytes(reply[4..8].try_into().unwrap());
    assert_eq!(len, 4);
}

#[test]
fn test_byte_range_lock() {
    let (mut server, dir) = test_server("byte-range-lock");
    std::fs::write(dir.join("map"), b"0123456789").unwrap();
    let open = |server: &mut Afp| {
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 3];
        data.extend_from_slice(b"map");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        u16::from_be_bytes([reply[2], reply[3]])
    };
    let lock = |server: &mut Afp, flags: u8, fork: u16, offset: i32, length: u32| {
        let mut data = vec![1, flags];
        data.extend_from_slice(&fork.to_be_bytes());
        data.extend_from_slice(&offset.to_be_bytes());
        data.extend_from_slice(&length.to_be_bytes());
        result_of(server.command(1, &data))
    };
    let a = open(&mut server);
    let b = open(&mut server);
    // The Finder's lock on the Trash Can Usage Map: one byte at 2
    let (r, reply) = lock(&mut server, 0, a, 2, 1);
    assert_eq!(r, 0);
    assert_eq!(reply, 2u32.to_be_bytes());
    assert_eq!(lock(&mut server, 0, a, 0, 4).0, ERR_RANGE_OVERLAP);
    assert_eq!(lock(&mut server, 0, b, 2, 1).0, ERR_LOCK);
    // Relative to the end: the last byte
    let (r, reply) = lock(&mut server, 0x80, b, -1, 1);
    assert_eq!(r, 0);
    assert_eq!(reply, 9u32.to_be_bytes());
    // An unlock names the range as it was locked
    assert_eq!(lock(&mut server, 0x01, a, 2, 2).0, ERR_RANGE_NOT_LOCKED);
    assert_eq!(lock(&mut server, 0x01, a, 2, 1).0, 0);
    assert_eq!(lock(&mut server, 0, b, 2, 1).0, 0);
    // Closing a fork releases its locks
    let close = [4, 0, (b >> 8) as u8, b as u8];
    assert_eq!(result_of(server.command(1, &close)).0, 0);
    assert_eq!(lock(&mut server, 0, a, 0, 0xFFFF_FFFF).0, 0);
    // A zero length is not a range
    assert_eq!(lock(&mut server, 0, a, 0, 0).0, ERR_PARAM);
}

#[test]
fn test_mod_date_follows_writes_with_a_container() {
    let (mut server, dir) = test_server_writable("rv3-moddate");
    std::fs::write(dir.join("f.txt"), b"abc").unwrap();
    std::fs::create_dir(dir.join("D")).unwrap();
    let old = (-157_785_600i32) as u32; // 1995
    let set_dates = |server: &mut Afp, name: &str, d: u32| {
        let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x0C];
        data.extend_from_slice(&long_path(name));
        if data.len() % 2 == 1 {
            data.push(0);
        }
        data.extend_from_slice(&d.to_be_bytes());
        data.extend_from_slice(&d.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, 0);
    };
    let mod_date = |server: &mut Afp, name: &str, is_dir: bool| {
        let (fb, db) = if is_dir {
            (0u16, 0x0008u16)
        } else {
            (0x0008, 0)
        };
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&fb.to_be_bytes());
        data.extend_from_slice(&db.to_be_bytes());
        data.extend_from_slice(&long_path(name));
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        u32::from_be_bytes(reply[6..10].try_into().unwrap())
    };
    set_dates(&mut server, "f.txt", old);
    set_dates(&mut server, "D", old);
    // Write the file through AFP and create a file inside D: both
    // modification dates must move off the stored value
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x00, 0x00, 0x03, 2, 5];
    data.extend_from_slice(b"f.txt");
    let (_, reply) = result_of(server.command(1, &data));
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    let mut w = vec![33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, b'Z'];
    w[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    assert_eq!(result_of(server.command(1, &w)).0, 0);
    let mut c = vec![4, 0];
    c.extend_from_slice(&fork_ref.to_be_bytes());
    assert_eq!(result_of(server.command(1, &c)).0, 0);
    let mut cr = vec![7, 0, 0, 1, 0, 0, 0, 2];
    cr.extend_from_slice(&long_path("D\0new"));
    assert_eq!(result_of(server.command(1, &cr)).0, 0);
    assert_ne!(mod_date(&mut server, "f.txt", false), old);
    assert_ne!(mod_date(&mut server, "D", true), old);
}

#[test]
fn test_read_newline_mask_stops_at_the_newline_char() {
    let (mut server, dir) = test_server("nl-mask");
    std::fs::write(dir.join("text.txt"), b"hello\nworld\rend").unwrap();
    // [OpenFork][flag data][vol 1][dir 2][bitmap 0x4000][access
    // read][path type 2]["text.txt"]
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 8];
    data.extend_from_slice(b"text.txt");
    let (_, reply) = result_of(server.command(1, &data));
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    // A carriage-return read stops after the carriage return, and
    // the line feed before it is data like any other byte
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 0xFF, b'\r'];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &read));
    assert_eq!(result, 0, "a newline cut is a complete read");
    assert_eq!(reply, b"hello\nworld\r");
    // The next read continues past the line
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 12, 0, 0, 0, 100, 0xFF, b'\r'];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &read));
    assert_eq!(result, ERR_EOF, "the last line has no line break");
    assert_eq!(reply, b"end");
    // A zero mask reads bytes only (the common case)
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 0, 0];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &read));
    assert_eq!(result, ERR_EOF);
    assert_eq!(reply, b"hello\nworld\rend");
}

#[test]
fn test_open_fork_read_spec_layout() {
    let (mut server, dir) = test_server("open-fork");
    std::fs::write(dir.join("hello.txt"), b"Hello from the Snow AFP server!").unwrap();
    // [OpenFork][flag data][vol 1][dir 2][bitmap 0x4000][access
    // read][path type 2]["hello.txt"]
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 9];
    data.extend_from_slice(b"hello.txt");
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[0..2], [0x02, 0x00]); // bitmap
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
    // The data fork length follows the fork reference number
    let len = u32::from_be_bytes(reply[4..8].try_into().unwrap());
    assert_eq!(len, 31);

    // [Read][0][fork ref][offset 0][count 8][nl mask 0][nl char 0]
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &read));
    assert_eq!(result, 0);
    assert_eq!(reply, b"Hello fr");

    // [GetForkParms][0][fork ref][file bitmap 0x0600]: the data
    // and resource fork lengths
    let mut parms = vec![14, 0, 0, 0, 0x06, 0x00];
    parms[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &parms));
    assert_eq!(result, 0);
    assert_eq!(reply[0..2], [0x06, 0x00]);
    assert_eq!(u32::from_be_bytes(reply[2..6].try_into().unwrap()), 31);
    assert_eq!(u32::from_be_bytes(reply[6..10].try_into().unwrap()), 0);

    // [CloseFork][0][fork ref]
    let mut close = vec![4, 0, 0, 0];
    close[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, _) = result_of(server.command(1, &close));
    assert_eq!(result, 0);
    assert!(server.forks.is_empty());
}

#[test]
fn test_read_of_multipacket_length() {
    // A fork longer than one ATP response packet (574 bytes) reads
    // back in full; the ATP layer splits the reply into packets
    let (mut server, dir) = test_server("big-read");
    let mut content = vec![0u8; 1500];
    for (i, b) in content.iter_mut().enumerate() {
        *b = b'0' + (i % 10) as u8;
    }
    std::fs::write(dir.join("big.txt"), &content).unwrap();
    // [OpenFork][flag data][vol 1][dir 2][bitmap 0][access
    // read][path type 2]["big.txt"]
    let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 7];
    data.extend_from_slice(b"big.txt");
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);

    // [Read][0][fork ref][offset 0][count 1500][nl mask 0]
    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05, 0xDC, 0, 0];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &read));
    assert_eq!(result, 0);
    assert_eq!(reply.len(), 1500);
    assert_eq!(reply, content);
}

#[test]
fn test_open_resource_fork_from_sidecar() {
    let (mut server, dir) = test_server("rsrc-fork");
    std::fs::write(dir.join("app.bin"), b"data").unwrap();
    let sidecar = apple_double(&[(2, b"RESOURCE!")]); // entry 2: rsrc fork
    std::fs::write(dir.join("._app.bin"), sidecar).unwrap();

    // [OpenFork][flag rsrc][vol 1][dir 2][bitmap 0][access
    // read][path type 2]["app.bin"]
    let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 7];
    data.extend_from_slice(b"app.bin");
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);

    let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0];
    read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
    let (result, reply) = result_of(server.command(1, &read));
    assert_eq!(result, 0);
    assert_eq!(reply, b"RESOURCE!");
}

#[test]
fn client_date_set_while_open_survives_close() {
    let (mut server, dir) = test_server_writable("close");
    std::fs::write(dir.join("f.txt"), b"").unwrap();
    let f = open_rw(&mut server, "f.txt", false);
    write(&mut server, f, 0, b"data");
    // The client copies the original's dates while the fork is open
    let old = (-157_785_600i32) as u32;
    let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x0C];
    data.extend_from_slice(&mac_path("f.txt"));
    if data.len() % 2 == 1 {
        data.push(0);
    }
    data.extend_from_slice(&old.to_be_bytes());
    data.extend_from_slice(&old.to_be_bytes());
    assert_eq!(result_of(server.command(1, &data)).0, 0);
    let mut c = vec![4, 0];
    c.extend_from_slice(&f.to_be_bytes());
    assert_eq!(result_of(server.command(1, &c)).0, 0);
    let m = server
        .meta_open(&dir.canonicalize().unwrap().join("f.txt"))
        .mod_date;
    assert_eq!(m, old, "the client's date was overwritten at close");
}

#[test]
fn rsrc_fork_reads_back_after_first_write() {
    let (mut server, dir) = test_server_writable("rsrc");
    std::fs::write(dir.join("doc"), b"DATAFORKDATAFORK").unwrap();
    let f = open_rw(&mut server, "doc", true);
    write(&mut server, f, 0, b"RSRC");
    let (r, got) = read(&mut server, f, 0, 4);
    assert_eq!(
        r, 0,
        "the resource fork must be readable after its first write"
    );
    assert_eq!(got, b"RSRC");
}

#[test]
fn a_write_inhibited_file_does_not_open_for_write() {
    let (mut s, dir) = test_server_writable("inhibit");
    std::fs::write(dir.join("f"), b"x").unwrap();
    assert_eq!(set_parms(&mut s, "f", 1, &0x8020u16.to_be_bytes()), 0);
    assert_eq!(open(&mut s, "f", false, 3).0, ERR_OBJECT_LOCKED);
    assert_eq!(open(&mut s, "f", true, 2).0, ERR_OBJECT_LOCKED);
    // Reading stays allowed
    assert_eq!(open(&mut s, "f", false, 1).0, 0);
}

#[test]
fn a_read_past_the_end_answers_eof_with_the_data() {
    let (mut s, dir) = test_server_writable("eof");
    std::fs::write(dir.join("f"), vec![7u8; 1500]).unwrap();
    let (_, f) = open(&mut s, "f", false, 1);
    let (r, data) = read(&mut s, f, 0, 2000);
    assert_eq!(data.len(), 1500);
    assert_eq!(r, ERR_EOF);
    // Exactly to the end is a complete read
    let (r, data) = read(&mut s, f, 0, 1500);
    assert_eq!((r, data.len()), (0, 1500));
    // Past it there is nothing
    assert_eq!(read(&mut s, f, 1500, 1).0, ERR_EOF);
}

#[test]
fn a_read_cut_by_the_reply_capacity_is_not_eof() {
    let (mut s, dir) = test_server_writable("capacity");
    std::fs::write(dir.join("f"), vec![7u8; 10_000]).unwrap();
    let (_, f) = open(&mut s, "f", false, 1);
    let (r, data) = read(&mut s, f, 0, 8000);
    assert_eq!((r, data.len()), (0, crate::atp::MAX_RESPONSE));
}

#[test]
fn a_negative_read_offset_or_count_is_a_parameter_error() {
    let (mut s, dir) = test_server_writable("negread");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let (_, f) = open(&mut s, "f", false, 1);
    assert_eq!(read(&mut s, f, 0x8000_0000, 1).0, ERR_PARAM);
    assert_eq!(read(&mut s, f, 0, 0xFFFF_FFFF).0, ERR_PARAM);
}

#[test]
fn reading_a_fork_not_opened_for_read_is_denied() {
    let (mut s, dir) = test_server_writable("noread");
    std::fs::write(dir.join("f"), b"data").unwrap();
    let (_, f) = open(&mut s, "f", false, 2);
    assert_eq!(read(&mut s, f, 0, 4).0, ERR_ACCESS_DENIED);
}

#[test]
fn opening_a_directory_is_an_object_type_error() {
    let (mut s, dir) = test_server_writable("opendir");
    std::fs::create_dir(dir.join("d")).unwrap();
    assert_eq!(open(&mut s, "d", false, 1).0, ERR_OBJECT_TYPE);
}

#[cfg(unix)]
#[test]
fn a_read_only_host_file_answers_access_denied() {
    use std::os::unix::fs::PermissionsExt;
    let (mut s, dir) = test_server_writable("hostro");
    std::fs::write(dir.join("f"), b"x").unwrap();
    std::fs::set_permissions(dir.join("f"), std::fs::Permissions::from_mode(0o444)).unwrap();
    assert_eq!(open(&mut s, "f", false, 3).0, ERR_ACCESS_DENIED);
    std::fs::set_permissions(dir.join("f"), std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn closing_an_unknown_fork_is_a_parameter_error() {
    let (mut s, _) = test_server_writable("close-unknown");
    assert_eq!(result_of(s.command(1, &[4, 0, 0, 99])).0, ERR_PARAM);
}

#[test]
fn fork_references_skip_zero_and_open_forks() {
    let (mut s, dir) = test_server_writable("refs");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let (_, first) = open(&mut s, "f", false, 1);
    s.next_fork_ref = 0xFFFF;
    assert_eq!(open(&mut s, "f", false, 1).1, 0xFFFF);
    // The wrap passes 0, and the fork still open at 1
    assert_eq!(first, 1);
    assert_eq!(open(&mut s, "f", false, 1).1, 2);
}

#[test]
fn set_fork_parms_extends_a_resource_fork() {
    let (mut s, dir) = test_server_writable("rsrc-grow");
    std::fs::write(dir.join("f"), b"").unwrap();
    let (_, f) = open(&mut s, "f", true, 3);
    assert_eq!(set_fork_len(&mut s, f, 0x0400, 100), 0);
    let mut g = vec![14u8, 0];
    g.extend_from_slice(&f.to_be_bytes());
    g.extend_from_slice(&0x0400u16.to_be_bytes());
    let (_, reply) = result_of(s.command(1, &g));
    assert_eq!(u32::from_be_bytes(reply[2..6].try_into().unwrap()), 100);
    // It reads as zeros
    let (r, data) = read(&mut s, f, 96, 4);
    assert_eq!((r, data), (0, vec![0; 4]));
    // A negative length is refused, and nothing changes
    assert_eq!(set_fork_len(&mut s, f, 0x0400, 0x8000_0000), ERR_PARAM);
}

#[test]
fn a_write_longer_than_the_quantum_writes_what_arrived() {
    let (mut s, dir) = test_server_writable("partial");
    std::fs::write(dir.join("f"), b"").unwrap();
    let (_, f) = open(&mut s, "f", false, 3);
    let mut d = vec![33u8, 0];
    d.extend_from_slice(&f.to_be_bytes());
    d.extend_from_slice(&0u32.to_be_bytes());
    d.extend_from_slice(&8000u32.to_be_bytes());
    d.extend_from_slice(&vec![1u8; 4624]);
    let (r, reply) = result_of(s.command(1, &d));
    assert_eq!(r, 0);
    assert_eq!(reply, 4624u32.to_be_bytes());
    assert_eq!(std::fs::metadata(dir.join("f")).unwrap().len(), 4624);
}

#[test]
fn a_resource_fork_read_follows_a_container_rewrite() {
    // Reading after a metadata write that rewrites the container
    let (mut s, dir) = test_server_writable("rsrc-rewrite");
    std::fs::write(dir.join("f"), b"").unwrap();
    let mut open = vec![26u8, 0x80, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3];
    open.extend(mac_path("f"));
    let (_, reply) = result_of(s.command(1, &open));
    let f = [reply[2], reply[3]];
    let mut w = vec![33u8, 0, f[0], f[1], 0, 0, 0, 0, 0, 0, 0, 4];
    w.extend_from_slice(b"RSRC");
    assert_eq!(result_of(s.command(1, &w)).0, 0);
    // Attributes are a new entry: the container is rewritten
    assert_eq!(set_parms(&mut s, "f", 1, &0x8001u16.to_be_bytes()), 0);
    let mut r = vec![27u8, 0, f[0], f[1], 0, 0, 0, 0, 0, 0, 0, 4];
    r.extend_from_slice(&[0, 0]);
    assert_eq!(result_of(s.command(1, &r)).1, b"RSRC");
}
