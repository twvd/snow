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
        let Some(shared_dir) = &self.shared_dir else {
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        };
        let Ok(entries) = fs::read_dir(shared_dir) else {
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        };

        let mut file_count = 0;
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if !name.starts_with('.') {
                    file_count += 1;
                }
            }
        }
        ScsiCmdResult::DataIn(vec![file_count as u8])
    }

    fn list_files(&self) -> ScsiCmdResult {
        let Some(shared_dir) = &self.shared_dir else {
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        };
        const ENTRY_SIZE: usize = 40;
        let Ok(entries) = fs::read_dir(shared_dir) else {
            return ScsiCmdResult::Status(STATUS_CHECK_CONDITION);
        };

        let mut data = Vec::new();
        let mut index = 0;

        for entry in entries.flatten() {
            if let Some(name_str) = entry.file_name().to_str() {
                if name_str.starts_with('.') {
                    continue;
                }

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
        let Some(shared_dir) = &self.shared_dir else {
            return None;
        };
        let Ok(entries) = fs::read_dir(shared_dir) else {
            return None;
        };
        let mut count = 0;
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if !name.starts_with('.') {
                    if count == index {
                        return Some(entry.path());
                    }
                    count += 1;
                }
            }
        }
        None
    }

    fn get_file(&mut self, cmd: &[u8]) -> ScsiCmdResult {
        let index = cmd[1];
        let offset = u32::from_be_bytes(cmd[2..6].try_into().unwrap()) as u64;
        let block_size = 4096;

        if offset == 0 {
            let path = self.get_file_from_index(index);
            if let Some(path) = path {
                self.file = File::open(path).ok();
            }
        }

        if let Some(file) = &mut self.file {
            let mut buffer = vec![0; block_size];
            if file
                .seek(SeekFrom::Start(offset * block_size as u64))
                .is_ok()
            {
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
        let bytes_sent = u16::from_be_bytes(cmd[1..3].try_into().unwrap());
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

    fn send_file_end(&mut self) -> ScsiCmdResult {
        if let Some(file) = self.file.take() {
            if file.sync_all().is_ok() {
                return ScsiCmdResult::Status(STATUS_GOOD);
            }
        }
        ScsiCmdResult::Status(STATUS_CHECK_CONDITION)
    }
}
