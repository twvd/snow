//! SCSI hard disk drive (block device)

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::debuggable::Debuggable;
use crate::emulator::devices::{
    FunctionMut, HardDisk, HardDiskSettings, HardDiskStatus, replace_status,
};
use crate::mac::scsi::ASC_INVALID_COMMAND;
use crate::mac::scsi::CC_KEY_ILLEGAL_REQUEST;
use crate::mac::scsi::STATUS_CHECK_CONDITION;
use crate::mac::scsi::STATUS_GOOD;
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::disk_image::{DiskImage, FileDiskImage};
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::scsi::target::{ScsiTarget, ScsiTargetCommon};

pub const DISK_BLOCKSIZE: usize = 512;

#[derive(Serialize, Deserialize)]
pub(super) struct ScsiTargetDisk {
    common: ScsiTargetCommon,

    #[serde(skip)]
    backend: Option<Box<dyn DiskImage>>,

    /// The image the user gave: a restored machine runs on a copy
    media: Option<PathBuf>,
}

impl ScsiTargetDisk {
    pub fn new(backend: Box<dyn DiskImage>) -> Self {
        Self {
            common: Default::default(),
            media: backend.image_path().map(Path::to_path_buf),
            backend: Some(backend),
        }
    }

    /// Try to load a disk image, given the filename of the image.
    ///
    /// This locks the file on disk and memory maps the file for use by
    /// the emulator for fast access and automatic writes back to disk,
    /// at the discretion of the operating system.
    pub(super) fn load_disk(filename: &Path) -> Result<Self> {
        Ok(Self::new(Box::new(FileDiskImage::open_block_sized(
            filename,
            true,
            DISK_BLOCKSIZE,
        )?)))
    }

    fn backend(&self) -> &dyn DiskImage {
        self.backend.as_deref().expect("SCSI disk backend missing")
    }

