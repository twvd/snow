//! File and directory IDs: one ID space for every object on the
//! volume (the AFP 2.1 file IDs, like netatalk's CNIDs)
//!
//! An ID is minted for a file on first contact and keyed by the
//! object's identity — its (device, inode) pair — so the ID survives
//! renames (and even host-side moves the server never sees). The
//! current path is kept alongside for reverse resolution
//! (FPResolveID). The IDs are in-memory, for the life of the server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The reserved Directory ID of the volume root
pub(crate) const ROOT_DIR_ID: u32 = 2;

/// The ID allocator: a plain counter over the u32 space. IDs are
/// never reused (like CNIDs), so a counter suffices — nothing is ever
/// freed, and the reserved IDs (0 invalid, 1 parent-of-root, 2 root)
/// are simply never handed out. A future scan-rebuild persistence
/// would start the counter above the largest scanned ID.
struct IdAllocator {
    next: u32,
}

impl IdAllocator {
    fn new() -> Self {
        Self { next: 3 }
    }

    fn allocate(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1).max(3);
        id
    }
}

/// The identity of a file: its (device, inode) pair on Unix, a path
/// hash where the platform has no stable inode.
///
/// A symbolic link is its own object, with the link's inode: the Mac sees
/// two files, so they need two IDs, and FPResolveID must answer the name
/// it was given. Hard links share an inode and so share an ID, and the
/// path recorded for it is the name touched last; any of them is the
/// same file.
fn identity(path: &Path) -> Option<(u64, u64)> {
    let md = std::fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((md.dev(), md.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.to_string_lossy().hash(&mut hasher);
        Some((0, hasher.finish()))
    }
}

/// The volume's ID table: identity → ID and ID → (current path, the
/// identity it was minted for)
pub(crate) struct IdTable {
    by_inode: HashMap<(u64, u64), u32>,
    by_id: HashMap<u32, (PathBuf, (u64, u64))>,
    allocator: IdAllocator,
}

impl IdTable {
    pub(crate) fn new() -> Self {
        Self {
            by_inode: HashMap::new(),
            by_id: HashMap::new(),
            allocator: IdAllocator::new(),
        }
    }

    /// The ID of a file, minting one on first contact. None when the
    /// file cannot be stat'ed. On a hit the recorded path is
    /// refreshed, which self-heals host-side moves the next time the
    /// client touches the object.
    pub(crate) fn id_of(&mut self, path: &Path) -> Option<u32> {
        let key = identity(path)?;
        if let Some(&id) = self.by_inode.get(&key) {
            self.record(id, path.to_path_buf(), key);
            return Some(id);
        }
        let id = self.allocator.allocate();
        self.by_inode.insert(key, id);
        self.record(id, path.to_path_buf(), key);
        Some(id)
    }

    /// Record an ID's current path and identity
    fn record(&mut self, id: u32, path: PathBuf, key: (u64, u64)) {
        self.by_id.insert(id, (path, key));
    }

    /// The host path an ID refers to, only while the recorded path
    /// still holds the recorded object: a path reused by a different
    /// file or directory must not be served under the old ID
    pub(crate) fn verified_path(&self, id: u32) -> Option<&Path> {
        let (path, key) = self.by_id.get(&id)?;
        if identity(path).as_ref() == Some(key) {
            Some(path.as_path())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_mints_unique_ids_and_never_reuses() {
        let mut alloc = IdAllocator::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..300 {
            let id = alloc.allocate();
            assert!(seen.insert(id), "duplicate id {id}");
        }
        // The reserved IDs never come out
        assert!(!seen.contains(&0));
        assert!(!seen.contains(&1));
        assert!(!seen.contains(&2));
        // And the minted IDs are dense, as a counter produces
        assert_eq!(seen.len(), 300);
        assert!(seen.contains(&3));
    }

    #[test]
    fn table_keys_by_identity_and_tracks_paths() {
        let dir = crate::test_dir("snow-ids-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("doc.txt");
        std::fs::write(&file, b"hello").unwrap();

        let mut table = IdTable::new();
        let id = table.id_of(&file).unwrap();
        // Stable on repeat
        assert_eq!(table.id_of(&file), Some(id));
        assert_eq!(table.verified_path(id), Some(file.as_path()));
        // A different file gets a different ID
        let other = dir.join("other.txt");
        std::fs::write(&other, b"hi").unwrap();
        let other_id = table.id_of(&other).unwrap();
        assert_ne!(id, other_id);
        // The root's reserved ID is never minted
        assert_ne!(id, ROOT_DIR_ID);
        assert_ne!(other_id, ROOT_DIR_ID);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_has_its_own_id() {
        let dir = crate::test_dir("snow-ids-link");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real");
        let link = dir.join("link");
        std::fs::write(&real, b"x").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut table = IdTable::new();
        let (real_id, link_id) = (table.id_of(&real).unwrap(), table.id_of(&link).unwrap());
        assert_ne!(real_id, link_id);
        assert_eq!(table.verified_path(real_id), Some(real.as_path()));
        assert_eq!(table.verified_path(link_id), Some(link.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
