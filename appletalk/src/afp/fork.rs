//! Forks: open, read and lengths

use super::*;

impl Afp {
    /// FPOpenFork: open the data fork or the resource fork of a file
    pub(super) fn cmd_open_fork(
        &mut self,
        version: AfpVersion,
        session: u8,
        params: &[u8],
    ) -> Reply {
        let mut r = Reader::new(params);
        let rsrc_data_flag = r.u8()?;
        let volume_id = r.u16()?;
        if volume_id != VOLUME_ID {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let dir_id = r.u32()?;
        let bitmap = r.u16()?;
        if bitmap & !FILE_BITMAP_ALLOWED != 0 {
            return Err(ERR_BITMAP);
        }
        // The access mode is 16 bits: bit 0 requests read, bit 1
        // write (the deny bits are ignored: single client). A write
        // on a read-only volume is afpVolLocked in 2.x and
        // afpAccessDenied in 1.1 (the book's rule)
        let access = r.u16()?;
        let write = access & 0x0002 != 0;
        if write && self.settings.read_only {
            let code = if version == AfpVersion::V11 {
                ERR_ACCESS_DENIED
            } else {
                ERR_VOL_LOCKED
            };
            return Err(code);
        }
        let path = r.path()?;

        let target = self
            .resolve_path(dir_id, &path)
            .ok_or(ERR_OBJECT_NOT_FOUND)?;
        let Ok(fs_meta) = std::fs::metadata(&target) else {
            return Err(ERR_OBJECT_NOT_FOUND);
        };
        if fs_meta.is_dir() {
            return Err(ERR_OBJECT_TYPE);
        }
        if !fs_meta.is_file() {
            return Err(ERR_OBJECT_NOT_FOUND);
        }
        let meta = self.meta_open(&target);
        // ch13: a WriteInhibit file does not open for write, which is
        // ObjectLocked in 2.x and AccessDenied in 1.1. An AppleSingle
        // container reports it: this server does not write those.
        if write && v11_attrs(version, meta.attrs) & ATTR_WRITE_INHIBIT != 0 {
            let code = if version == AfpVersion::V11 {
                ERR_ACCESS_DENIED
            } else {
                ERR_OBJECT_LOCKED
            };
            return Err(code);
        }
        let parent_id = self.parent_id_of(&target);
        let fork_name = self.mac_name_of(&target);

        // The fork: the data fork lives at data_offset inside the host
        // file (nonzero for AppleSingle); the resource fork lives in
        // the metadata container. An absent resource fork reads as an
        // empty fork (a write creates it lazily).
        let rsrc = rsrc_data_flag & 0x80 != 0;
        let (file_path, offset, len) = if rsrc {
            match meta.rsrc_file.clone() {
                Some(file) => (file, meta.rsrc_offset, u64::from(meta.rsrc_fork_len)),
                None => (target.clone(), 0, 0),
            }
        } else {
            (target.clone(), meta.data_offset, meta.data_len)
        };
        let file = match open_with_access(&file_path, write) {
            Ok(file) => file,
            Err(e) => return Err(io_err(&e)),
        };

        let fork_ref = self.allocate_fork_ref().ok_or(ERR_TOO_MANY_FILES_OPEN)?;
        self.forks.insert(
            fork_ref,
            Fork {
                file,
                len,
                offset,
                name: fork_name.clone(),
                meta: meta.clone(),
                parent: parent_id,
                marker: 0,
                path: target.clone(),
                file_id: self.ids.id_of(&target).unwrap_or(0),
                read: access & 0x0001 != 0,
                session,
                rsrc,
            },
        );
        let mac_name = fork_name;
        wire_log!(
            "AFP: opened fork {} on '{}' ({} bytes{}{} at host offset {})",
            fork_ref,
            mac_name,
            len,
            if rsrc { " of the resource fork" } else { "" },
            if write { " for writing" } else { "" },
            offset
        );

        // The reply: the bitmap, the fork reference number, then the
        // requested file parameters
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap.to_be_bytes());
        reply.extend_from_slice(&fork_ref.to_be_bytes());
        let file_id = self.ids.id_of(&target).unwrap_or(0);
        reply.extend_from_slice(&file_params(bitmap, &mac_name, &meta, parent_id, file_id));
        Ok(reply)
    }