    fn backend_mut(&mut self) -> &mut dyn DiskImage {
        self.backend
            .as_deref_mut()
            .expect("SCSI disk backend missing")
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetDisk {
    fn common(&mut self) -> &mut ScsiTargetCommon {
        &mut self.common
    }

    fn function(&mut self) -> FunctionMut<'_> {
        FunctionMut::HardDisk(self)
    }

    fn take_event(&mut self) -> Option<super::target::ScsiTargetEvent> {
        None
    }

    fn target_type(&self) -> ScsiTargetType {
        ScsiTargetType::Disk
    }

    fn unit_ready(&mut self) -> Result<ScsiCmdResult> {
        Ok(ScsiCmdResult::Status(STATUS_GOOD))
    }

    fn inquiry(&mut self, cmd: &[u8]) -> Result<ScsiCmdResult> {
        let mut result = vec![0; 36];

        // 0 Peripheral qualifier (5-7), peripheral device type (4-0)
        result[0] = 0; // Magnetic disk
        // Device Type Modifier
        result[1] = 0;

        // SCSI version compliance
        result[2] = 0x02; // ANSI-2
        result[3] = 0x02; // ANSI-2

        // 4 Additional length (N-4), min. 32
        result[4] = result.len() as u8 - 4;

        // 8..16 Vendor identification
        result[8..16].copy_from_slice(b"SNOW    ");

        // 16..32 Product identification
        result[16..32].copy_from_slice(b"VIRTUAL HDD     ");

        // 32..36 Revision
        result[32..35].copy_from_slice(b"1.0");

        // Honour the initiator's allocation length (cmd[4] for 6-byte INQUIRY).
        let alloc = cmd.get(4).copied().unwrap_or(0) as usize;
        if alloc > 0 && alloc < result.len() {
            result.truncate(alloc);
        }

        Ok(ScsiCmdResult::DataIn(result))
    }

    fn mode_sense(&mut self, page: u8) -> Option<Vec<u8>> {
        match page {
            0x01 => {
                // Read/write error recovery page
                Some(vec![
                    0b1100_0000, // DCR, DTE, PER, EER, RC, TB, ARRE, AWRE
                    8,           // Read retry count
                    0,           // Correction span
                    0,           // Head offset count
                    0,           // Data strobe offset count
                    0,           // Reserved
                    0,           // Write retry count
                    0,           // Reserved
                    0,           // Recovery time limit (MSB)
                    0,           // Recovery time limit (LSB)
                ])
            }
            0x02 => {
                // Disconnect-reconnect page
                Some(vec![
                    0, // Buffer full ratio
                    0, // Buffer empty ratio
                    0, // Bus inactivity limit (MSB)
                    0, // Bus inactivity limit (LSB)
                    0, // Disconnect time limit (MSB)
                    0, // Disconnect time limit (LSB)
                    0, // Connect time limit (MSB)
                    0, // Connect time limit (LSB)
                    0, // Maximum burst size (MSB)
                    0, // Maximum burst size (LSB)
                    0, // DID, DTDC
                    0, // Reserved
                    0, // Reserved
                    0, // Reserved
                ])
            }
            0x03 => {
                // Format device page
                Some(vec![
                    0,                             // Reserved
                    0,                             // Reserved
                    0,                             // Tracks per zone (MSB)
                    0,                             // Tracks per zone (LSB)
                    0,                             // Alternate sectors per zone (MSB)
                    0,                             // Alternate sectors per zone (LSB)
                    0,                             // Alternate tracks per zone (MSB)
                    0,                             // Alternate tracks per zone (LSB)
                    0,                             // Alternate tracks per volume (MSB)
                    0,                             // Alternate tracks per volume (LSB)
                    0,                             // Sectors per track (MSB)
                    0,                             // Sectors per track (LSB)
                    (DISK_BLOCKSIZE >> 8) as u8,   // Bytes per physical sector (MSB)
                    (DISK_BLOCKSIZE & 0xFF) as u8, // Bytes per physical sector (LSB)
                    0,                             // Interleave (MSB)
                    0,                             // Interleave (LSB)
                    0,                             // Track skew factor (MSB)
                    0,                             // Track skew factor (LSB)
                    0,                             // Cylinder skew factor (MSB)
                    0,                             // Cylinder skew factor (LSB)
                    0,                             // Flags
                    0,                             // Reserved
                ])
            }
            0x30 => {
                // ? Non-standard mode page

                let mut result = vec![0; 20];

                // The string below has to appear for HD SC Setup and possibly other tools to work.
                // https://68kmla.org/bb/index.php?threads/apple-rom-hard-disks.44920/post-493863
                result[0..20].copy_from_slice(b"APPLE COMPUTER, INC.");

                Some(result)
            }
            _ => None,
        }
    }

    fn blocksize(&self) -> Option<usize> {
        Some(DISK_BLOCKSIZE)
    }

    fn blocks(&self) -> Option<usize> {
        Some(self.backend().byte_len() / DISK_BLOCKSIZE)
    }

    fn read(&mut self, block_offset: usize, block_count: usize) -> Result<ScsiCmdResult> {
        let offset = block_offset * DISK_BLOCKSIZE;
        let length = block_count * DISK_BLOCKSIZE;
        Ok(ScsiCmdResult::DataIn(
            self.backend().read_bytes(offset, length),
        ))
    }

    fn write(&mut self, block_offset: usize, data: &[u8]) {
        let offset = block_offset * DISK_BLOCKSIZE;
        self.backend_mut().write_bytes(offset, data);
    }

    fn specific_cmd(&mut self, cmd: &[u8], _outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        log::error!("Unknown command {:02X}h", cmd[0]);
        self.common
            .set_cc(CC_KEY_ILLEGAL_REQUEST, ASC_INVALID_COMMAND);
        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
    }

    fn ms_density(&self) -> u8 {
        0
    }

    fn ms_media_type(&self) -> u8 {
        0
    }

    fn ms_device_specific(&self) -> u8 {
        0
    }

    fn set_blocksize(&mut self, _blocksize: usize) -> bool {
        false
    }

    #[cfg(feature = "savestates")]
    fn after_deserialize(&mut self, imgfn: &Path) -> Result<()> {
        self.backend = Some(Box::new(FileDiskImage::open_block_sized(
            imgfn,
            true,
            DISK_BLOCKSIZE,
        )?));
        Ok(())
    }

    #[cfg(feature = "savestates")]
    fn savestate_img_len(&self) -> Option<usize> {
        self.capacity()
    }

    #[cfg(feature = "savestates")]
    fn savestate_img_data(&self) -> Option<&[u8]> {
        self.backend().media_bytes()
    }
}

impl HardDisk for ScsiTargetDisk {
    fn update_status(&mut self, status: &mut HardDiskStatus) -> bool {
        let new = HardDiskStatus {
            settings: HardDiskSettings {
                media: self.media.clone().unwrap_or_default(),
                read_only: false,
            },
            capacity: self.capacity(),
            can_branch: true,
        };
        replace_status(status, new)
    }

    fn branch(&mut self, path: &Path) -> Result<()> {
        self.backend_mut().branch_media(path)?;
        self.media = Some(path.to_path_buf());
        Ok(())
    }
}

impl Debuggable for ScsiTargetDisk {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        vec![]
    }
}

pub const INFO: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::ScsiHardDisk,
    name: "Hard disk",
    bus: crate::emulator::devices::BusKind::Scsi,
    fits: crate::emulator::devices::ModelInfo::fits_all,
    function: crate::emulator::devices::FunctionKind::HardDisk,
    fixed: false,
    one_per_bus: false,
    rom: None,
    path_keys: &["media"],
    adds_buses: &[],
};
