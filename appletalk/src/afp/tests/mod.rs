//! The AFP server's tests, by topic, and the helpers they share

use super::*;

mod params;

pub(super) fn login(server: &mut Afp, session: u8, version: &str) -> i32 {
    let mut data = vec![18, version.len() as u8];
    data.extend_from_slice(version.as_bytes());
    data.push(15);
    data.extend_from_slice(b"No User Authent");
    let (result, _) = result_of(server.command(session, &data));
    result
}

pub(super) fn result_of(reply: (Vec<u8>, Vec<u8>)) -> (i32, Vec<u8>) {
    (i32::from_be_bytes(reply.0.try_into().unwrap()), reply.1)
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
