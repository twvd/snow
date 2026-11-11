//! Path names: the AFP pathname grammar, the share jail, and the IDs and names of what they reach

use super::*;

impl Afp {
    /// Resolve a wire path against a base directory. The AFP pathname
    /// grammar (ch13): the path is relative to the base; a single NUL
    /// separates names, and each run of K NULs ascends K-1 levels,
    /// stopping at the volume root; `.`
    /// and `..` as names stay refused. Every component maps through
    /// the `/` → `:` bijection, a mangled name finds its entry by
    /// mangling the directory's names, and — like the case-insensitive
    /// Mac file system — a case variant of a host name resolves to the
    /// real entry.
    pub(super) fn resolve_from(&self, base: &Path, path: &str) -> Option<PathBuf> {
        let (walked, _) = self.walk_path(base, path)?;
        self.jailed(&walked)
    }

    /// Like `resolve_from`, for the commands that act on the directory
    /// entry itself: a symlink as the last element stays the link. A
    /// delete or rename of a link must not reach its target.
    pub(super) fn resolve_entry_from(&self, base: &Path, path: &str) -> Option<PathBuf> {
        let (walked, named) = self.walk_path(base, path)?;
        if !named {
            return self.jailed(&walked);
        }
        let entry = self.jailed(walked.parent()?)?.join(walked.file_name()?);
        entry.symlink_metadata().ok()?;
        Some(entry)
    }

    /// Walk the pathname grammar from a base, without following anything:
    /// the path reached, and whether its last step was a name
    pub(super) fn walk_path(&self, base: &Path, path: &str) -> Option<(PathBuf, bool)> {
        let mut base = base.to_path_buf();
        let mut named = false;
        // Mac names cannot contain the separator, so every NUL is
        // grammar: each run of K NULs ascends K-1 levels, wherever it
        // is (ch13's examples: "e[0][0]g" goes through e's parent,
        // "e[0][0][0]" ends two levels above e). The Finder's New
        // Folder asks about the new folder's parent with "[0][0]".
        let mut rest = path;
        while !rest.is_empty() {
            let nuls = rest.len() - rest.trim_start_matches('\0').len();
            for _ in 1..nuls {
                // Above the root is ObjectNotFound
                if base == self.root {
                    return None;
                }
                base.pop();
                named = false;
            }
            rest = &rest[nuls..];
            let end = rest.find('\0').unwrap_or(rest.len());
            let component = &rest[..end];
            rest = &rest[end..];
            if component.is_empty() {
                continue;
            }
            let host = crate::name::to_host(component);
            if !crate::name::is_one_component(&host) {
                // Parent traversal is never addressable on the wire, and a
                // name cannot reach anywhere else either
                return None;
            }
            if host.starts_with('.') && self.settings.hidden_files == HiddenFiles::Hide {
                // Hidden entries are not addressable unless the server
                // shows them
                return None;
            }
            if crate::name::is_meta_name(&host) {
                // The server's metadata store is never addressable
                return None;
            }
            let exact = base.join(&host);
            base = if exact.symlink_metadata().is_ok() {
                exact
            } else {
                // The Mac file system is case-insensitive while the
                // host may not be: a case variant of the name resolves
                // to the real entry (refusing to resolve into the
                // server's own metadata store under a case variant)
                match self.dir_entry(&base, &host) {
                    Some(found) => {
                        let fname = found
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default();
                        if crate::name::is_meta_name(fname)
                            || (fname.starts_with('.')
                                && self.settings.hidden_files == HiddenFiles::Hide)
                        {
                            return None;
                        }
                        found
                    }
                    None => exact,
                }
            };
            named = true;
        }
        Some((base, named))
    }

