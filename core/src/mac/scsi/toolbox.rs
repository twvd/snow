//! BlueSCSI Toolbox vendor-specific commands
//!
//! This is an implementation of the BlueSCSI Toolbox v0 commands suitable for the Snow emulator.
//! The CD switching commands (LIST_CDS/SET_NEXT_CD/COUNT_CDS) live in `scsi::cdrom`, as the folder
//! of images belongs to the drive rather than to the bus.
//! API Docs: https://github.com/BlueSCSI/BlueSCSI-v2/wiki/Toolbox-Developer-Docs
//! Note: There are some limitations due to RAM/Flash space on the BlueSCSI that are not a concern
//!  on more powerful machines. Snow still enforces the ones that are part of the wire protocol,
//!  such as the 100-entry listing limit, so a client developed here behaves the same on hardware.
//!  We would like to use Snow as a test/dev for the BlueSCSI toolbox. We hope to prototype v1 of
//!  the Toolbox API in Snow first, then port it back to BlueSCSI's Pico.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use log::*;

use super::{
    ASC_TOO_MANY_FILES, CC_KEY_ILLEGAL_REQUEST, STATUS_CHECK_CONDITION, STATUS_GOOD, ScsiCmdResult,
};
use crate::util::mac::{macroman_to_utf8, utf8_to_macroman};

const MAX_FILE_PATH: usize = 32; // Max Macintosh File name length

/// Maximum entries in a Toolbox directory listing.
/// `MAX_FILE_LISTING_FILES` in BlueSCSI_Toolbox.h.
pub(crate) const MAX_FILE_LISTING_FILES: usize = 100;

/// 0xD9 subcommands
const TOOLBOX_LIST_DEVICES: u8 = 0x00;
const TOOLBOX_GET_CAPABILITIES: u8 = 0x01;

/// Capability flags for TOOLBOX_GET_CAPABILITIES response
pub const CAP_LARGE_TRANSFERS: u8 = 0x01; // Supports >512 byte transfers
pub const CAP_LARGE_SEND: u8 = 0x02; // Supports large (32KB) send file chunks

/// Current Toolbox API version
const TOOLBOX_API_VERSION: u8 = 0;

/// Size of a single BlueSCSI Toolbox directory listing entry, in bytes.
pub(crate) const TOOLBOX_ENTRY_SIZE: usize = 40;

/// Builds a single 40-byte BlueSCSI Toolbox listing entry. Used by both the
/// shared-folder file listing (0xD0) and the folder-backed CD-ROM image
/// listing (0xD7) so the wire format stays identical.
///
/// Layout: `[0]` index, `[1]` type (0x01 = file, 0x00 = directory),
/// `[2..35]` NUL-padded MacRoman name (max 32 chars), `[36..40]` big-endian size.
pub(crate) fn toolbox_file_entry(
    index: u8,
    is_dir: bool,
    name: &str,
    size: u64,
) -> [u8; TOOLBOX_ENTRY_SIZE] {
    let mut entry = [0u8; TOOLBOX_ENTRY_SIZE];
    entry[0] = index;
    entry[1] = if is_dir { 0x00 } else { 0x01 };
    let name_bytes = utf8_to_macroman(name);
    let len = name_bytes.len().min(MAX_FILE_PATH);
    entry[2..2 + len].copy_from_slice(&name_bytes[..len]);
    entry[36..40].copy_from_slice(&(size as u32).to_be_bytes());
    entry
}

