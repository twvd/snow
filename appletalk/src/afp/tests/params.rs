//! Tests of the volume, file and directory parameters, logins and versions

use super::*;

#[test]
fn legacy_volume_size_is_positive() {
    assert_eq!(clamp32(5_000_000), 5_000_000);
    assert_eq!(clamp32(u64::MAX), 0x7FFF_FFFF);
}

#[test]
fn test_vol_params_layout() {
    let vol = vol_params(0x0120, "Snow", Path::new("/"), true);
    // Bitmap, then the volume ID, then the name offset, then "Snow"
    assert_eq!(
        &vol[..10],
        &[0x01, 0x20, 0x00, 0x01, 0x00, 0x04, 4, b'S', b'n', b'o']
    );
    assert_eq!(&vol[10..], b"w");
    // The backup date is "never"
    let vol = vol_params(0x0010, "Snow", Path::new("/"), true);
    assert_eq!(&vol[2..], &0x8000_0000u32.to_be_bytes());
}

#[test]
fn test_vol_params_name_offset_precedes_extended_fields() {
    // Bits 8 (name), 10 (extended total) and 11 (block size): the
    // name-offset field comes first in bitmap order, the name
    // follows all the fixed fields at the recorded offset
    let vol = vol_params(0x0D00, "Snow", Path::new("/"), true);
    assert_eq!(&vol[..2], &[0x0D, 0x00]);
    assert_eq!(&vol[2..4], &14u16.to_be_bytes()); // the name offset
    assert_eq!(&vol[12..16], &4096u32.to_be_bytes()); // block size
    assert_eq!(&vol[16..21], &[4, b'S', b'n', b'o', b'w']); // "Snow"
}

#[test]
fn test_open_vol_spec_layout() {
    let (mut server, _dir) = test_server("open-vol");
    // [OpenVol][0][bitmap 0x0120]["Snow"]
    let data = [24, 0, 0x01, 0x20, 4, b'S', b'n', b'o', b'w'];
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(&reply[..6], &[0x01, 0x20, 0x00, 0x01, 0x00, 0x04]);
    assert_eq!(&reply[7..11], b"Snow");
    // A null bitmap and a bit no version defines are BitmapErr, and
    // the 2.2 bits are only for a 2.2 session
    for bitmap in [0x0000u16, 0x1000, 0x0200] {
        let mut data = vec![17, 0, 0, 1];
        data.extend_from_slice(&bitmap.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, ERR_BITMAP);
    }
    server.versions.insert(1, AfpVersion::V22);
    assert_eq!(
        result_of(server.command(1, &[17, 0, 0, 1, 0x02, 0x00])).0,
        0
    );
}

#[test]
fn test_volume_attributes_follow_read_only() {
    let (mut server, _) = test_server("attrs-ro");
    // [GetVolParms][0][vol 1][bitmap 0x0001] -> attributes = 1
    let (r, reply) = result_of(server.command(1, &[17, 0, 0, 1, 0, 1]));
    assert_eq!(r, 0);
    assert_eq!(reply, [0, 1, 0, 1]);
    let (mut server, _) = test_server_writable("attrs-rw");
    let (r, reply) = result_of(server.command(1, &[17, 0, 0, 1, 0, 1]));
    assert_eq!(r, 0);
    assert_eq!(reply, [0, 1, 0, 0]);
}

#[test]
fn test_access_rights_wire_order() {
    // Big-endian: [UARights][world][group][owner]; UARights has
    // the owner bit set plus search and read, and the write bits
    // only on a writable volume
    assert_eq!(access_rights(true), [0x83, 0x03, 0x03, 0x03]);
    assert_eq!(access_rights(false), [0x87, 0x07, 0x07, 0x07]);
}

#[test]
fn test_dir_params_access_rights_bit() {
    let meta = crate::meta::open(Path::new("/nonexistent"));
    // Bit 12 (Access Rights) + bit 6 (Long Name)
    let params = dir_params(0x1000 | 0x0040, "sub", &meta, 0, ROOT_DIR_ID, 1, true);
    // Long-name offset (2 bytes) then the 4-byte access rights,
    // then the name
    assert_eq!(&params[0..2], &[0x00, 0x06]);
    assert_eq!(&params[2..6], &[0x83, 0x03, 0x03, 0x03]);
    assert_eq!(&params[6..10], &[3, b's', b'u', b'b']);
}

