//! Parameters: the volume, file and directory parameters and Enumerate

use super::*;

impl Afp {
    /// FPOpenVol: make the volume available, returning the volume ID
    pub(super) fn cmd_open_vol(&self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let bitmap = r.u16()?;
        let name = r.pstring()?;
        if name != self.volume_name {
            wire_log!("AFP: open of unknown volume '{}'", name);
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        if !vol_bitmap_ok(bitmap) {
            return Err(ERR_BITMAP);
        }
        Ok(vol_params(
            bitmap,
            &self.volume_name,
            &self.root,
            self.settings.read_only,
        ))
    }

    /// FPGetVolParms: the volume parameters
    pub(super) fn cmd_get_vol_parms(&self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let bitmap = r.u16()?;
        if !vol_bitmap_ok(bitmap) {
            return Err(ERR_BITMAP);
        }
        Ok(vol_params(
            bitmap,
            &self.volume_name,
            &self.root,
            self.settings.read_only,
        ))
    }

    /// The attribute bits of a file's open state: DAlreadyOpen when
    /// a data fork is open, RAlreadyOpen when the resource fork is
    pub(super) fn open_attrs(&self, path: &Path) -> u16 {
        let mut attrs = 0;
        for fork in self.forks.values() {
            if fork.path == path {
                if fork.rsrc {
                    attrs |= 0x0010;
                } else {
                    attrs |= 0x0008;
                }
            }
        }
        attrs
    }

    /// In "show as invisible" mode a dot-prefixed entry carries the
    /// attribute and the Finder's invisible flag, which is what the Finder
    /// looks at
    pub(super) fn mark_hidden(&self, entry: &Path, meta: &mut Meta) {
        if self.settings.hidden_files == HiddenFiles::Invisible
            && entry
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        {
            meta.attrs |= 0x0001;
            meta.finder_info[8] |= 0x40;
        }
    }

    /// The metadata of a host file, honoring the server's probe
    /// settings
    pub(super) fn meta_open(&self, path: &Path) -> Meta {
        // A writable volume always reads back what it writes
        crate::meta::open_with(
            path,
            self.settings.probe_apple_double || !self.settings.read_only,
        )
    }

    /// FPGetFileDirParms: parameters of a file or directory
    pub(super) fn cmd_get_file_dir_parms(&mut self, version: AfpVersion, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let dir_id = r.u32()?;
        let file_bitmap = r.u16()?;
        let dir_bitmap = r.u16()?;
        let path = r.path()?;
        if file_bitmap & !FILE_BITMAP_ALLOWED != 0 || dir_bitmap & !DIR_BITMAP_ALLOWED != 0 {
            return Err(ERR_BITMAP);
        }
        wire_log!(
            "AFP: GetFileDirParms dir {} path {:?} file bm {:04x} dir bm {:04x}",
            dir_id,
            path,
            file_bitmap,
            dir_bitmap
        );

        // Resolve relative to the given directory
        let target = self
            .resolve_path(dir_id, &path)
            .ok_or(ERR_OBJECT_NOT_FOUND)?;
        let Ok(fs_meta) = std::fs::metadata(&target) else {
            return Err(ERR_OBJECT_NOT_FOUND);
        };
        let mut meta = self.meta_open(&target);
        meta.attrs = v11_attrs(version, meta.attrs | self.open_attrs(&target));
        let name = if target == self.root {
            self.volume_name.clone()
        } else {
            // A symlink's parameters are its target's, under its own name
            let entry = self.resolve_entry(dir_id, &path);
            let entry = entry.as_deref().unwrap_or(&target);
            self.mark_hidden(entry, &mut meta);
            self.mac_name_of(entry)
        };
        let parent_id = self.parent_id_of(&target);

        let mut reply = Vec::new();
        reply.extend_from_slice(&file_bitmap.to_be_bytes());
        reply.extend_from_slice(&dir_bitmap.to_be_bytes());
        if fs_meta.is_dir() {
            let offspring = visible_offspring(&target, self.settings.hidden_files);
            reply.push(DIR_FLAG);
            // The reserved byte after the File/DirFlag exists in both
            // AFP 1.1 and AFP 2.0
            reply.push(0);
            reply.extend_from_slice(&dir_params(
                dir_bitmap,
                &name,
                &meta,
                offspring,
                self.dir_id(&target),
                parent_id,
                self.settings.read_only,
            ));
        } else {
            reply.push(0);
            reply.push(0);
            let file_id = self.ids.id_of(&target).unwrap_or(0);
            reply.extend_from_slice(&file_params(file_bitmap, &name, &meta, parent_id, file_id));
        }
        Ok(reply)
    }

    /// FPEnumerate: list the offspring of a directory
    pub(super) fn cmd_enumerate(
        &mut self,
        version: AfpVersion,
        params: &[u8],
        file_flag: Option<u8>,
    ) -> Reply {
        let mut r = Reader::new(params);
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let dir_id = r.u32()?;
        let file_bitmap = r.u16()?;
        let dir_bitmap = r.u16()?;
        let req_count = r.u16()?;
        let start_index = r.u16()?;
        let max_reply_size = r.u16()?;
        let path = r.path()?;
        if file_bitmap & !FILE_BITMAP_ALLOWED != 0 || dir_bitmap & !DIR_BITMAP_ALLOWED != 0 {
            return Err(ERR_BITMAP);
        }
        // Both bitmaps null is a bitmap error; a null File Bitmap
        // enumerates only directories, a null Directory Bitmap only
        // files
        if file_bitmap == 0 && dir_bitmap == 0 {
            return Err(ERR_BITMAP);
        }

        let dir = self.resolve_path(dir_id, &path).ok_or(ERR_DIR_NOT_FOUND)?;
        // Enumerating a file answers afpObjectTypeErr
        if !std::fs::metadata(&dir).is_ok_and(|m| m.is_dir()) {
            return Err(ERR_OBJECT_TYPE);
        }
        // The parent Directory ID the offspring report
        let parent_id = self.dir_id(&dir);

        let mut reply = Vec::new();
        reply.extend_from_slice(&file_bitmap.to_be_bytes());
        reply.extend_from_slice(&dir_bitmap.to_be_bytes());
        reply.extend_from_slice(&0u16.to_be_bytes()); // actual count, filled in

        // An empty request enumerates nothing
        if req_count == 0 {
            return Ok(reply);
        }

        // The null bitmaps and the 1.1 FileFlag filter the record set;
        // the Start Index counts the filtered list
        let files_included = file_flag.is_none_or(|f| f & 0x80 == 0);
        let filter = if file_bitmap == 0 || !files_included {
            Offspring::Folders
        } else if dir_bitmap == 0 {
            Offspring::Files
        } else {
            Offspring::All
        };
        // The client pages through a folder from Start Index 1 on. The
        // listing of the first page serves the following ones while the
        // folder does not change; otherwise a folder of N entries would be
        // read, converted and sorted N / count times.
        let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
        let cached = self.enum_cache.take().filter(|c| {
            start_index > 1 && c.dir == dir && c.filter == filter && Some(c.mtime) == mtime
        });
        let offspring = match cached {
            Some(cache) => cache.offspring,
            None => self.list_offspring(&dir, filter)?,
        };
        // The enumeration index space: the book's ObjectNotFound means
        // "no more offspring exist to be enumerated" — the client pages
        // until it gets that error, so a Start Index past the last
        // visible offspring must end the enumeration
        let skip = usize::from(start_index.saturating_sub(1));
        if skip >= offspring.len() {
            return Err(ERR_OBJECT_NOT_FOUND);
        }

        let mut count = 0u16;
        for (target, mac_name) in offspring.iter().skip(skip) {
            let Ok(fs_meta) = std::fs::metadata(target) else {
                continue;
            };
            // A symlink's metadata is its target's, as GetFileDirParms
            // reports it
            let linked = target
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
                .then(|| self.jailed(target))
                .flatten();
            let entry = target;
            let target = linked.as_ref().unwrap_or(target);
            let mut meta = self.meta_open(target);
            meta.attrs = v11_attrs(version, meta.attrs | self.open_attrs(target));
            self.mark_hidden(entry, &mut meta);
            let (flag, params) = if fs_meta.is_dir() {
                let offspring_count = visible_offspring(target, self.settings.hidden_files);
                (
                    DIR_FLAG,
                    dir_params(
                        dir_bitmap,
                        mac_name,
                        &meta,
                        offspring_count,
                        self.dir_id(target),
                        parent_id,
                        self.settings.read_only,
                    ),
                )
            } else {
                let file_id = self.ids.id_of(target).unwrap_or(0);
                (
                    0,
                    file_params(file_bitmap, mac_name, &meta, parent_id, file_id),
                )
            };
            let mut record = Vec::new();
            record.push(0); // struct length, filled in
            record.push(flag);
            record.extend_from_slice(&params);
            if record.len() % 2 != 0 {
                record.push(0);
            }
            record[0] = record.len() as u8;
            // Honor the client's MaxReplySize: no partial offspring
            // structures are returned. ch13: ParamErr when "MaxReplySize
            // is too small to hold a single offspring structure", rather
            // than noErr with nothing, which gives the client no way on.
            if reply.len() + record.len() > max_reply_size as usize {
                if count == 0 {
                    return Err(ERR_PARAM);
                }
                break;
            }
            reply.extend_from_slice(&record);
            count += 1;
            if count >= req_count {
                break;
            }
        }
        reply[4..6].copy_from_slice(&count.to_be_bytes());
        if let Some(mtime) = mtime {
            self.enum_cache = Some(EnumCache {
                dir,
                mtime,
                filter,
                offspring,
            });
        }
        Ok(reply)
    }

    /// The visible offspring of a folder with their wire names (dotfiles,
    /// .AppleDouble entries and ._ sidecars are hidden), sorted by wire
    /// name for a stable enumeration order.
    fn list_offspring(&self, dir: &Path, filter: Offspring) -> Result<Vec<(PathBuf, String)>, i32> {
        let entries = std::fs::read_dir(dir).map_err(|_| ERR_DIR_NOT_FOUND)?;
        let mut offspring: Vec<(PathBuf, String)> = entries
            .flatten()
            .filter(listable)
            .map(|e| e.path())
            .filter_map(|p| {
                let host_name = p.file_name()?.to_string_lossy().into_owned();
                let hidden = crate::name::is_hidden(&host_name);
                if hidden && self.settings.hidden_files == HiddenFiles::Hide {
                    return None;
                }
                if crate::name::is_meta_name(&host_name) {
                    // The server's metadata store is never enumerated
                    return None;
                }
                Some((p, crate::name::to_mac(&host_name).0))
            })
            .collect();
        let is_dir_entry = |p: &PathBuf| std::fs::metadata(p).is_ok_and(|m| m.is_dir());
        match filter {
            Offspring::Folders => offspring.retain(|(p, _)| is_dir_entry(p)),
            Offspring::Files => offspring.retain(|(p, _)| !is_dir_entry(p)),
            Offspring::All => {}
        }
        offspring.sort_by(|a, b| a.1.cmp(&b.1));
        Ok(offspring)
    }
}

/// The File/DirFlag value marking a directory in GetFileDirParms and
/// Enumerate replies: bit 7 (0x80) in every AFP version. The book's
/// reply diagrams draw the flag as the high bit of the byte, and the
/// "0 = file, 1 = directory" text gives the bit value; CAP's
/// FDP_DIRFLG is 0x80 too. A 1.1 client tests bit 7, so the value 1
/// would make every subdirectory look like a file.
pub(super) const DIR_FLAG: u8 = 0x80;

/// The WriteInhibit bit of the file attributes (ReadOnly in AFP 1.1)
pub(super) const ATTR_WRITE_INHIBIT: u16 = 0x0020;

/// The RenameInhibit and DeleteInhibit bits of the file attributes
pub(super) const ATTR_RENAME_DELETE_INHIBIT: u16 = 0x0180;

/// The attributes as a session sees them. ch13's rule for servers that
/// speak both versions: a 1.1 client reads ReadOnly as the OR of the three
/// inhibit bits.
pub(super) fn v11_attrs(version: AfpVersion, attrs: u16) -> u16 {
    if version == AfpVersion::V11 && attrs & ATTR_RENAME_DELETE_INHIBIT != 0 {
        attrs | ATTR_WRITE_INHIBIT
    } else {
        attrs
    }
}

/// Whether a session may ask for these volume parameters. ch13
/// (FPGetVolParms): BitmapErr for a parameter "that cannot be obtained
/// with this call; bitmap is null". Bits 0 to 8 are ch13's.
pub(super) fn vol_bitmap_ok(bitmap: u16) -> bool {
    bitmap != 0 && bitmap & !0x01FF == 0
}

/// The volume parameters requested by a bitmap: the fixed parameters in
/// bitmap order, the variable one (the volume name) at the end
///
/// Bit 0: attributes, bit 1: signature, bit 2: creation date, bit 3:
/// modification date, bit 4: backup date, bit 5: volume ID, bit 6:
/// bytes free, bit 7: bytes total, bit 8: volume name
pub(super) fn vol_params(bitmap: u16, name: &str, root: &Path, read_only: bool) -> Vec<u8> {
    let (bytes_free, bytes_total) = statvfs(root);

    let mut fixed = Vec::new();
    if bitmap & 0x0001 != 0 {
        // Attributes: bit 0 = read-only
        let attrs = if read_only { 1u16 } else { 0u16 };
        fixed.extend_from_slice(&attrs.to_be_bytes());
    }
    if bitmap & 0x0002 != 0 {
        // Signature: 2 = a volume with fixed Directory IDs
        fixed.extend_from_slice(&2u16.to_be_bytes());
    }
    if bitmap & 0x0004 != 0 {
        fixed.extend_from_slice(&0u32.to_be_bytes()); // creation date
    }
    if bitmap & 0x0008 != 0 {
        // The root's modification time (a host-side change deeper in
        // the tree does not show here)
        fixed.extend_from_slice(&host_mtime_date(root).to_be_bytes());
    }
    if bitmap & 0x0010 != 0 {
        // ch13: "When a volume is created, its backup date-time is set to
        // $80000000", the "never backed up" value. This server keeps no
        // backup date, so the volume never leaves that state.
        fixed.extend_from_slice(&0x8000_0000u32.to_be_bytes());
    }
    if bitmap & 0x0020 != 0 {
        fixed.extend_from_slice(&VOLUME_ID.to_be_bytes());
    }
    if bitmap & 0x0040 != 0 {
        // The 32-bit byte counts clamp below 2 GiB
        fixed.extend_from_slice(&clamp32(bytes_free).to_be_bytes());
    }
    if bitmap & 0x0080 != 0 {
        fixed.extend_from_slice(&clamp32(bytes_total).to_be_bytes());
    }
    let mut name_off_pos = None;
    if bitmap & 0x0100 != 0 {
        // Placeholder for the offset to the volume name
        name_off_pos = Some(fixed.len());
        fixed.extend_from_slice(&0u16.to_be_bytes());
    }
    let name_offset = fixed.len() as u16;
    if let Some(pos) = name_off_pos {
        fixed[pos..pos + 2].copy_from_slice(&name_offset.to_be_bytes());
    }
    let mut reply = Vec::new();
    reply.extend_from_slice(&bitmap.to_be_bytes());
    reply.extend_from_slice(&fixed);
    if bitmap & 0x0100 != 0 {
        reply.extend_from_slice(&pstring(name));
    }
    reply
}

/// Clamp a 64-bit byte count to the legacy 32-bit volume fields. The
/// AppleShare client (7.1's ApShare Mounter) treats them as signed: it
/// derives the allocation block size from the total with an arithmetic
/// shift, and a total of 2 GiB or more makes it divide by zero.
pub(super) fn clamp32(v: u64) -> u32 {
    v.min(i32::MAX as u64) as u32
}

/// The bytes free and the bytes total of the file system holding the
/// volume root (0, 0 when unknown)
#[cfg(unix)]
#[allow(clippy::useless_conversion)] // the field widths differ across platforms
pub(super) fn statvfs(path: &Path) -> (u64, u64) {
    let mut buf = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return (0, 0);
    };
    // SAFETY: buf is a valid statvfs buffer and c_path is a valid
    // null-terminated path
    if unsafe { libc::statvfs(c_path.as_ptr(), buf.as_mut_ptr()) } != 0 {
        return (0, 0);
    }
    // SAFETY: statvfs initialized the buffer on success
    let buf = unsafe { buf.assume_init() };
    // The field widths differ across platforms (macOS has 32-bit
    // block counts and a 64-bit fragment size), so widen explicitly
    let block = u64::from(buf.f_frsize);
    (
        u64::from(buf.f_bavail).saturating_mul(block),
        u64::from(buf.f_blocks).saturating_mul(block),
    )
}

