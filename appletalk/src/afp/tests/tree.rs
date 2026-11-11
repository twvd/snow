//! Tests of the write commands

use super::*;

#[test]
fn test_write_commands_are_locked_when_read_only() {
    let (mut server, dir) = test_server("write-locked");
    // Create a file on the host so the lookups can succeed
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
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
}