#[test]
fn test_file_dir_parms_finder_info() {
    let (mut server, dir) = test_server("finder-info");
    std::fs::write(dir.join("doc.txt"), b"body").unwrap();
    let mut finder = [0u8; 32];
    finder[..4].copy_from_slice(b"ttro");
    let sidecar = apple_double(&[(9, &finder)]); // entry 9: Finder info
    std::fs::write(dir.join("._doc.txt"), sidecar).unwrap();

    // [GetFileDirParms][0][vol 1][dir 2][file bm 0x0007][dir bm 0]
    // [path type 2]["doc.txt"]
    let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x00, 0xE0, 0, 0, 2, 7];
    data.extend_from_slice(b"doc.txt");
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[0..2], [0x00, 0xE0]); // file bitmap
    assert_eq!(reply[2..4], [0, 0]); // directory bitmap
    assert_eq!(reply[4], 0); // a file
    assert_eq!(reply[5], 0); // the reserved byte after the flag
    // Parameters: Finder info, then the long-name and short-name
    // offsets, then the names. The offsets live after the 32-byte
    // Finder info block, at the end of the fixed parameters.
    let long_off = u16::from_be_bytes([reply[38], reply[39]]) as usize;
    let short_off = u16::from_be_bytes([reply[40], reply[41]]) as usize;
    assert_eq!(long_off, 36);
    assert_eq!(short_off, 44);
    assert_eq!(&reply[6..10], b"ttro"); // the Finder info
    assert_eq!(&reply[6 + long_off..6 + long_off + 8], b"\x07doc.txt");
}

#[test]
fn test_login_spec_layout() {
    let (mut server, _dir) = test_server("login");
    // [Login]["AFPVersion 2.0"]["No User Authent"] (no reserved
    // byte)
    let mut data = vec![18, 14];
    data.extend_from_slice(b"AFPVersion 2.0");
    data.push(15);
    data.extend_from_slice(b"No User Authent");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
}

#[test]
fn test_login_rejects_bad_version() {
    let (mut server, _dir) = test_server("login-bad");
    let mut data = vec![18, 6];
    data.extend_from_slice(b"AFP3.1");
    data.push(15);
    data.extend_from_slice(b"No User Authent");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, ERR_BAD_VERS);
}

#[test]
fn test_custom_volume_name_and_root() {
    let dir = crate::test_dir("snow-afp-custom");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("x.txt"), b"x").unwrap();
    let mut server = Afp::new_with(
        dir,
        Some("Shared".to_string()),
        MaxVersion::default(),
        AfpSettings::default(),
    );

    // FPGetSrvrParms lists the custom name
    let (result, reply) = result_of(server.command(1, &[16, 0]));
    assert_eq!(result, 0);
    assert_eq!(reply[4], 1);
    assert_eq!(reply[6], 6);
    assert_eq!(&reply[7..13], b"Shared");

    let _ = std::fs::remove_dir_all(crate::test_dir("snow-afp-custom"));
}

#[test]
fn test_both_versions_accepted_and_tracked() {
    let (mut server, _dir) = test_server("versions-both");
    assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
    assert_eq!(login(&mut server, 2, "AFPVersion 2.0"), 0);
    assert_eq!(server.versions[&1], AfpVersion::V11);
    assert_eq!(server.versions[&2], AfpVersion::V20);
}

#[test]
fn test_v11_session_commands_have_no_pad_byte() {
    let (mut server, _dir) = test_server("versions-v11-cmd");
    assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
    // FPGetSrvrParms in 1.1 is the bare function byte
    let (result, reply) = result_of(server.command(1, &[16]));
    assert_eq!(result, 0);
    assert_eq!(reply[4], 1); // one volume (after the server time)
    assert_eq!(reply[6], 4); // volume name length
    assert_eq!(&reply[7..11], b"Snow");
}

#[test]
fn test_v11_open_vol_has_pad_byte() {
    // The exact OpenVol the System 7.1 client sends: the reserved
    // byte is present in 1.1 too
    let (mut server, _dir) = test_server("versions-v11-openvol");
    assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
    let data = [24, 0, 0x00, 0x20, 4, b'S', b'n', b'o', b'w'];
    let (result, reply) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    assert_eq!(reply[0..2], [0x00, 0x20]); // bitmap copy
    assert_eq!(reply[2..4], [0x00, 0x01]); // volume ID
}

#[test]
fn test_bare_get_srvr_parms_in_2x() {
    // The 2.1 client sends FPGetSrvrParms as a bare function byte,
    // like 1.1
    let (mut server, _dir) = test_server("bare-gsp");
    let mut data = vec![18, 14];
    data.extend_from_slice(b"AFPVersion 2.0");
    data.push(15);
    data.extend_from_slice(b"No User Authent");
    let (result, _) = result_of(server.command(1, &data));
    assert_eq!(result, 0);
    // [GetSrvrParms] alone: no pad, no parameters
    let (result, _) = result_of(server.command(1, &[16]));
    assert_eq!(result, 0);
    // [Logout] alone
    let (result, _) = result_of(server.command(1, &[20]));
    assert_eq!(result, 0);
}