#[cfg(windows)]
pub(super) fn statvfs(path: &Path) -> (u64, u64) {
    use std::os::windows::ffi::OsStrExt;
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: a NUL-terminated path, valid out-pointers, and the last one
    // may be null
    let ok =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) };
    if ok != 0 { (free, total) } else { (0, 0) }
}

#[cfg(not(any(unix, windows)))]
pub(super) fn statvfs(_path: &Path) -> (u64, u64) {
    (0, 0)
}

/// Pack the parameters of a file or directory in bitmap order (bit 0
/// first). The variable-length name fields (bits 6 and 7, the long and
/// short names) are represented by offsets from the start of the
/// parameters, with the names packed after all fixed parameters. The
/// closure returns the bytes of each fixed parameter; for bits 6 and 7
/// it returns the name to append.
pub(super) fn pack_params<F>(bitmap: u16, mut param: F) -> Vec<u8>
where
    F: FnMut(usize) -> Vec<u8>,
{
    let mut out = Vec::new();
    let mut name_offsets: Vec<usize> = Vec::new();
    let mut names: Vec<Vec<u8>> = Vec::new();
    for bit in 0..16 {
        if bitmap & (1 << bit) == 0 {
            continue;
        }
        if bit == 6 || bit == 7 {
            // A placeholder for the offset to this name
            out.extend_from_slice(&0u16.to_be_bytes());
            name_offsets.push(out.len() - 2);
            names.push(param(bit));
        } else {
            out.extend_from_slice(&param(bit));
        }
    }
    // The names follow the fixed parameters; patch the placeholders at
    // their recorded positions
    let mut pos = out.len();
    for (i, off_pos) in name_offsets.iter().enumerate() {
        out[*off_pos..*off_pos + 2].copy_from_slice(&(pos as u16).to_be_bytes());
        pos += names[i].len();
    }
    for name in names {
        out.extend_from_slice(&name);
    }
    out
}

