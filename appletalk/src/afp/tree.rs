//! The tree: create, delete, rename, move, file IDs and FPExchangeFiles

use super::*;

impl Afp {
    /// FPCreateFile / FPCreateDir: [vol][dir id][path name]; the path
    /// may name ancestors ("sub\0new"). FPCreateFile's `flag` is the
    /// CreateFlag: bit 7 = hard create (an existing closed file is
    /// deleted and recreated; an open one answers afpFileBusy, a
    /// directory afpObjectTypeErr). FPCreateDir replies with the New
    /// Directory ID.
    pub(super) fn cmd_create(&mut self, is_dir: bool, flag: u8, params: &[u8]) -> Reply {
        if self.settings.read_only {
            return Err(ERR_VOL_LOCKED);
        }
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let dir_id = r.u32()?;
        let name = r.path()?;
        // Everything before the last separator resolves as a path
        // (ancestors), the final component is the new name
        let (dir_part, new_name) = match name.rfind('\0') {
            Some(pos) => (&name[..pos], &name[pos + 1..]),
            None => ("", name.as_str()),
        };
        let base = self
            .resolve_path(dir_id, dir_part)
            .ok_or(ERR_OBJECT_NOT_FOUND)?;
        let host = crate::name::to_host(new_name);
        if !crate::name::is_one_component(&host) {
            return Err(ERR_PARAM);
        }
        if host.starts_with('.') && self.settings.hidden_files == HiddenFiles::Hide {
            // Hidden entries are not addressable in Hide mode
            return Err(ERR_PARAM);
        }
        if crate::name::is_meta_name(&host) {
            // The server's metadata store is never creatable
            return Err(ERR_PARAM);
        }
        let target = base.join(&host);
        // The Mac file system is case-insensitive: a case variant of a
        // name is that name, even where the host would store both side by
        // side
        let existing = if target.exists() {
            Some(target.clone())
        } else {
            self.dir_entry(&base, &host)
        };
        let hard = !is_dir && flag & 0x80 != 0;
        if let Some(existing) = existing.as_ref().filter(|_| hard) {
            // A hard create: replace an existing closed file
            if std::fs::metadata(existing).is_ok_and(|m| m.is_dir()) {
                return Err(ERR_OBJECT_TYPE);
            }
            if self.forks.values().any(|f| f.path == *existing) {
                return Err(ERR_FILE_BUSY);
            }
            if let Err(e) = std::fs::remove_file(existing) {
                return Err(io_err(&e));
            }
            crate::meta::remove_meta(existing);
            self.ids.remove_path(existing);
            if let Some(db) = &mut self.desktop {
                db.remove_appl(existing);
            }
        } else if existing.is_some() {
            return Err(ERR_OBJECT_EXISTS);
        }
        // Metadata under this name belongs to a file or a directory that was
        // deleted or moved away on the host, not to the new one
        crate::meta::remove_meta(&target);
        let created = if is_dir {
            std::fs::create_dir(&target)
        } else {
            // A soft create: an existing name is an error
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map(|_| ())
        };
        match created {
            Ok(()) => {
                self.touch_volume();
                if is_dir {
                    // FPCreateDir replies with the New Directory ID
                    let id = self.ids.id_of(&target).unwrap_or(0);
                    Ok(id.to_be_bytes().to_vec())
                } else {
                    Ok(Vec::new())
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(ERR_OBJECT_EXISTS),
            Err(e) => Err(io_err(&e)),
        }
    }

    /// FPDelete: [vol][dir id][path name]; the metadata containers go
    /// with the file
    pub(super) fn cmd_delete(&mut self, params: &[u8]) -> Reply {
        if self.settings.read_only {
            return Err(ERR_VOL_LOCKED);
        }
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let dir_id = r.u32()?;
        let name = r.path()?;
        let target = self
            .resolve_entry(dir_id, &name)
            .ok_or(ERR_OBJECT_NOT_FOUND)?;
        if target == self.root {
            return Err(ERR_ACCESS_DENIED);
        }
        // A symlink goes alone: the locks and the open forks are its
        // target's, and the target stays
        let link_target = symlink_target(&target);
        if link_target.is_none() {
            // DeleteInhibit (attribute bit 8) answers afpObjectLocked; an
            // open fork answers afpFileBusy (unlinking it would orphan
            // the handle's writes)
            let attrs = self.meta_open(&target).attrs;
            if attrs & 0x0100 != 0 {
                return Err(ERR_OBJECT_LOCKED);
            }
            if self.forks.values().any(|f| f.path == target) {
                return Err(ERR_FILE_BUSY);
            }
        }
        let is_dir = std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir());
        // Emptiness ignores the server's own metadata: a folder full of
        // `._*` orphans (a macOS copy) is still deletable
        if is_dir
            && std::fs::read_dir(&target).is_ok_and(|d| {
                d.flatten()
                    .any(|e| !crate::name::is_container_name(&e.file_name().to_string_lossy()))
            })
        {
            return Err(ERR_DIR_NOT_EMPTY);
        }
        let removed = if is_dir {
            // Only metadata remains: clear it so remove_dir succeeds. The
            // folder's own container is kept in memory and written back
            // if the folder cannot be removed after all (the permissions
            // of its parent, say), so it keeps its Finder information.
            let container = target.join(".AppleDouble").join(".Parent");
            let saved = std::fs::read(&container).ok();
            let _ = std::fs::remove_dir_all(target.join(".AppleDouble"));
            if let Ok(entries) = std::fs::read_dir(&target) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if crate::name::is_container_name(&name) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            let removed = std::fs::remove_dir(&target);
            if removed.is_err()
                && let Some(saved) = saved
            {
                let _ = std::fs::create_dir(target.join(".AppleDouble"));
                let _ = std::fs::write(&container, saved);
            }
            removed
        } else {
            std::fs::remove_file(&target)
        };
        if let Err(e) = removed {
            let code = match e.kind() {
                std::io::ErrorKind::DirectoryNotEmpty => ERR_DIR_NOT_EMPTY,
                _ => io_err(&e),
            };
            return Err(code);
        }
        // Only after the delete succeeded: the folder's own metadata
        // (its `._` sidecar and the parent's `.AppleDouble` entry)
        crate::meta::remove_meta(&target);
        if let Some(real) = link_target {
            // The ID belongs to the target, which is still there: it may
            // have been recorded under the link's path
            let _ = self.ids.id_of(&real);
        } else {
            // The subtree's IDs and their identity bindings go (a reused
            // inode must get a fresh ID, never alias the deleted object)
            self.ids.remove_path(&target);
        }
        if let Some(db) = &mut self.desktop {
            db.remove_appl(&target);
        }
        self.touch_volume();
        Ok(Vec::new())
    }

    /// FPRename: [vol][dir id][old path name][new path name]
    pub(super) fn cmd_rename(&mut self, params: &[u8]) -> Reply {
        self.cmd_rename_inner(params, false)
    }

    /// FPMoveAndRename: [vol][source dir][dest dir][source path]
    /// [dest path][new name]; a null source path addresses a directory
    /// by its own Directory ID, an empty new name keeps the source's
    pub(super) fn cmd_move_and_rename(&mut self, params: &[u8]) -> Reply {
        self.cmd_rename_inner(params, true)
    }

    pub(super) fn cmd_rename_inner(&mut self, params: &[u8], move_rename: bool) -> Reply {
        if self.settings.read_only {
            return Err(ERR_VOL_LOCKED);
        }
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let src_dir_id = r.u32()?;
        // FPMoveAndRename carries the destination Directory ID, the
        // source path, the destination path and the new name
        // ([vol][src dir][dst dir][src path][dst path][new name]); an
        // empty new name keeps the source's. FPRename has the old and
        // the new name right after the source ID.
        let dst_dir_id = if move_rename { Some(r.u32()?) } else { None };
        let old_name = r.path()?;
        let dst_path = if move_rename {
            r.path()?
        } else {
            String::new()
        };
        let new_name = r.path()?;
        let target = self
            .resolve_entry(src_dir_id, &old_name)
            .ok_or(ERR_OBJECT_NOT_FOUND)?;
        if target == self.root {
            return Err(ERR_ACCESS_DENIED);
        }
        // A symlink moves alone: its target's lock is not its own
        let is_link = symlink_target(&target).is_some();
        // RenameInhibit (attribute bit 7) answers afpObjectLocked
        if !is_link && self.meta_open(&target).attrs & 0x0080 != 0 {
            return Err(ERR_OBJECT_LOCKED);
        }
        // The new name: an empty one, or the name the client sees for the
        // source, keeps the source's own host name (the resolved target's
        // file name — the wire form would lose a mangled name, and a null
        // source path, which addresses a directory by its own Directory
        // ID, would make the destination the directory itself)
        let new_name = if new_name.is_empty() || new_name == self.mac_name_of(&target) {
            target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            crate::name::to_host(&new_name)
        };
        let dst_base = if let Some(dst_dir_id) = dst_dir_id {
            let dst_base = self
                .resolve_path(dst_dir_id, &dst_path)
                .ok_or(ERR_OBJECT_NOT_FOUND)?;
            if !dst_base.is_dir() {
                return Err(ERR_OBJECT_TYPE);
            }
            dst_base
        } else {
            target.parent().unwrap_or(&self.root).to_path_buf()
        };
        if !crate::name::is_one_component(&new_name) {
            return Err(ERR_PARAM);
        }
        if new_name.starts_with('.') && self.settings.hidden_files == HiddenFiles::Hide {
            // Hidden entries are not addressable in Hide mode
            return Err(ERR_PARAM);
        }
        if crate::name::is_meta_name(&new_name) {
            // The server's metadata store is never renamable
            return Err(ERR_PARAM);
        }
        let new_target = dst_base.join(&new_name);
        // The Mac file system is case-insensitive: renaming onto an existing
        // name, even a case variant, is ObjectExists, unless it is a case-only
        // rename of the source itself. That takes the destination existing as
        // the same object (by identity, never by path: a case-sensitive host
        // holds both spellings as two files, and a plain rename would replace
        // the other one), in the source's folder, with a name that matches
        // ignoring case. A hard link under another name is ObjectExists: a
        // POSIX rename would leave it untouched while the bookkeeping below
        // would still move.
        let exact_dest = new_target.symlink_metadata().is_ok();
        let same_object = exact_dest
            && same_file(&new_target, &target)
            && target.parent().is_some_and(|p| p == dst_base)
            && target.file_name().is_some_and(|n| {
                n.to_string_lossy().nfc().collect::<String>().to_lowercase()
                    == new_name.nfc().collect::<String>().to_lowercase()
            });
        if exact_dest && !same_object {
            return Err(ERR_OBJECT_EXISTS);
        }
        if !exact_dest
            && self
                .case_variant_other(&dst_base, &new_name, &target)
                .is_some()
        {
            return Err(ERR_OBJECT_EXISTS);
        }
        // Moving a directory into itself or one of its children is
        // afpCantMove
        if !is_link && target.is_dir() && dst_base.starts_with(&target) {
            return Err(ERR_CANT_MOVE);
        }
        let rename_result = if same_object {
            // Only a case-insensitive host makes this possible (the
            // destination resolves to the source): its no-replace
            // rename would refuse it, so a plain rename
            std::fs::rename(&target, &new_target)
        } else {
            rename_no_replace(&target, &new_target)
        };
        match rename_result {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ERR_OBJECT_EXISTS);
            }
            Err(e) => return Err(io_err(&e)),
        }
        let _ = crate::meta::move_meta(&target, &new_target);
        // The subtree's recorded paths follow the move (the identity
        // bindings are unchanged, so the IDs stay usable)
        self.ids.move_path(&target, &new_target);
        for fork in self.forks.values_mut() {
            if fork.path == target {
                // The renamed file's forks carry the new wire name too
                fork.name = crate::name::to_mac(new_name.as_str()).0;
                fork.path = new_target.clone();
            } else if let Ok(rest) = fork.path.strip_prefix(&target) {
                // Forks of files inside a moved directory follow it
                fork.path = new_target.join(rest);
            }
        }
        if let Some(db) = &mut self.desktop {
            db.remove_appl(&target);
        }
        self.touch_volume();
        Ok(Vec::new())
    }

    /// FPCreateID: [vol][dir id][path name] → the file ID (the
    /// identity table's ID, minted on first contact)
    pub(super) fn cmd_create_id(&mut self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let dir_id = r.u32()?;
        let name = r.path()?;
        let target = self
            .resolve_path(dir_id, &name)
            .ok_or(ERR_OBJECT_NOT_FOUND)?;
        if std::fs::metadata(&target).is_ok_and(|m| m.is_dir()) {
            return Err(ERR_OBJECT_TYPE);
        }
        // The identity table mints the ID on first contact and keeps
        // it stable thereafter
        let id = self.ids.id_of(&target).ok_or(ERR_OBJECT_NOT_FOUND)?;
        Ok(id.to_be_bytes().to_vec())
    }

    /// FPDeleteID: [vol][file id] — invalidate a file ID
    pub(super) fn cmd_delete_id(&mut self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let id = r.u32()?;
        // Only a file's ID can be invalidated: a directory's ID is the
        // server's own naming, an unknown or replaced ID answers
        // afpIDNotFound
        let path = self
            .ids
            .verified_path(id)
            .map(|p| p.to_path_buf())
            .ok_or(ERR_ID_NOT_FOUND)?;
        match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => return Err(ERR_OBJECT_TYPE),
            Ok(_) => {}
            Err(_) => return Err(ERR_ID_NOT_FOUND),
        }
        self.ids.remove_id(id);
        Ok(Vec::new())
    }

    /// FPResolveID: [vol][file id][file bitmap] → the file parameters
    pub(super) fn cmd_resolve_id(&mut self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let id = r.u32()?;
        let bitmap = r.u16()?;
        if bitmap & !FILE_BITMAP_ALLOWED != 0 {
            return Err(ERR_BITMAP);
        }
        // The recorded path must still hold the recorded file: a host
        // save-by-rename replaces the inode, and the old ID must fail,
        // never alias the replacement. The AFP 2.1 specification
        // (afpResolveID): IDNotFound when the "File ID was not found",
        // BadIDErr when the number "is not a defined FileID". An ID whose
        // file has gone is still defined; one never handed out, or removed
        // by FPDeleteID, is not.
        let Some(target) = self.ids.verified_path(id).map(|p| p.to_path_buf()) else {
            let code = if self.ids.is_defined(id) {
                ERR_ID_NOT_FOUND
            } else {
                ERR_BAD_ID
            };
            return Err(code);
        };
        // A directory's ID is not a file ID
        if std::fs::metadata(&target).is_ok_and(|m| m.is_dir()) {
            return Err(ERR_OBJECT_TYPE);
        }
        let mut meta = self.meta_open(&target);
        meta.attrs |= self.open_attrs(&target);
        let name = self.mac_name_of(&target);
        let parent_id = self.parent_id_of(&target);
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap.to_be_bytes());
        // The resolved ID itself is the file number (the CNID)
        reply.extend_from_slice(&file_params(bitmap, &name, &meta, parent_id, id));
        Ok(reply)
    }

    /// FPExchangeFiles: [vol][source dir][dest dir][source path][dest path]
    ///
    /// A safe save: the application writes a temporary file, then exchanges
    /// it with the original, whose ID its aliases hold. The data, the
    /// resource fork, the modification date, the fork reference numbers
    /// and the locks change hands; the name, the parent, the file ID and
    /// the rest of the metadata stay with the file.
    pub(super) fn cmd_exchange_files(&mut self, params: &[u8]) -> Reply {
        if self.settings.read_only {
            return Err(ERR_VOL_LOCKED);
        }
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let src_dir = r.u32()?;
        let dst_dir = r.u32()?;
        let src_name = r.path()?;
        let dst_name = r.path()?;
        let (Some(a), Some(b)) = (
            self.resolve_path(src_dir, &src_name),
            self.resolve_path(dst_dir, &dst_name),
        ) else {
            return Err(ERR_OBJECT_NOT_FOUND);
        };
        if a == b {
            return Err(ERR_SAME_OBJECT);
        }
        let (meta_a, meta_b) = (self.meta_open(&a), self.meta_open(&b));
        for path in [&a, &b] {
            if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
                return Err(ERR_OBJECT_TYPE);
            }
        }
        // In the book's model the two catalog entries swap names, so a
        // rename inhibit refuses the exchange as a write inhibit does,
        // although this server swaps the contents instead. The write
        // inhibit also covers an AppleSingle file, whose data cannot be
        // separated from its container.
        if (meta_a.attrs | meta_b.attrs) & (ATTR_WRITE_INHIBIT | 0x0080) != 0 {
            return Err(ERR_OBJECT_LOCKED);
        }
        let (Some(id_a), Some(id_b)) = (self.ids.id_of(&a), self.ids.id_of(&b)) else {
            return Err(ERR_OBJECT_NOT_FOUND);
        };
        // Inside Macintosh: Files, p. 1-26: "Fields that describe the
        // data remain with the data; fields that describe the file remain
        // with the file". The data forks, the resource forks and the
        // modification dates change hands. The Finder information, the
        // attributes, the creation and backup dates and the file IDs stay
        // with the names, and so do the metadata containers.
        let (rsrc_a, rsrc_b) = match (
            crate::meta::read_rsrc(&meta_a),
            crate::meta::read_rsrc(&meta_b),
        ) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => return Err(io_err(&e)),
        };
        if let Err(e) = crate::meta::exchange_paths(&a, &b) {
            return Err(io_err(&e));
        }
        let swap = |rsrc_a: &[u8], mod_a: u32, rsrc_b: &[u8], mod_b: u32| {
            crate::meta::set_rsrc(&a, rsrc_a)?;
            crate::meta::set_rsrc(&b, rsrc_b)?;
            crate::meta::set_dates(&a, meta_a.create_date, mod_a, meta_a.backup_date)?;
            crate::meta::set_dates(&b, meta_b.create_date, mod_b, meta_b.backup_date)
        };
        if let Err(e) = swap(&rsrc_b, meta_b.mod_date, &rsrc_a, meta_a.mod_date) {
            // Each name would be left with half of the other file: put
            // everything back
            let _ = crate::meta::exchange_paths(&a, &b);
            let _ = swap(&rsrc_a, meta_a.mod_date, &rsrc_b, meta_b.mod_date);
            return Err(io_err(&e));
        }
        self.ids.exchange((id_a, &a), (id_b, &b));
        // An open fork stays with its data, under its new name
        let (name_a, name_b) = (self.mac_name_of(&a), self.mac_name_of(&b));
        let (parent_a, parent_b) = (self.parent_id_of(&a), self.parent_id_of(&b));
        for fork in self.forks.values_mut() {
            if fork.path == a {
                fork.path = b.clone();
                fork.name = name_b.clone();
                fork.parent = parent_b;
                fork.file_id = id_b;
            } else if fork.path == b {
                fork.path = a.clone();
                fork.name = name_a.clone();
                fork.parent = parent_a;
                fork.file_id = id_a;
            }
        }
        self.refresh_forks(&a);
        self.refresh_forks(&b);
        if let Some(db) = &mut self.desktop {
            db.remove_appl(&a);
            db.remove_appl(&b);
        }
        self.touch_volume();
        Ok(Vec::new())
    }
}

/// Rename a host file without replacing an existing destination (the
/// AFP semantics: an existing name answers afpObjectExists).
/// `std::fs::rename` silently replaces on Linux, which would be data
/// loss; the check-then-rename fallback is the non-atomic case.
#[cfg(target_os = "linux")]
pub(super) fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes()).unwrap_or_default();
    let to = CString::new(to.as_os_str().as_bytes()).unwrap_or_default();
    // SAFETY: valid NUL-terminated paths and constants
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
pub(super) fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes()).unwrap_or_default();
    let to = CString::new(to.as_os_str().as_bytes()).unwrap_or_default();
    // SAFETY: valid NUL-terminated paths and constants
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
    }
    std::fs::rename(from, to)
}