#[test]
fn test_short_commands_do_not_panic() {
    let (mut server, _dir) = test_server("short-cmds");
    // Every truncated command must answer an error, never panic
    let mut cases: Vec<Vec<u8>> = vec![
        vec![18],       // login with no parameters
        vec![9],        // enumerate with no parameters
        vec![9, 0],     // enumerate with only the pad
        vec![34],       // GetFileDirParms
        vec![34, 0, 0], // GetFileDirParms with a short volume id
        vec![51],       // GetIcon
        vec![55],       // GetAPPL
        vec![7],        // an unsupported command
    ];
    // Truncated requests: every parser must answer
    // an error, never slice past the checked bytes
    cases.push(vec![34, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // GFDParms: 10 param bytes
    let mut enumerate = vec![9, 0]; // Enumerate: 16 param bytes
    enumerate.extend_from_slice(&[0; 16]);
    cases.push(enumerate);
    for n in 9..=11 {
        let mut open_fork = vec![26, 0]; // OpenFork: 9-11 param bytes
        open_fork.resize(open_fork.len() + n, 0);
        cases.push(open_fork);
    }
    let mut icon = vec![51, 0]; // GetIcon: 11 param bytes
    icon.extend_from_slice(&[0; 11]);
    cases.push(icon);
    for data in &cases {
        let (result, _) = result_of(server.command(1, data));
        assert!(result < 0, "a short command must be an error");
    }
    // SetFileDirParms with an odd-length path and no pad byte
    // after it: the values must exist at the next even offset
    let (mut wserver, wdir) = super::tests::test_server_writable("short-cmds-w");
    std::fs::write(wdir.join("f.txt"), b"x").unwrap();
    let odd = [
        35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x01, 2, 5, b'f', b'.', b't', b'x', b't',
    ];
    let (result, _) = result_of(wserver.command(1, &odd));
    assert!(result < 0);
}

#[test]
fn test_v11_only_rejects_20() {
    let dir = crate::test_dir("snow-afp-v11-only");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Afp::new_with(dir.clone(), None, MaxVersion::Max11, AfpSettings::default());
    assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
    assert_eq!(login(&mut server, 2, "AFP2.0"), ERR_BAD_VERS); // the shorthand is not a version string
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_max20_rejects_21() {
    let dir = crate::test_dir("snow-afp-max20");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Afp::new_with(dir.clone(), None, MaxVersion::Max20, AfpSettings::default());
    assert_eq!(login(&mut server, 1, "AFPVersion 2.0"), 0);
    assert_eq!(login(&mut server, 2, "AFPVersion 1.1"), 0);
    assert_eq!(login(&mut server, 3, "AFPVersion 2.1"), ERR_BAD_VERS);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_max22_accepts_22_and_lists_it_first() {
    let dir = crate::test_dir("snow-afp-max22");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Afp::new_with(dir.clone(), None, MaxVersion::Max22, AfpSettings::default());
    assert_eq!(login(&mut server, 1, "AFP2.2"), 0);
    assert_eq!(login(&mut server, 2, "AFPVersion 2.1"), 0);
    assert_eq!(login(&mut server, 3, "AFPVersion 1.1"), 0);
    let info = server.server_info();
    let versions_off = u16::from_be_bytes([info[2], info[3]]) as usize;
    assert_eq!(info[versions_off], 4); // four version strings
    assert_eq!(&info[versions_off + 1..versions_off + 8], b"\x06AFP2.2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_vol_params_extended_size_bits() {
    // AFP 2.2's bits 0x0200/0x0400 are 64-bit sizes, 0x0800 the
    // 4-byte block size, in bitmap order
    let params = vol_params(0x0E00, "Snow", Path::new("/"), true);
    assert_eq!(&params[0..2], &[0x0E, 0x00]); // the bitmap
    assert_eq!(params.len(), 2 + 8 + 8 + 4);
}

#[test]
fn test_server_info_lists_versions_high_first() {
    let (server, _dir) = test_server("versions-info");
    let info = server.server_info();
    let versions_off = u16::from_be_bytes([info[2], info[3]]) as usize;
    assert_eq!(info[versions_off], 4); // four version strings
    assert_eq!(&info[versions_off + 1..versions_off + 8], b"\x06AFP2.2");
    assert_eq!(
        &info[versions_off + 8..versions_off + 23],
        b"\x0eAFPVersion 2.1"
    );
    assert_eq!(
        &info[versions_off + 23..versions_off + 38],
        b"\x0eAFPVersion 2.0"
    );
    assert_eq!(
        &info[versions_off + 38..versions_off + 53],
        b"\x0eAFPVersion 1.1"
    );
}

#[test]
fn the_uam_count_matches_the_list() {
    let (s, _) = test_server_writable("uams");
    let block = s.server_info();
    let at = u16::from_be_bytes([block[4], block[5]]) as usize;
    assert_eq!(block[at], 1);
    assert_eq!(block[at + 1] as usize, b"No User Authent".len());
    assert_eq!(&block[at + 2..], b"No User Authent");
}

#[test]
fn host_errors_map_to_afp_errors() {
    use std::io::{Error, ErrorKind};
    assert_eq!(io_err(&Error::from(ErrorKind::StorageFull)), ERR_DISK_FULL);
    assert_eq!(
        io_err(&Error::from(ErrorKind::QuotaExceeded)),
        ERR_DISK_FULL
    );
    assert_eq!(
        io_err(&Error::from(ErrorKind::PermissionDenied)),
        ERR_ACCESS_DENIED
    );
    assert_eq!(
        io_err(&Error::from(ErrorKind::ReadOnlyFilesystem)),
        ERR_VOL_LOCKED
    );
    assert_eq!(
        io_err(&Error::from(ErrorKind::NotFound)),
        ERR_OBJECT_NOT_FOUND
    );
    assert_eq!(io_err(&Error::from(ErrorKind::InvalidData)), ERR_PARAM);
}
