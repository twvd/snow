//! SCSI hard disk drive (block device)

use anyhow::{bail, Context, Result};
#[cfg(feature = "mmap")]
use memmap2::Mmap;

use std::path::Path;
use std::path::PathBuf;

use crate::types::LatchingEvent;

use super::target::ScsiTarget;
use super::target::ScsiTargetEvent;
use super::target::ScsiTargetType;
use super::ScsiCmdResult;
use super::ASC_INVALID_FIELD_IN_CDB;
use super::ASC_MEDIUM_NOT_PRESENT;
use super::CC_KEY_ILLEGAL_REQUEST;
use super::CC_KEY_MEDIUM_ERROR;
use super::STATUS_CHECK_CONDITION;
use super::STATUS_GOOD;

const CDROM_BLOCKSIZE: usize = 2048;

const TRACK_LEADOUT: u8 = 0xAA;

#[derive(Default)]
pub(super) struct ScsiTargetCdrom {
    /// Disk contents
    #[cfg(feature = "mmap")]
    pub(super) disk: Option<Mmap>,

    #[cfg(not(feature = "mmap"))]
    pub(super) disk: Option<Vec<u8>>,

    /// Path where the original image resides
    pub(super) path: PathBuf,

    /// Check condition code
    cc_code: u8,

    /// Check condition ASC
    cc_asc: u16,

    /// Media eject event
    event_eject: LatchingEvent,
}

