//! BlueSCSI Toolbox vendor-specific commands
//!
//! This is an implementation of th BlueSCSI Toolbox v0 commands suitable for the Snow emulator.
//! CD switching is not implemented as the emulator can do this easily via the UI.
//! API Docs: https://github.com/BlueSCSI/BlueSCSI-v2/wiki/Toolbox-Developer-Docs
//! Note: THere are some limitations due to RAM/Flash space on the BlueSCSI that are not a concern
//!  on a more powerful machines. We would like to use Snow as a test/dev for the BlueSCSI toolbox.
//!  We hope to prototype v1 of the Toolbox API in Snow first, then port it back to BlueSCSI's Pico.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use log::*;

use super::{ScsiCmdResult, STATUS_CHECK_CONDITION, STATUS_GOOD};

const MAX_FILE_PATH: usize = 32; // Max Macintosh File name length

/// 0xD9 subcommands
const TOOLBOX_LIST_DEVICES: u8 = 0x00;
const TOOLBOX_GET_CAPABILITIES: u8 = 0x01;

/// Capability flags for TOOLBOX_GET_CAPABILITIES response
pub const CAP_LARGE_TRANSFERS: u8 = 0x01; // Supports >512 byte transfers
pub const CAP_LARGE_SEND: u8 = 0x02; // Supports large (32KB) send file chunks

/// Current Toolbox API version
const TOOLBOX_API_VERSION: u8 = 0;

#[derive(Default)]
pub struct BlueSCSI {
    shared_dir: Option<PathBuf>,
    file: Option<File>,
}

impl BlueSCSI {
    pub fn new(shared_dir: Option<PathBuf>) -> Self {
        Self {
            shared_dir,
            file: None,
        }
    }

    pub(crate) fn handle_command(
        &mut self,
        cmd: &[u8],
        outdata: Option<&[u8]>,
        debug_enabled: &mut bool,
    ) -> ScsiCmdResult {
        if *debug_enabled {
            debug!("BlueSCSI command: {:02X?}", cmd);
        }
        match cmd[0] {
            0xD0 => self.list_files(),
            0xD1 => self.get_file(cmd),
            0xD2 => self.count_files(),
            0xD3 => self.send_file_prep(outdata),
            0xD4 => self.send_file_10(cmd, outdata),
            0xD5 => self.send_file_end(),
            0xD6 => self.toggle_debug(cmd, debug_enabled),
            0xD9 => self.toolbox_metadata(cmd),
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

    fn count_files(&self) -> ScsiCmdResult {
        if self.shared_dir.is_none() {
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        }
        let entries = self.get_sorted_entries();
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
        const ENTRY_SIZE: usize = 40;
        let entries = self.get_sorted_entries();

        let mut data = Vec::new();
        let mut index = 0;

        for entry in entries {
            if let Some(name_str) = entry.file_name().to_str() {
                let mut file_entry = vec![0; ENTRY_SIZE];
                let metadata = entry.metadata().ok();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);

                file_entry[0] = index;
                file_entry[1] = if is_dir { 0x00 } else { 0x01 };

                let name_bytes = name_str.as_bytes();
                let len = name_bytes.len().min(MAX_FILE_PATH);
                file_entry[2..2 + len].copy_from_slice(&name_bytes[..len]);

                file_entry[36..40].copy_from_slice(&(size as u32).to_be_bytes());

                data.extend_from_slice(&file_entry);
                index += 1;
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
            let mut buffer = vec![0; bytes_requested as usize];
            if file.seek(SeekFrom::Start(offset * block_size)).is_ok() {
                if let Ok(bytes_read) = file.read(&mut buffer) {
                    buffer.truncate(bytes_read);
                    if bytes_read == 0 {
                        self.file = None;
                    }
                    return ScsiCmdResult::DataIn(buffer);
                }
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
                if let Ok(name) = std::str::from_utf8(&data[..pos]) {
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
                if file.seek(SeekFrom::Start(offset as u64 * 512)).is_ok()
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
        if let Some(file) = self.file.take() {
            if file.sync_all().is_ok() {
                return ScsiCmdResult::Status(STATUS_GOOD);
            }
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
    fn toolbox_metadata(&self, cmd: &[u8]) -> ScsiCmdResult {
        let subcommand = cmd[1];
        let alloc_len = if cmd[8] == 0 { 8 } else { cmd[8] as usize };

        // Currently max response is 8 bytes
        if alloc_len > 8 {
            error!("0xD9: allocation length {} exceeds maximum 8", alloc_len);
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        }

        match subcommand {
            TOOLBOX_LIST_DEVICES => {
                // Return 8 bytes, one for each SCSI ID
                // 0xFF = not emulated by this device
                // Snow only emulates one disk at a time, typically ID 0
                let mut response = [0xFFu8; 8];
                response[0] = 0x00; // ID 0 = Fixed disk (S2S_CFG_FIXED)
                ScsiCmdResult::DataIn(response[..alloc_len].to_vec())
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