/// Reads until `buf` is full or the file ends, returning the byte count.
/// A single `read()` is allowed to come up short, which would end the guest's
/// transfer early on a file the guest still has more of.
fn read_fully(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match file.read(&mut buf[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

#[derive(Default)]
pub struct BlueSCSI {
    shared_dir: Option<PathBuf>,
    file: Option<File>,

    /// Sense for the controller to hand to the addressed target.
    pending_sense: Option<(u8, u16)>,
}

impl BlueSCSI {
    pub fn new(shared_dir: Option<PathBuf>) -> Self {
        Self {
            shared_dir,
            file: None,
            pending_sense: None,
        }
    }

    pub(crate) fn take_pending_sense(&mut self) -> Option<(u8, u16)> {
        self.pending_sense.take()
    }

    pub(crate) fn handle_command(
        &mut self,
        cmd: &[u8],
        outdata: Option<&[u8]>,
        debug_enabled: &mut bool,
        devices: &[u8; 8],
    ) -> ScsiCmdResult {
        if *debug_enabled {
            debug!("BlueSCSI command: {:02X?}", cmd);
        }
        self.pending_sense = None;
        match cmd[0] {
            0xD0 => self.list_files(),
            0xD1 => self.get_file(cmd),
            0xD2 => self.count_files(),
            0xD3 => self.send_file_prep(outdata),
            0xD4 => self.send_file_10(cmd, outdata),
            0xD5 => self.send_file_end(),
            0xD6 => self.toggle_debug(cmd, debug_enabled),
            0xD9 => self.toolbox_metadata(cmd, devices),
            _ => {
                error!("Unknown BlueSCSI command: {:02X}", cmd[0]);
                ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
            }
        }
    }

    fn toggle_debug(&self, cmd: &[u8], debug_enabled: &mut bool) -> ScsiCmdResult {
        if cmd[1] == 0 {
            *debug_enabled = cmd[2] != 0;
            debug!("Set BlueSCSI debug logs to: {}", *debug_enabled);
            ScsiCmdResult::Status(STATUS_GOOD)
        } else {
            debug!("Get BlueSCSI debug logs state: {}", *debug_enabled);
            ScsiCmdResult::DataIn(vec![*debug_enabled as u8])
        }
    }

    fn count_files(&mut self) -> ScsiCmdResult {
        if self.shared_dir.is_none() {
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        }
        let entries = self.get_sorted_entries();
        if entries.len() > MAX_FILE_LISTING_FILES {
            error!(
                "Toolbox COUNT_FILES: {} files in shared folder, maximum is {}",
                entries.len(),
                MAX_FILE_LISTING_FILES
            );
            self.pending_sense = Some((CC_KEY_ILLEGAL_REQUEST, ASC_TOO_MANY_FILES));
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        }
        ScsiCmdResult::DataIn(vec![entries.len() as u8])
    }

    /// Returns directory entries sorted by name for consistent ordering.
    /// fs::read_dir does not guarantee order, which causes index mismatches
    /// between list_files and get_file_from_index calls.
    fn get_sorted_entries(&self) -> Vec<fs::DirEntry> {
        let Some(shared_dir) = &self.shared_dir else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(shared_dir) else {
            return Vec::new();
        };

        let mut sorted: Vec<_> = entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| !n.starts_with('.'))
                    .unwrap_or(false)
            })
            .collect();
        sorted.sort_by_key(|e| e.file_name());
        sorted
    }

    fn list_files(&self) -> ScsiCmdResult {
        if self.shared_dir.is_none() {
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        };
        let entries = self.get_sorted_entries();

        let mut data = Vec::new();
        let mut index = 0;

        for entry in &entries {
            if let Some(name_str) = entry.file_name().to_str() {
                let metadata = entry.metadata().ok();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);

                data.extend_from_slice(&toolbox_file_entry(index, is_dir, name_str, size));
                index += 1;
                // Firmware truncates here rather than erroring; COUNT_FILES
                // is where the overflow is reported.
                if usize::from(index) >= MAX_FILE_LISTING_FILES {
                    break;
                }
            }
        }
        ScsiCmdResult::DataIn(data)
    }

    fn get_file_from_index(&self, index: u8) -> Option<PathBuf> {
        let entries = self.get_sorted_entries();
        entries.get(index as usize).map(|e| e.path())
    }

    fn get_file(&mut self, cmd: &[u8]) -> ScsiCmdResult {
        let index = cmd[1];
        let offset = u32::from_be_bytes(cmd[2..6].try_into().unwrap()) as u64;
        let block_size: u64 = 4096;
        // cmd[6] = number of 4K blocks to transfer (0 = 1 for backward compatibility)
        let block_count = if cmd[6] == 0 { 1 } else { cmd[6] as u64 };
        let bytes_requested = block_count * block_size;

        if offset == 0 {
            // Close any previously open file before opening new one
            self.file = None;
            let path = self.get_file_from_index(index);
            if let Some(path) = path {
                self.file = File::open(path).ok();
            }
        }

        if let Some(file) = &mut self.file {
            let byte_offset = offset * block_size;
            let file_total = file.metadata().map(|m| m.len()).unwrap_or(0);
            // Cap to what is left of the file, as onGetFile10() does. Reading
            // past the end would otherwise leave the guest waiting on bytes
            // that never arrive.
            let bytes_requested = bytes_requested.min(file_total.saturating_sub(byte_offset));

            let mut buffer = vec![0; bytes_requested as usize];
            if file.seek(SeekFrom::Start(byte_offset)).is_ok()
                && let Ok(bytes_read) = read_fully(file, &mut buffer)
            {
                buffer.truncate(bytes_read);
                // Close as soon as the transfer reaches the end of the file,
                // matching gFile.close() in onGetFile10(). Holding the file
                // open until the next transfer starts keeps a handle on the
                // shared folder that the host cannot always overwrite: on
                // Windows a delete-and-recreate leaves the old file
                // delete-pending until we let go, so the guest keeps seeing
                // the copy it already had.
                if byte_offset + bytes_read as u64 >= file_total {
                    self.file = None;
                }
                return ScsiCmdResult::DataIn(buffer);
            }
        }
        ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
    }

    fn send_file_prep(&mut self, outdata: Option<&[u8]>) -> ScsiCmdResult {
        let Some(shared_dir) = &self.shared_dir else {
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        };
        if let Some(data) = outdata {
            if let Some(pos) = data.iter().position(|&b| b == 0) {
                let name = macroman_to_utf8(&data[..pos]);
                let path = shared_dir.join(name);
                match File::create(path) {
                    Ok(f) => {
                        self.file = Some(f);
                        return ScsiCmdResult::Status(STATUS_GOOD);
                    }
                    Err(e) => {
                        error!("Failed to create file: {}", e);
                    }
                }
            }
        } else {
            // Expecting data out
            return ScsiCmdResult::DataOut(32 + 1);
        }
        ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
    }

    fn send_file_10(&mut self, cmd: &[u8], outdata: Option<&[u8]>) -> ScsiCmdResult {
        // CDB[6] = block count for new block-based encoding (0 = use legacy CDB[1-2])
        let block_count = cmd[6];
        let bytes_sent = if block_count > 0 {
            // New block-based encoding: transfer size = CDB[6] × 512 bytes
            block_count as u16 * 512
        } else {
            // Legacy encoding: Number of bytes sent this request
            u16::from_be_bytes(cmd[1..3].try_into().unwrap())
        };
        let mut offset_bytes = [0u8; 4];
        offset_bytes[1..4].copy_from_slice(&cmd[3..6]);
        let offset = u32::from_be_bytes(offset_bytes);

        if let Some(file) = &mut self.file {
            if let Some(data) = outdata {
                // Offset is relative to the end of the last write, matching
                // gFile.seekCur(offset * 512) in BlueSCSI_Toolbox.cpp. Seeking
                // absolutely truncates every upload past the first block.
                if file.seek(SeekFrom::Current(offset as i64 * 512)).is_ok()
                    && file.write_all(&data[..bytes_sent as usize]).is_ok()
                {
                    return ScsiCmdResult::Status(STATUS_GOOD);
                }
            } else {
                return ScsiCmdResult::DataOut(bytes_sent as usize);
            }
        }
        ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
    }

    /// Match BlueSCSI behavior: no data phase, go directly to status
    fn send_file_end(&mut self) -> ScsiCmdResult {
        if let Some(file) = self.file.take()
            && file.sync_all().is_ok()
        {
            return ScsiCmdResult::Status(STATUS_GOOD);
        }
        ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
    }

    /// 0xD9 - Toolbox metadata/capabilities command
    /// Subcommand in CDB[1]:
    ///   0x00 = List devices (8 bytes, one per SCSI ID)
    ///   0x01 = Get capabilities (8 bytes)
    /// Allocation length in CDB[8]:
    ///   0 = 8 bytes (backward compatibility)
    ///   1-8 = requested number of bytes
    ///   >8 = error (INVALID_FIELD_IN_CDB)
    fn toolbox_metadata(&self, cmd: &[u8], devices: &[u8; 8]) -> ScsiCmdResult {
        let subcommand = cmd[1];
        let alloc_len = if cmd[8] == 0 { 8 } else { cmd[8] as usize };

        // Currently max response is 8 bytes
        if alloc_len > 8 {
            error!("0xD9: allocation length {} exceeds maximum 8", alloc_len);
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        }

        match subcommand {
            TOOLBOX_LIST_DEVICES => {
                // Return 8 bytes, one for each SCSI ID, with the BlueSCSI device
                // type code of the attached target (0xFF = no device on that ID).
                ScsiCmdResult::DataIn(devices[..alloc_len].to_vec())
            }
            TOOLBOX_GET_CAPABILITIES => {
                // Return capabilities structure:
                // Byte 0: API version
                // Byte 1: Capability flags
                // Bytes 2-7: Reserved for future use
                let mut response = [0u8; 8];
                response[0] = TOOLBOX_API_VERSION;
                response[1] = CAP_LARGE_TRANSFERS | CAP_LARGE_SEND;
                ScsiCmdResult::DataIn(response[..alloc_len].to_vec())
            }
            _ => {
                error!("Unknown 0xD9 subcommand: {:02X}", subcommand);
                ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::mac::scsi::{ASC_TOO_MANY_FILES, CC_KEY_ILLEGAL_REQUEST};

    const NO_DEVICES: [u8; 8] = [0xFF; 8];

    struct TempDir(PathBuf);

    impl TempDir {
        /// Names sort lexically in creation order, matching listing order.
        fn with_files(tag: &str, n: usize) -> Self {
            let dir = std::env::temp_dir().join(format!("snow_toolbox_{}", tag));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            for i in 0..n {
                fs::write(dir.join(format!("f{:04}.txt", i)), b"x").unwrap();
            }
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn cmd(tb: &mut BlueSCSI, op: u8) -> ScsiCmdResult {
        let mut debug = false;
        tb.handle_command(
            &[op, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            None,
            &mut debug,
            &NO_DEVICES,
        )
    }

    fn count_files(tb: &mut BlueSCSI) -> Result<u8, u8> {
        match cmd(tb, 0xD2) {
            ScsiCmdResult::DataIn(d) => {
                assert_eq!(d.len(), 1, "COUNT_FILES returns a single byte");
                Ok(d[0])
            }
            ScsiCmdResult::Status(s) => Err(s),
            ScsiCmdResult::DataOut(_) => panic!("COUNT_FILES asked for data out"),
        }
    }

    fn list_files(tb: &mut BlueSCSI) -> Vec<u8> {
        match cmd(tb, 0xD0) {
            ScsiCmdResult::DataIn(d) => d,
            _ => panic!("LIST_FILES returned no data"),
        }
    }

    #[test]
    fn count_files_at_limit_succeeds() {
        let dir = TempDir::with_files("count_at_limit", MAX_FILE_LISTING_FILES);
        let mut tb = BlueSCSI::new(Some(dir.0.clone()));

        assert_eq!(count_files(&mut tb), Ok(MAX_FILE_LISTING_FILES as u8));
        assert_eq!(tb.take_pending_sense(), None);
    }

    #[test]
    fn count_files_over_limit_reports_too_many_files() {
        for n in [MAX_FILE_LISTING_FILES + 1, 256, 300] {
            let dir = TempDir::with_files(&format!("count_over_{}", n), n);
            let mut tb = BlueSCSI::new(Some(dir.0.clone()));

            assert_eq!(
                count_files(&mut tb),
                Err(STATUS_CHECK_CONDITION),
                "{} files should be rejected, not counted",
                n
            );
            assert_eq!(
                tb.take_pending_sense(),
                Some((CC_KEY_ILLEGAL_REQUEST, ASC_TOO_MANY_FILES))
            );
        }
    }

    #[test]
    fn count_files_clears_sense_from_a_previous_command() {
        let dir = TempDir::with_files("sense_cleared", MAX_FILE_LISTING_FILES + 1);
        let mut tb = BlueSCSI::new(Some(dir.0.clone()));

        assert_eq!(count_files(&mut tb), Err(STATUS_CHECK_CONDITION));
        assert!(tb.take_pending_sense().is_some());

        list_files(&mut tb);
        assert_eq!(tb.take_pending_sense(), None);
    }

    /// 256 files used to overflow the u8 entry index.
    #[test]
    fn list_files_truncates_at_the_limit() {
        for n in [MAX_FILE_LISTING_FILES + 1, 256, 300] {
            let dir = TempDir::with_files(&format!("list_over_{}", n), n);
            let mut tb = BlueSCSI::new(Some(dir.0.clone()));

            let data = list_files(&mut tb);
            assert_eq!(
                data.len(),
                MAX_FILE_LISTING_FILES * TOOLBOX_ENTRY_SIZE,
                "{} files should list as {} entries",
                n,
                MAX_FILE_LISTING_FILES
            );

            for i in 0..MAX_FILE_LISTING_FILES {
                let entry = &data[i * TOOLBOX_ENTRY_SIZE..(i + 1) * TOOLBOX_ENTRY_SIZE];
                assert_eq!(entry[0], i as u8);
                let end = entry[2..35].iter().position(|&b| b == 0).unwrap();
                assert_eq!(
                    std::str::from_utf8(&entry[2..2 + end]).unwrap(),
                    format!("f{:04}.txt", i)
                );
            }
        }
    }

    /// SEND_FILE_PREP, then `chunks` blocks of `blocks_per_chunk` * 512 bytes,
    /// then SEND_FILE_END. Returns what landed on disk.
    fn send_file(dir: &Path, name: &str, chunks: usize, blocks_per_chunk: u8) -> Vec<u8> {
        let mut tb = BlueSCSI::new(Some(dir.to_path_buf()));
        let mut debug = false;

        let mut prep = vec![0u8; 33];
        prep[..name.len()].copy_from_slice(name.as_bytes());
        tb.handle_command(
            &[0xD3, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            Some(&prep),
            &mut debug,
            &NO_DEVICES,
        );

        let mut expected = Vec::new();
        for c in 0..chunks {
            let payload = vec![c as u8 + 1; blocks_per_chunk as usize * 512];
            expected.extend_from_slice(&payload);
            // Clients send offset 0 and rely on the append that produces.
            let cdb = [0xD4, 0, 0, 0, 0, 0, blocks_per_chunk, 0, 0, 0];
            tb.handle_command(&cdb, Some(&payload), &mut debug, &NO_DEVICES);
        }
        tb.handle_command(
            &[0xD5, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            None,
            &mut debug,
            &NO_DEVICES,
        );

        let written = fs::read(dir.join(name)).unwrap();
        assert_eq!(written.len(), expected.len(), "file length");
        assert_eq!(written, expected, "file contents");
        written
    }

    #[test]
    fn send_file_single_chunk() {
        let dir = TempDir::with_files("send_single", 0);
        assert_eq!(send_file(&dir.0, "one.bin", 1, 4).len(), 4 * 512);
    }

    /// The absolute seek this replaced rewound to byte 0 on every chunk, so an
    /// upload larger than one chunk ended up as just its last chunk.
    #[test]
    fn send_file_multiple_chunks_appends() {
        let dir = TempDir::with_files("send_multi", 0);
        assert_eq!(send_file(&dir.0, "many.bin", 5, 2).len(), 5 * 2 * 512);
    }

    #[test]
    fn list_files_under_the_limit_is_complete() {
        let dir = TempDir::with_files("list_under", 3);
        let mut tb = BlueSCSI::new(Some(dir.0.clone()));

        assert_eq!(list_files(&mut tb).len(), 3 * TOOLBOX_ENTRY_SIZE);
    }
}

#[cfg(test)]
mod fd_lifetime_tests {
    use super::*;

    const CHUNK: usize = 16; // blocks_per_xfer in SCSITransfer

    fn cdb_get(index: u8, offset: u32, blocks: u8) -> [u8; 10] {
        let o = offset.to_be_bytes();
        [0xD1, index, o[0], o[1], o[2], o[3], blocks, 0, 0, 0]
    }

    fn req(tb: &mut BlueSCSI, cdb: &[u8]) -> Option<Vec<u8>> {
        let mut dbg = false;
        match tb.handle_command(cdb, None, &mut dbg, &[0xFF; 8]) {
            ScsiCmdResult::DataIn(d) => Some(d),
            _ => None,
        }
    }

    /// Mimics SCSITransfer's transferFile(): 16x4K chunks from offset 0,
    /// taking min(remaining, 64K) of each response, `size` = the size the
    /// client believes the file has (i.e. from its cached listing).
    fn client_download(tb: &mut BlueSCSI, index: u8, size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut x: u32 = 0;
        let blocks = size.div_ceil(4096);
        while blocks > x as usize {
            let Some(d) = req(tb, &cdb_get(index, x, CHUNK as u8)) else {
                break;
            };
            let want = (size - out.len()).min(CHUNK * 4096);
            let got = d.len().min(want);
            if got == 0 {
                break;
            }
            out.extend_from_slice(&d[..got]);
            x += got.div_ceil(4096) as u32;
        }
        out
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("snow_fd_{}", tag));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn write(&self, name: &str, byte: u8, len: usize) {
            fs::write(self.0.join(name), vec![byte; len]).unwrap();
        }
        /// Replace via a temp file + rename: a NEW inode, like a host-side
        /// copy or "save as" over the old file.
        fn replace(&self, name: &str, byte: u8, len: usize) {
            let tmp = self.0.join(format!(".{}.tmp", name));
            fs::write(&tmp, vec![byte; len]).unwrap();
            fs::rename(&tmp, self.0.join(name)).unwrap();
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn assert_all(data: &[u8], byte: u8, len: usize, what: &str) {
        assert_eq!(data.len(), len, "{}: length", what);
        assert!(
            data.iter().all(|&b| b == byte),
            "{}: expected all {:02X}, got {:02X?}...",
            what,
            byte,
            &data[..data.len().min(8)]
        );
    }

    /// Two complete transfers of the same file, overwritten in place between.
    #[test]
    fn overwrite_in_place_between_transfers() {
        let d = Dir::new("inplace");
        d.write("a.bin", b'A', 200_000);
        let mut tb = BlueSCSI::new(Some(d.0.clone()));

        assert_all(
            &client_download(&mut tb, 0, 200_000),
            b'A',
            200_000,
            "first",
        );
        d.write("a.bin", b'B', 200_000);
        assert_all(
            &client_download(&mut tb, 0, 200_000),
            b'B',
            200_000,
            "second",
        );
    }

    /// Same, but the host replaces the file with a new inode (mv/rename).
    #[test]
    fn replace_inode_between_transfers() {
        let d = Dir::new("inode");
        d.write("a.bin", b'A', 200_000);
        let mut tb = BlueSCSI::new(Some(d.0.clone()));

        assert_all(
            &client_download(&mut tb, 0, 200_000),
            b'A',
            200_000,
            "first",
        );
        d.replace("a.bin", b'B', 200_000);
        assert_all(
            &client_download(&mut tb, 0, 200_000),
            b'B',
            200_000,
            "second",
        );
    }

    /// The client abandons a transfer partway (error, user cancel), leaving
    /// Snow's fd open, then the host overwrites and the client retries.
    #[test]
    fn abandoned_transfer_then_overwrite() {
        let d = Dir::new("abandon");
        d.write("a.bin", b'A', 200_000);
        let mut tb = BlueSCSI::new(Some(d.0.clone()));

        // one chunk only, then give up
        let first = req(&mut tb, &cdb_get(0, 0, CHUNK as u8)).unwrap();
        assert_eq!(first[0], b'A');

        d.write("a.bin", b'B', 200_000);
        assert_all(
            &client_download(&mut tb, 0, 200_000),
            b'B',
            200_000,
            "retry",
        );
    }

    /// Client's cached size is stale-small (host file grew). Snow should still
    /// serve the NEW bytes for what is asked.
    #[test]
    fn stale_small_size_still_serves_new_bytes() {
        let d = Dir::new("stale_small");
        d.write("a.bin", b'A', 10);
        let mut tb = BlueSCSI::new(Some(d.0.clone()));

        assert_all(&client_download(&mut tb, 0, 10), b'A', 10, "first");
        d.write("a.bin", b'B', 100);
        assert_all(
            &client_download(&mut tb, 0, 10),
            b'B',
            10,
            "stale-size retry",
        );
    }

    /// Client's cached size is stale-large (host file shrank).
    #[test]
    fn stale_large_size_still_serves_new_bytes() {
        let d = Dir::new("stale_large");
        d.write("a.bin", b'A', 200_000);
        let mut tb = BlueSCSI::new(Some(d.0.clone()));

        assert_all(
            &client_download(&mut tb, 0, 200_000),
            b'A',
            200_000,
            "first",
        );
        d.write("a.bin", b'B', 70_000);
        let got = client_download(&mut tb, 0, 200_000);
        assert!(
            got.iter().all(|&b| b == b'B'),
            "stale-size retry served bytes from the old file"
        );
    }

    /// A second file downloaded right after the first, with the fd from the
    /// first still open.
    #[test]
    fn second_file_after_open_fd() {
        let d = Dir::new("twofiles");
        d.write("a.bin", b'A', 200_000);
        d.write("b.bin", b'B', 200_000);
        let mut tb = BlueSCSI::new(Some(d.0.clone()));

        assert_all(
            &client_download(&mut tb, 0, 200_000),
            b'A',
            200_000,
            "a.bin",
        );
        assert_all(
            &client_download(&mut tb, 1, 200_000),
            b'B',
            200_000,
            "b.bin",
        );
    }
}

#[cfg(test)]
mod fd_close_tests {
    use super::*;

    fn get(tb: &mut BlueSCSI, index: u8, offset: u32, blocks: u8) -> ScsiCmdResult {
        let o = offset.to_be_bytes();
        let mut dbg = false;
        tb.handle_command(
            &[0xD1, index, o[0], o[1], o[2], o[3], blocks, 0, 0, 0],
            None,
            &mut dbg,
            &[0xFF; 8],
        )
    }

    fn dir(tag: &str, len: usize) -> PathBuf {
        let d = std::env::temp_dir().join(format!("snow_fdclose_{}", tag));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("a.bin"), vec![b'A'; len]).unwrap();
        d
    }

    /// The firmware closes gFile once a transfer reaches the end of the file.
    /// Snow used to hold it open until the next transfer started, keeping a
    /// handle on a shared-folder file the host may be trying to replace.
    #[test]
    fn file_is_closed_at_end_of_transfer() {
        let d = dir("closed", 4096);
        let mut tb = BlueSCSI::new(Some(d.clone()));

        assert!(matches!(get(&mut tb, 0, 0, 1), ScsiCmdResult::DataIn(v) if v.len() == 4096));
        assert!(
            tb.file.is_none(),
            "file left open after a complete transfer"
        );

        // A continuation with no transfer in progress has nothing to read from.
        assert!(matches!(
            get(&mut tb, 0, 1, 1),
            ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
        ));
        let _ = fs::remove_dir_all(&d);
    }

    /// A multi-chunk transfer keeps the file open until its last chunk.
    #[test]
    fn file_stays_open_mid_transfer() {
        let d = dir("midxfer", 200_000);
        let mut tb = BlueSCSI::new(Some(d.clone()));

        assert!(matches!(get(&mut tb, 0, 0, 16), ScsiCmdResult::DataIn(v) if v.len() == 65536));
        assert!(tb.file.is_some(), "file closed mid-transfer");
        assert!(matches!(get(&mut tb, 0, 16, 16), ScsiCmdResult::DataIn(v) if v.len() == 65536));
        assert!(matches!(get(&mut tb, 0, 32, 16), ScsiCmdResult::DataIn(v) if v.len() == 65536));

        // Last chunk: 200000 - 196608 = 3392 bytes, capped to the file size.
        assert!(matches!(get(&mut tb, 0, 48, 16), ScsiCmdResult::DataIn(v) if v.len() == 3392));
        assert!(tb.file.is_none(), "file left open after the last chunk");
        let _ = fs::remove_dir_all(&d);
    }

    /// Reads never run past the end of the file, as onGetFile10() caps them.
    #[test]
    fn read_is_capped_to_the_file_size() {
        let d = dir("capped", 100);
        let mut tb = BlueSCSI::new(Some(d.clone()));

        match get(&mut tb, 0, 0, 16) {
            ScsiCmdResult::DataIn(v) => assert_eq!(v.len(), 100),
            _ => panic!("GET_FILE failed"),
        }
        let _ = fs::remove_dir_all(&d);
    }
}