    /// The canonical form of a path, if it stays inside the share
    pub(super) fn jailed(&self, path: &Path) -> Option<PathBuf> {
        let canonical = path.canonicalize().ok()?;
        let inside = canonical.strip_prefix(&self.root).ok()?;
        // Hardened: no symlink may lead into a hidden entry
        if self.settings.hidden_files == HiddenFiles::Hide
            && inside
                .components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            return None;
        }
        Some(canonical)
    }

    /// Resolve a wire path against a Directory ID: `dir_base` then the
    /// pathname grammar, with the reserved parent ID 1 whose only
    /// child is the volume itself, addressed by its name as the first
    /// element
    pub(super) fn resolve_path(&self, dir_id: u32, path: &str) -> Option<PathBuf> {
        let (base, rest) = self.path_start(dir_id, path)?;
        self.resolve_from(&base, rest)
    }

    /// `resolve_path` for the commands that act on the directory entry
    /// itself (see `resolve_entry_from`)
    pub(super) fn resolve_entry(&self, dir_id: u32, path: &str) -> Option<PathBuf> {
        let (base, rest) = self.path_start(dir_id, path)?;
        self.resolve_entry_from(&base, rest)
    }

    /// Where a path starts: the Directory ID's directory, and the rest of
    /// the path after the volume name for the reserved parent ID
    pub(super) fn path_start<'a>(&self, dir_id: u32, path: &'a str) -> Option<(PathBuf, &'a str)> {
        if dir_id == ROOT_PARENT_DIR_ID {
            // [volume name]\0... — anything else is ObjectNotFound
            let path = path.strip_prefix('\0').unwrap_or(path);
            let rest = path.strip_prefix(self.volume_name.as_str())?;
            let rest = if rest.is_empty() {
                rest
            } else {
                rest.strip_prefix('\0')?
            };
            return Some((self.root.clone(), rest));
        }
        Some((self.dir_base(dir_id)?, path))
    }

    /// The fixed Directory ID of a host directory (the root itself is
    /// the reserved ID 2): the identity table's ID, minted on first
    /// contact; 0 marks a vanished directory
    pub(super) fn dir_id(&mut self, abs: &Path) -> u32 {
        if abs == self.root {
            return ROOT_DIR_ID;
        }
        // The ID comes from the volume's identity table (minted on
        // first contact); 0 marks a vanished directory
        self.ids.id_of(abs).unwrap_or(0)
    }

    /// The entry of `dir` whose name matches `host` case-insensitively
    /// (Unicode lower-case folding, as the Mac file system folds), or
    /// whose mangled name is `host`; None when no entry matches. Called
    /// only when the exact name misses, so the read_dir cost is paid on
    /// misses alone.
    pub(super) fn dir_entry(&self, dir: &Path, host: &str) -> Option<PathBuf> {
        // NFC first: the server serves composed names, and a host may
        // store decomposed ones (macOS does); a byte-exact file system
        // then needs the normalization before the case fold
        let wanted: String = host.nfc().collect::<String>().to_lowercase();
        // Every mangled name holds the hash's `#`, so other misses skip
        // the mangling
        let mangled = wanted
            .contains('#')
            .then(|| crate::name::from_host(&wanted));
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let name = entry.file_name();
            let lossy = name.to_string_lossy();
            if lossy.nfc().collect::<String>().to_lowercase() == wanted
                || mangled.as_ref().is_some_and(|m| {
                    let (mac, original) = crate::name::to_mac(&lossy);
                    original.is_some() && mac.to_lowercase() == *m
                })
            {
                return Some(dir.join(name));
            }
        }
        None
    }

    /// The host directory a Directory ID refers to (the root is the
    /// fixed ID 2), only while the recorded path still holds the
    /// recorded directory
    pub(super) fn dir_base(&self, dir_id: u32) -> Option<PathBuf> {
        if dir_id == ROOT_DIR_ID {
            return Some(self.root.clone());
        }
        self.ids.verified_path(dir_id).map(|p| p.to_path_buf())
    }

    /// The parent Directory ID of a host path: the root's parent is
    /// the reserved ID 1 (the HFS convention, where the parent walk
    /// terminates), a directory's parent is the ID of its parent
    /// directory otherwise
    pub(super) fn parent_id_of(&mut self, abs: &Path) -> u32 {
        if abs == self.root {
            return ROOT_PARENT_DIR_ID;
        }
        match abs.parent() {
            Some(parent) if parent != abs => self.dir_id(parent),
            _ => ROOT_DIR_ID,
        }
    }

    /// The wire name of a resolved host path, mangled when it has to be
    pub(super) fn mac_name_of(&self, target: &Path) -> String {
        let host_name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        crate::name::to_mac(&host_name).0
    }
}

/// Whether a directory entry is listed: a file, a directory, or a symbolic
/// link to one. A link whose target is missing has no parameters to report,
/// and a FIFO, a socket or a device is nothing a Mac can open.
///
/// FPEnumerate drops the other entries before its Start Index counts the
/// list. Skipping one while building the reply would shift every entry
/// after it by one for the client, which then receives one of them twice.
pub(super) fn listable(entry: &std::fs::DirEntry) -> bool {
    let Ok(kind) = entry.file_type() else {
        return false;
    };
    if kind.is_symlink() {
        return entry
            .path()
            .metadata()
            .is_ok_and(|m| m.is_file() || m.is_dir());
    }
    kind.is_file() || kind.is_dir()
}

/// The number of offspring, honoring the server's hidden-file mode
pub(super) fn visible_offspring(dir: &Path, mode: HiddenFiles) -> u16 {
    std::fs::read_dir(dir).map_or(0, |d| {
        d.flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                (mode != HiddenFiles::Hide || !name.starts_with('.'))
                    && !crate::name::is_meta_name(&name)
                    && listable(e)
            })
            .count()
            .min(65535)
    }) as u16
}
