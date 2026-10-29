//! File and directory IDs: one ID space for every object on the
//! volume (the AFP 2.1 file IDs, like netatalk's CNIDs)
//!
//! An ID is minted for a file on first contact and keyed by the
//! object's identity — its (device, inode) pair — so the ID survives
//! renames (and even host-side moves the server never sees). The
//! current path is kept alongside for reverse resolution
//! (FPResolveID). The IDs are in-memory: an on-disk store is a
//! possible future refinement (see the state notes).

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
/// hash where the platform has no stable inode
fn identity(path: &Path) -> Option<(u64, u64)> {
    let md = std::fs::metadata(path).ok()?;
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
            self.by_id.insert(id, (path.to_path_buf(), key));
            return Some(id);
        }
        let id = self.allocator.allocate();
        self.by_inode.insert(key, id);
        self.by_id.insert(id, (path.to_path_buf(), key));
        Some(id)
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

    /// Drop a deleted object: the reverse entries of the path and
    /// everything below it AND their identity bindings, so a future
    /// file reusing one of the inodes gets a fresh ID (a stale
    /// reference must fail, never alias the new object)
    pub(crate) fn remove_path(&mut self, path: &Path) {
        let ids: Vec<u32> = self
            .by_id
            .iter()
            .filter(|(_, (p, _))| p.as_path() == path || p.starts_with(path))
            .map(|(&id, _)| id)
            .collect();
        for id in ids {
            if let Some((_, key)) = self.by_id.remove(&id) {
                self.by_inode.remove(&key);
            }
        }
    }

    /// Update the recorded paths after a rename or move: every path
    /// at or below `from` moves under `to`, and a path-derived
    /// identity (a platform without inodes) re-keys with it
    pub(crate) fn move_path(&mut self, from: &Path, to: &Path) {
        for (&id, (p, key)) in &mut self.by_id {
            if let Ok(rest) = p.strip_prefix(from) {
                // An empty remainder is the moved entry itself:
                // join("") would append a trailing separator, which
                // fails to stat for files
                *p = if rest.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(rest)
                };
                // A path-derived identity (a platform without inodes)
                // re-keys with the moved path
                if let Some(new_key) = identity(p)
                    && *key != new_key
                {
                    self.by_inode.remove(key);
                    self.by_inode.insert(new_key, id);
                    *key = new_key;
                }
            }
        }
    }

    /// Re-bind a file whose inode changed (an AppleSingle rewrite
    /// replaces the file): the recorded identity follows the new inode
    /// so the ID stays stable across the rewrite
    pub(crate) fn rebind(&mut self, path: &Path) {
        let Some(id) = self
            .by_id
            .iter()
            .find(|(_, (p, _))| p.as_path() == path)
            .map(|(&id, _)| id)
        else {
            return;
        };
        let Some(key) = identity(path) else {
            return;
        };
        if let Some((_, old_key)) = self.by_id.get_mut(&id) {
            self.by_inode.remove(old_key);
            *old_key = key;
        }
        self.by_inode.insert(key, id);
    }

    /// Invalidate one ID (FPDeleteID). The ID stays reserved: a stale
    /// reference must fail, never alias another file (IDs are not
    /// reused, like CNIDs)
    pub(crate) fn remove_id(&mut self, id: u32) {
        if let Some((_, key)) = self.by_id.remove(&id) {
            self.by_inode.remove(&key);
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
    fn table_rebinds_after_an_inode_change() {
        let dir = std::env::temp_dir().join("snow-ids-rebind");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("doc.txt");
        std::fs::write(&file, b"hello").unwrap();
        let mut table = IdTable::new();
        let id = table.id_of(&file).unwrap();
        // An AppleSingle rewrite replaces the file with a new inode.
        // The replacement is created while the original still exists,
        // so the two identities are guaranteed distinct even on a
        // file system that reuses freed inodes eagerly (ext4 does,
        // which made the old remove-then-recreate form flaky on CI)
        let rewritten = dir.join("rewritten.tmp");
        std::fs::write(&rewritten, b"rewritten").unwrap();
        std::fs::remove_file(&file).unwrap();
        std::fs::rename(&rewritten, &file).unwrap();
        // Until the rebind, the old ID fails verification (a stale
        // reference must not alias the replacement)
        assert_eq!(table.verified_path(id), None);
        table.rebind(&file);
        // After the rebind the same ID serves the new file
        assert_eq!(table.verified_path(id), Some(file.as_path()));
        assert_eq!(table.id_of(&file), Some(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn table_keys_by_identity_and_tracks_paths() {
        let dir = std::env::temp_dir().join("snow-ids-test");
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
        // Rename moves the recorded path, keeps the ID (the identity
        // binding means the same file still resolves to the same ID)
        let renamed = dir.join("renamed.txt");
        std::fs::rename(&file, &renamed).unwrap();
        table.move_path(&file, &renamed);
        assert_eq!(table.verified_path(id), Some(renamed.as_path()));
        assert_eq!(table.id_of(&renamed), Some(id));
        // A subtree move rewrites the paths of everything below the
        // directory too
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let child = sub.join("child.txt");
        std::fs::write(&child, b"c").unwrap();
        let sub_id = table.id_of(&sub).unwrap();
        let child_id = table.id_of(&child).unwrap();
        let moved = dir.join("moved");
        std::fs::rename(&sub, &moved).unwrap();
        table.move_path(&sub, &moved);
        assert_eq!(table.verified_path(sub_id), Some(moved.as_path()));
        assert_eq!(
            table.verified_path(child_id),
            Some(moved.join("child.txt").as_path())
        );
        // The verified path serves an object only while the recorded
        // path still holds the recorded identity
        assert_eq!(
            table.verified_path(child_id),
            Some(moved.join("child.txt").as_path())
        );
        // Delete drops the reverse entry AND the identity binding: a
        // new file (even one reusing the inode) gets a fresh ID
        table.remove_path(&renamed);
        assert_eq!(table.verified_path(id), None);
        let again_id = table.id_of(&renamed).unwrap();
        assert_ne!(again_id, id);
        // remove_id (FPDeleteID) invalidates the ID permanently: the
        // next mint gets a fresh one, never the dead ID
        table.remove_id(sub_id);
        assert_eq!(table.verified_path(sub_id), None);
        let third = dir.join("third.txt");
        std::fs::write(&third, b"x").unwrap();
        let third_id = table.id_of(&third).unwrap();
        assert_ne!(third_id, sub_id);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
