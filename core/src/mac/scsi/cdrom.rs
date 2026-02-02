//! SCSI CD-ROM drive (block device)

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use std::path::Path;

use crate::debuggable::Debuggable;
use crate::types::LatchingEvent;

use super::disk_image::{DiskImage, FileDiskImage};
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

const TRACK_LEADOUT: u8 = 0xAA;

#[derive(Serialize, Deserialize)]
pub(super) struct ScsiTargetCdrom {
    /// Disk contents
    #[serde(skip)]
    pub(super) backend: Option<Box<dyn DiskImage>>,

    /// Check condition code
    cc_code: u8,

    /// Check condition ASC
    cc_asc: u16,

    /// Media eject event
    event_eject: LatchingEvent,

    /// Block size
    blocksize: usize,
}

impl Default for ScsiTargetCdrom {
    fn default() -> Self {
        Self {
            backend: None,
            cc_code: 0,
            cc_asc: 0,
            event_eject: Default::default(),
            blocksize: 2048,
        }
    }
}

impl ScsiTargetCdrom {
    const VALID_BLOCKSIZES: [usize; 2] = [512, 2048];

    fn read_toc(&mut self, format: u8, track: u8, alloc_len: usize) -> Result<ScsiCmdResult> {
        if self.backend.is_none() {
            // No CD inserted
            self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
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
                        self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
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

                self.set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }

    fn eject_media(&mut self) {
        self.event_eject.set();
        self.backend = None;
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetCdrom {
    /// Try to load a disk image, given the filename of the image.
    ///
    /// This locks the file on disk and memory maps the file for use by
    /// the emulator for fast access and automatic writes back to disk,
    /// at the discretion of the operating system.
    fn load_media(&mut self, path: &Path) -> Result<()> {
        self.load_image(Box::new(FileDiskImage::open(path)?))
    }

    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()> {
        self.backend = Some(image);
        self.cc_code = 0;
        self.cc_asc = 0;
        self.event_eject.get_clear();
        Ok(())
    }

    fn media(&self) -> Option<&[u8]> {
        self.backend
            .as_ref()
            .and_then(|backend| backend.media_bytes())
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
        if self.backend.is_some() {
            // CD inserted, ready
            Ok(ScsiCmdResult::Status(STATUS_GOOD))
        } else {
            // No CD inserted
            self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
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
        result[2] = 0x02; // ANSI-2
        result[3] = 0x02; // ANSI-2

        // 4 Additional length (N-4), min. 32
        result[4] = result.len() as u8 - 4;

        // 8..16 Vendor identification
        result[8..16].copy_from_slice(b"SNOW    ");

        // 16..32 Product identification
        result[16..32].copy_from_slice(b"CD-ROM CDU-55S  ");
        // 32..36 Revision
        result[32..36].copy_from_slice(b"1.9a");
        Ok(ScsiCmdResult::DataIn(result))
    }

    fn mode_sense(&mut self, page: u8) -> Option<Vec<u8>> {
        match page {
            0x01 => {
                // Read/write recovery page

                // Error recovery stuff, can remain at 0.

                Some(vec![0; 6])
            }
            0x03 => {
                // Format device page

                Some(vec![0; 0x16])
            }
            0x30 => {
                // ? Non-standard mode page

                let mut result = vec![0; 0x16];
                result[0..0x16].copy_from_slice(b"APPLE COMPUTER, INC   ");
                Some(result)
            }
            _ => None,
        }
    }

    fn blocksize(&self) -> Option<usize> {
        Some(self.blocksize)
    }

    fn blocks(&self) -> Option<usize> {
        Some(self.backend.as_ref()?.byte_len().div_ceil(self.blocksize))
    }

    fn read(&self, block_offset: usize, block_count: usize) -> Vec<u8> {
        // If blocks() returns None this will never be called by
        // ScsiTarget::cmd
        let blocksize = self.blocksize;
        let backend = self.backend.as_ref().expect("read() but no media inserted");
        let start_offset = block_offset * blocksize;
        let image_end_offset =
            std::cmp::min((block_offset + block_count) * blocksize, backend.byte_len());

        let mut result = backend.read_bytes(start_offset, image_end_offset - start_offset);
        // CD-ROM images may not be exactly aligned on block size
        // Pad the end to a full block size
        result.resize(block_count * blocksize, 0);
        result
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {
        log::error!("Write command to CD-ROM");
    }

    fn image_fn(&self) -> Option<&Path> {
        self.backend
            .as_ref()
            .and_then(|backend| backend.image_path())
    }

    fn specific_cmd(&mut self, cmd: &[u8], _outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        match cmd[0] {
            // READ(6) (no media)
            0x08 => {
                self.set_cc(CC_KEY_MEDIUM_ERROR, ASC_MEDIUM_NOT_PRESENT);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
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
            // VENDOR SPECIFIC (EJECT)
            0xC0 => {
                self.eject_media();
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            _ => {
                log::error!("Unknown command {:02X}", cmd[0]);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }

    fn ms_density(&self) -> u8 {
        1 // User data only, 2048 bytes
    }

    fn ms_media_type(&self) -> u8 {
        2 // 120mm CD-ROM
    }

    fn ms_device_specific(&self) -> u8 {
        0
    }

    fn set_cc(&mut self, code: u8, asc: u16) {
        self.cc_code = code;
        self.cc_asc = asc;
    }

    fn set_blocksize(&mut self, blocksize: usize) -> bool {
        if Self::VALID_BLOCKSIZES.contains(&blocksize) {
            self.blocksize = blocksize;
            return true;
        }
        false
    }

    #[cfg(feature = "savestates")]
    fn after_deserialize(&mut self, imgfn: &Path) -> Result<()> {
        self.load_media(imgfn)?;
        Ok(())
    }

    fn branch_media(&mut self, _path: &Path) -> Result<()> {
        bail!("Unsupported on CD-ROM");
    }

    #[cfg(feature = "ethernet")]
    fn eth_set_link(&mut self, _link: super::ethernet::EthernetLinkType) -> Result<()> {
        unreachable!()
    }

    #[cfg(feature = "ethernet")]
    fn eth_link(&self) -> Option<super::ethernet::EthernetLinkType> {
        None
    }
}

impl Debuggable for ScsiTargetCdrom {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        vec![]
    }
}