/// The bits of the file bitmap a 2.0 server can return
pub(super) const FILE_BITMAP_ALLOWED: u16 = 0x27FF; // bits 0-10 and 13

/// The bits of the directory bitmap a 2.0 server can return
pub(super) const DIR_BITMAP_ALLOWED: u16 = 0x3FFF; // bits 0-13

/// The file parameters requested by a bitmap
pub(super) fn file_params(
    bitmap: u16,
    name: &str,
    meta: &Meta,
    parent_id: u32,
    file_id: u32,
) -> Vec<u8> {
    pack_params(bitmap, |bit| match bit {
        0 => meta.attrs.to_be_bytes().to_vec(), // attributes
        1 => parent_id.to_be_bytes().to_vec(),
        2 => meta.create_date.to_be_bytes().to_vec(),
        3 => meta.mod_date.to_be_bytes().to_vec(),
        4 => meta.backup_date.to_be_bytes().to_vec(),
        5 => meta.finder_info.to_vec(), // FInfo + FXInfo, 32 bytes in every version
        6 => pstring(name),             // long name
        7 => pstring(&short_name(name)),
        8 => file_id.to_be_bytes().to_vec(),
        9 => (meta.data_len.min(0x7FFF_FFFF) as u32)
            .to_be_bytes()
            .to_vec(),
        10 => meta.rsrc_fork_len.to_be_bytes().to_vec(),
        13 => vec![0; 6], // ProDOS info
        _ => Vec::new(),  // filtered by the allowed mask
    })
}