    /// The next free fork reference number: never 0, and never one that
    /// is still open (a long copy wraps the 16 bits)
    pub(super) fn allocate_fork_ref(&mut self) -> Option<u16> {
        for _ in 0..=u16::MAX {
            let fork_ref = self.next_fork_ref;
            self.next_fork_ref = self.next_fork_ref.wrapping_add(1);
            if fork_ref != 0 && !self.forks.contains_key(&fork_ref) {
                return Some(fork_ref);
            }
        }
        None
    }

    /// FPCloseFork: close an open fork
    pub(super) fn cmd_close_fork(&mut self, params: &[u8]) -> Reply {
        let fork_ref = Reader::new(params).u16()?;
        // ch13: an unknown reference number is a parameter error
        self.forks.remove(&fork_ref).ok_or(ERR_PARAM)?;
        Ok(Vec::new())
    }

    /// FPRead: read data from an open fork
    /// The data read, and whether the fork ended before the request did
    pub(super) fn cmd_read(&mut self, params: &[u8]) -> Result<(Vec<u8>, bool), i32> {
        let mut r = Reader::new(params);
        let fork_ref = r.u16()?;
        let offset = r.u32()?;
        let count = r.u32()?;
        let fork = self.forks.get_mut(&fork_ref).ok_or(ERR_PARAM)?;
        if !fork.read {
            return Err(ERR_ACCESS_DENIED);
        }
        // ch13: a negative ReqCount or Offset is a parameter error
        if (offset as i32) < 0 || (count as i32) < 0 {
            return Err(ERR_PARAM);
        }

        let offset = u64::from(offset);
        let requested = u64::from(count);
        // ch13: with a non-zero NewlineMask, the read also stops after
        // the first byte whose masked value equals NewlineChar. A zero
        // mask (the common case) reads bytes only.
        let mask = r.u8().unwrap_or(0);
        let nlchr = r.u8().unwrap_or(0);
        // The read is capped at the response capacity the requester
        // offered, so the reply fits its buffers without truncation
        let count = (count as usize).min(self.req_capacity);
        if offset >= fork.len {
            return Err(ERR_EOF);
        }
        let count = (count as u64).min(fork.len - offset) as usize;
        if fork
            .file
            .seek(SeekFrom::Start(fork.offset + offset))
            .is_err()
        {
            return Err(ERR_EOF);
        }
        let mut data = vec![0; count];
        let mut total = 0;
        let mut newline_hit = false;
        while total < count {
            match fork.file.read(&mut data[total..]) {
                Ok(0) => break,
                Ok(n) => {
                    if mask != 0
                        && let Some(pos) = data[total..total + n]
                            .iter()
                            .position(|&b| b & mask == nlchr)
                    {
                        total += pos + 1;
                        newline_hit = true;
                        break;
                    }
                    total += n;
                }
                Err(_) => break,
            }
        }
        data.truncate(total);
        fork.marker = offset + total as u64;
        wire_log!(
            "AFP: read {} bytes at {} on fork {}",
            total,
            offset,
            fork_ref
        );
        if newline_hit {
            // The line break ended the read: a complete text line
            Ok((data, false))
        } else if requested > total as u64 && fork.len <= offset + total as u64 {
            // The fork ended before the request did: the bytes read so
            // far go back with afpEOFErr (ch13's FPRead rule). A read cut
            // short by the reply's capacity is not that: the client asks
            // again for the rest.
            Ok((data, true))
        } else {
            Ok((data, false))
        }
    }

    /// FPGetForkParms: the bitmap is the file bitmap; the reply is the
    /// bitmap copy plus the requested file parameters
    pub(super) fn cmd_get_fork_parms(&self, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let fork_ref = r.u16()?;
        let bitmap = r.u16()?;
        if bitmap & !FILE_BITMAP_ALLOWED != 0 {
            return Err(ERR_BITMAP);
        }
        let fork = self.forks.get(&fork_ref).ok_or(ERR_PARAM)?;
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap.to_be_bytes());
        reply.extend_from_slice(&file_params(
            bitmap,
            &fork.name,
            &fork.meta,
            fork.parent,
            fork.file_id,
        ));
        Ok(reply)
    }
}

/// Open a file with the requested write access
pub(super) fn open_with_access(path: &Path, write: bool) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(write)
        .open(path)
}
