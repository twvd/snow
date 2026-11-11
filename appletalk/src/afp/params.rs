//! Parameters: the volume parameters

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