/// The directory parameters requested by a bitmap
pub(super) fn dir_params(
    bitmap: u16,
    name: &str,
    meta: &Meta,
    offspring: u16,
    self_id: u32,
    parent_id: u32,
    read_only: bool,
) -> Vec<u8> {
    pack_params(bitmap, |bit| match bit {
        0 => meta.attrs.to_be_bytes().to_vec(), // attributes
        1 => parent_id.to_be_bytes().to_vec(),
        2 => meta.create_date.to_be_bytes().to_vec(),
        3 => meta.mod_date.to_be_bytes().to_vec(),
        4 => meta.backup_date.to_be_bytes().to_vec(),
        5 => meta.finder_info.to_vec(), // DInfo + DXInfo, 32 bytes in every version
        6 => pstring(name),
        7 => pstring(&short_name(name)),
        8 => self_id.to_be_bytes().to_vec(),
        9 => offspring.to_be_bytes().to_vec(),
        10 => 0u32.to_be_bytes().to_vec(), // owner ID
        11 => 0u32.to_be_bytes().to_vec(), // group ID
        12 => access_rights(read_only),
        13 => vec![0; 6], // ProDOS info
        _ => Vec::new(),  // filtered by the allowed mask
    })
}

/// The 4-byte access rights long, big-endian on the wire:
/// [UARights][world][group][owner], each byte bit 0 = search,
/// bit 1 = read, bit 2 = write (make changes). The UARights owner bit
/// is set because the volume is not owned by any registered user; a
/// read-only volume strips the write bits everywhere, and the client
/// checks them before allowing a New Folder or a drop.
pub(super) fn access_rights(read_only: bool) -> Vec<u8> {
    if read_only {
        0x8303_0303u32.to_be_bytes().to_vec()
    } else {
        0x8707_0707u32.to_be_bytes().to_vec()
    }
}

/// A short (12-character) form of a name, used for the Short Name
/// parameter when requested
pub(super) fn short_name(name: &str) -> String {
    name.chars().take(12).collect()
}

/// The listing of the folder the client is paging through
pub(super) struct EnumCache {
    dir: PathBuf,
    mtime: std::time::SystemTime,
    filter: Offspring,
    offspring: Vec<(PathBuf, String)>,
}

/// The offspring an Enumerate request lists
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Offspring {
    All,
    Folders,
    Files,
}