impl ScsiTargetCdrom {
    fn read_toc(&mut self, format: u8, track: u8, alloc_len: usize) -> Result<ScsiCmdResult> {
        if self.disk.is_none() {
            // No CD inserted
            self.cc_code = CC_KEY_MEDIUM_ERROR;
            self.cc_asc = ASC_MEDIUM_NOT_PRESENT;
            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        }
        match format {
            0 => {
                // SCSI-2 TOC
                match track {
                    0 | 1 => {
                        let mut result = vec![0; 0x14];

                        // Length
                        result[1] = 0x12;
                        // First track
                        result[2] = 1;
                        // Last track
                        result[3] = 1;

                        // Track descriptor for track 1
                        // 4 reserved
                        // Digital
                        result[5] = 0x14;
                        // Track number
                        result[6] = 1;
                        // 7 reserved
                        // 8..12 Start block number (0)

                        // Track descriptor for lead-out
                        // 12 reserved
                        // Digital
                        result[13] = 0x14;
                        // Track number
                        result[14] = TRACK_LEADOUT;

                        result.truncate(alloc_len);
                        Ok(ScsiCmdResult::DataIn(result))
                    }
                    TRACK_LEADOUT => {
                        let mut result = vec![0; 12];
                        // Length
                        result[1] = 0x0A;
                        // First track
                        result[2] = 1;
                        // Last track
                        result[3] = 1;

                        // Track descriptor for track 1
                        // 4 reserved
                        // Digital
                        result[5] = 0x14;
                        // Track number
                        result[6] = TRACK_LEADOUT;
                        // 7 reserved
                        // 8..12 Start block number (0)
                        result.truncate(alloc_len);
                        Ok(ScsiCmdResult::DataIn(result))
                    }
                    _ => {
                        self.cc_code = CC_KEY_ILLEGAL_REQUEST;
                        self.cc_asc = ASC_INVALID_FIELD_IN_CDB;
                        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                    }
                }
            }
            1 => {
                // Session TOC
                let mut result = vec![0; 12];

                // Length
                result[1] = 0x0A;
                // First track
                result[2] = 1;
                // Last track
                result[3] = 1;

                // Track descriptor for track 1
                // 4 reserved
                // Digital
                result[5] = 0x14;
                // Track number
                result[6] = 1;
                // 7 reserved
                // 8..12 Start block number (0)

                result.truncate(alloc_len);
                Ok(ScsiCmdResult::DataIn(result))
            }
            _ => {
                log::error!("Unknown READ TOC format: {}", format);

                self.cc_code = CC_KEY_ILLEGAL_REQUEST;
                self.cc_asc = ASC_INVALID_FIELD_IN_CDB;
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }

    fn eject_media(&mut self) {
        self.event_eject.set();
        self.disk = None;
    }
}

impl ScsiTarget for ScsiTargetCdrom {
    /// Try to load a disk image, given the filename of the image.
    ///
    /// This locks the file on disk and memory maps the file for use by
    /// the emulator for fast access and automatic writes back to disk,
    /// at the discretion of the operating system.
    #[cfg(feature = "mmap")]
    fn load_media(&mut self, path: &Path) -> Result<()> {
        use fs2::FileExt;
        use std::fs::OpenOptions;
        use std::io::{Seek, SeekFrom};

        if !Path::new(path).exists() {
            bail!("File not found: {}", path.display());
        }

        let mut f = OpenOptions::new()
            .read(true)
            .open(path)
            .with_context(|| format!("Failed to open {}", path.display()))?;

        let file_size = f.seek(SeekFrom::End(0))? as usize;
        f.seek(SeekFrom::Start(0))?;

        if file_size % CDROM_BLOCKSIZE != 0 {
            bail!(
                "Cannot load disk image {}: not multiple of {}",
                path.display(),
                CDROM_BLOCKSIZE
            );
        }

        f.try_lock_exclusive()
            .with_context(|| format!("Failed to lock {}", path.display()))?;

        let mmapped = unsafe {
            use memmap2::MmapOptions;

            MmapOptions::new()
                .len(file_size)
                .map(&f)
                .with_context(|| format!("Failed to mmap file {}", path.display()))?
        };

        self.disk = Some(mmapped);
        self.path = path.to_path_buf();
        Ok(())
    }

    #[cfg(not(feature = "mmap"))]
    fn load_media(&mut self, path: &Path) -> Result<()> {
        use std::fs;

        if !path.exists() {
            bail!("File not found: {}", path.display());
        }

        let disk =
            fs::read(path).with_context(|| format!("Failed to open file {}", path.display()))?;

        if disk.len() % CDROM_BLOCKSIZE != 0 {
            bail!(
                "Cannot load disk image {}: not multiple of {}",
                path.display(),
                CDROM_BLOCKSIZE
            );
        }

        self.disk = Some(disk);
        self.path = path.to_path_buf();
        Ok(())
    }

    fn take_event(&mut self) -> Option<ScsiTargetEvent> {
        if self.event_eject.get_clear() {
            Some(ScsiTargetEvent::MediaEjected)
        } else {
            None
        }
    }

    fn target_type(&self) -> ScsiTargetType {
        ScsiTargetType::Cdrom
    }

    fn unit_ready(&mut self) -> Result<ScsiCmdResult> {
        if self.disk.is_some() {
            // CD inserted, ready
            Ok(ScsiCmdResult::Status(STATUS_GOOD))
        } else {
            // No CD inserted
            self.cc_code = CC_KEY_MEDIUM_ERROR;
            self.cc_asc = ASC_MEDIUM_NOT_PRESENT;
            Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
        }
    }

    fn req_sense(&mut self) -> (u8, u16) {
        (
            std::mem::take(&mut self.cc_code),
            std::mem::take(&mut self.cc_asc),
        )
    }

    fn inquiry(&mut self, _cmd: &[u8]) -> Result<ScsiCmdResult> {
        let mut result = vec![0; 36];

        // 0 Peripheral qualifier (5-7), peripheral device type (4-0)
        result[0] = 5; // CD-ROM drive
        result[1] = 0x80; // Media removable

        // 4 Additional length (N-4), min. 32
        result[4] = result.len() as u8 - 4;

        // 8..16 Vendor identification
        result[8..(8 + 4)].copy_from_slice(b"SNOW");

        // 16..32 Product identification
        result[16..(16 + 14)].copy_from_slice(b"CD-ROM CDU-55S");
        Ok(ScsiCmdResult::DataIn(result))
    }

    fn mode_sense(&mut self, page: u8) -> Result<ScsiCmdResult> {
        match page {
            0x30 => {
                // ? Non-standard mode page

                let mut result = vec![0; 36];
                // Page code
                result[0] = 0x30;
                // Page length
                result[1] = 0x16;

                result[14..(14 + 22)].copy_from_slice(b"APPLE COMPUTER, INC   ");

                Ok(ScsiCmdResult::DataIn(result))
            }
            _ => {
                log::warn!("Unknown MODE SENSE page {:02X}", page);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }

    fn blocksize(&self) -> Option<usize> {
        Some(CDROM_BLOCKSIZE)
    }

    fn blocks(&self) -> Option<usize> {
        Some(self.disk.as_ref()?.len() / CDROM_BLOCKSIZE)
    }

    fn read(&self, block_offset: usize, block_count: usize) -> &[u8] {
        // If blocks() returns None this will never be called by
        // ScsiTarget::cmd
        &self.disk.as_ref().expect("read() but no media inserted")
            [(block_offset * CDROM_BLOCKSIZE)..((block_offset + block_count) * CDROM_BLOCKSIZE)]
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {
        log::error!("Write command to CD-ROM");
    }

    fn image_fn(&self) -> Option<&Path> {
        if self.disk.is_none() {
            None
        } else {
            Some(self.path.as_ref())
        }
    }

    fn specific_cmd(&mut self, cmd: &[u8], _outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        match cmd[0] {
            // START/STOP UNIT
            0x1B => {
                // LoEj + !start = eject
                let eject = cmd[4] & 0b11 == 0b10;

                if eject {
                    self.eject_media();
                }

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            // PREVENT/ALLOW MEDIA REMOVAL
            0x1E => Ok(ScsiCmdResult::Status(STATUS_GOOD)),
            // READ TOC
            0x43 => {
                let format = cmd[9] >> 6;
                let track = cmd[6];
                let alloc_len = u16::from_be_bytes(cmd[7..9].try_into()?) as usize;

                self.read_toc(format, track, alloc_len)
            }
            _ => {
                log::error!("Unknown command {:02X}", cmd[0]);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }
}
