//! SCSI hard disk drive (block device)

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::debuggable::Debuggable;
use crate::devices::{
    BusKind, DeviceConfig, DeviceModel, DeviceOps, FunctionConfig, FunctionInfo, FunctionMut,
    HardDiskConfig, HardDiskFunction, HardDiskStatus, MadeDevice, ModelInfo,
};
use crate::mac::scsi::ASC_INVALID_COMMAND;
use crate::mac::scsi::ASC_MEDIUM_NOT_PRESENT;
use crate::mac::scsi::CC_KEY_ILLEGAL_REQUEST;
use crate::mac::scsi::CC_KEY_NOT_READY;
use crate::mac::scsi::STATUS_CHECK_CONDITION;
use crate::mac::scsi::STATUS_GOOD;
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::disk_image::{DiskImage, FileDiskImage};
use crate::mac::scsi::target::{ScsiTarget, ScsiTargetCommon};

pub const DISK_BLOCKSIZE: usize = 512;

#[derive(Serialize, Deserialize)]
pub(super) struct ScsiTargetDisk {
    common: ScsiTargetCommon,

    /// The disk's storage. A disk with none answers Not Ready.
    #[serde(skip)]
    backend: Option<Box<dyn DiskImage>>,
}

impl ScsiTargetDisk {
    /// A disk with the image file `media`, or with no storage yet.
    ///
    /// This locks the file on disk and memory maps the file for use by
    /// the emulator for fast access and automatic writes back to disk,
    /// at the discretion of the operating system.
    fn new(media: Option<&Path>) -> Result<Self> {
        let mut disk = Self {
            common: Default::default(),
            backend: None,
        };
        if let Some(filename) = media {
            if !filename.exists() {
                bail!("File {} does not exist", filename.to_string_lossy());
            }
            disk.backend = Some(Box::new(FileDiskImage::open_block_sized(
                filename,
                true,
                DISK_BLOCKSIZE,
            )?));
        }
        Ok(disk)
    }

    fn backend(&self) -> &dyn DiskImage {
        self.backend.as_deref().expect("SCSI disk backend missing")
    }

    fn backend_mut(&mut self) -> &mut dyn DiskImage {
        self.backend
            .as_deref_mut()
            .expect("SCSI disk backend missing")
    }

    fn not_ready(&mut self) -> Result<ScsiCmdResult> {
        self.common.set_cc(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
    }
}

pub const INFO: ModelInfo = ModelInfo {
    create: Some(create),
    ..ModelInfo::base(
        DeviceModel::ScsiHardDisk,
        "Hard disk",
        BusKind::Scsi,
        FunctionInfo::HardDisk,
    )
};

fn create(config: &DeviceConfig) -> Result<MadeDevice> {
    let FunctionConfig::HardDisk(disk) = &config.function else {
        bail!("A hard disk needs a hard disk's config");
    };
    Ok(MadeDevice::Scsi(Box::new(ScsiTargetDisk::new(
        disk.media.as_deref(),
    )?)))
}

impl DeviceOps for ScsiTargetDisk {
    fn info(&self) -> &'static ModelInfo {
        &INFO
    }

    fn config(&self) -> FunctionConfig {
        FunctionConfig::HardDisk(HardDiskConfig {
            media: self.status().media,
        })
    }

    fn function(&mut self) -> FunctionMut<'_> {
        FunctionMut::HardDisk(self)
    }
}

impl HardDiskFunction for ScsiTargetDisk {
    fn status(&self) -> HardDiskStatus {
        HardDiskStatus {
            media: self
                .backend
                .as_ref()
                .and_then(|backend| backend.image_path())
                .map(Path::to_path_buf),
            capacity: self.capacity().unwrap_or(0),
        }
    }

    fn branch(&mut self, path: &Path) -> Result<()> {
        self.backend
            .as_deref_mut()
            .context("The disk has no image")?
            .branch_media(path)
    }

    fn load_image(&mut self, image: Box<dyn DiskImage>) -> Result<()> {
        if self.backend.is_some() {
            bail!("The disk already has an image");
        }
        self.backend = Some(image);
        Ok(())
    }

    fn savestate_image(&self) -> Option<&[u8]> {
        self.backend.as_ref()?.media_bytes()
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetDisk {
    fn common(&mut self) -> &mut ScsiTargetCommon {
        &mut self.common
    }

    fn take_event(&mut self) -> Option<super::target::ScsiTargetEvent> {
        None
    }

    fn unit_ready(&mut self) -> Result<ScsiCmdResult> {
        if self.backend.is_none() {
            return self.not_ready();
        }
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
            0x31 => {
                // BlueSCSI vendor page
                let mut result = vec![0; 42];
                // A joke based on https://www.folklore.org/Stolen_From_Apple.html
                result[0..42].copy_from_slice(b"BlueSCSI is the BEST STOLEN FROM BLUESCSI\x00");

                Some(result)
            }
            _ => None,
        }
    }

    fn blocksize(&self) -> Option<usize> {
        Some(DISK_BLOCKSIZE)
    }

    fn blocks(&self) -> Option<usize> {
        Some(self.backend.as_ref()?.byte_len() / DISK_BLOCKSIZE)
    }

    fn read(&mut self, block_offset: usize, block_count: usize) -> Result<ScsiCmdResult> {
        if self.blocks() == Some(0) {
            // An empty image: nothing to read
            self.common.set_cc(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
            return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
        }
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
        // READ(6) and WRITE(6) are only left to the device when it has no storage
        if matches!(cmd[0], 0x08 | 0x0A) {
            return self.not_ready();
        }
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
    fn after_deserialize(&mut self, _imgfn: &Path) -> Result<()> {
        Ok(())
    }
}

impl Debuggable for ScsiTargetDisk {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A disk image in memory, which notes the file it is branched to
    struct MemoryImage {
        data: Vec<u8>,
        path: Option<PathBuf>,
    }

    impl DiskImage for MemoryImage {
        fn byte_len(&self) -> usize {
            self.data.len()
        }

        fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8> {
            self.data[offset..offset + length].to_vec()
        }

        fn write_bytes(&mut self, offset: usize, data: &[u8]) {
            self.data[offset..offset + data.len()].copy_from_slice(data);
        }

        fn media_bytes(&self) -> Option<&[u8]> {
            Some(&self.data)
        }

        fn image_path(&self) -> Option<&Path> {
            self.path.as_deref()
        }

        fn branch_media(&mut self, path: &Path) -> Result<()> {
            self.path = Some(path.to_path_buf());
            Ok(())
        }
    }

    fn image() -> Box<dyn DiskImage> {
        Box::new(MemoryImage {
            data: vec![0; 4 * DISK_BLOCKSIZE],
            path: None,
        })
    }

    fn function(disk: &mut ScsiTargetDisk) -> &mut dyn HardDiskFunction {
        match disk.function() {
            FunctionMut::HardDisk(disk) => disk,
            _ => panic!("A hard disk has the HardDisk function"),
        }
    }

    /// The status of a command, and the sense key it left
    fn status_and_key(disk: &mut ScsiTargetDisk, cmd: &[u8]) -> (u8, u8) {
        let ScsiCmdResult::Status(status) = disk.cmd(cmd, None).unwrap() else {
            panic!("Expected a status");
        };
        (status, disk.common.req_sense().0)
    }

    #[test]
    fn branch_changes_the_image_file() {
        let mut disk = ScsiTargetDisk::new(None).unwrap();
        function(&mut disk).load_image(image()).unwrap();
        assert_eq!(
            function(&mut disk).status(),
            HardDiskStatus {
                media: None,
                capacity: 4 * DISK_BLOCKSIZE,
            }
        );

        // The Config follows the image file
        function(&mut disk).branch(Path::new("branch.img")).unwrap();
        let branch = Some(PathBuf::from("branch.img"));
        assert_eq!(function(&mut disk).status().media, branch);
        assert_eq!(
            disk.config(),
            FunctionConfig::HardDisk(HardDiskConfig { media: branch })
        );
    }

    #[test]
    fn disk_without_storage_is_not_ready() {
        let mut disk = ScsiTargetDisk::new(None).unwrap();
        assert_eq!(function(&mut disk).status().capacity, 0);
        assert!(function(&mut disk).branch(Path::new("b.img")).is_err());

        let not_ready = (STATUS_CHECK_CONDITION, CC_KEY_NOT_READY);
        // TEST UNIT READY, READ(6), READ CAPACITY(10), READ(10)
        assert_eq!(status_and_key(&mut disk, &[0x00, 0, 0, 0, 0, 0]), not_ready);
        assert_eq!(status_and_key(&mut disk, &[0x08, 0, 0, 0, 1, 0]), not_ready);
        assert_eq!(status_and_key(&mut disk, &[0x25; 10]), not_ready);
        assert_eq!(
            status_and_key(&mut disk, &[0x28, 0, 0, 0, 0, 0, 0, 0, 1, 0]),
            not_ready
        );

        // Its storage arrives: it is ready, and takes no other
        function(&mut disk).load_image(image()).unwrap();
        assert_eq!(
            status_and_key(&mut disk, &[0x00, 0, 0, 0, 0, 0]),
            (STATUS_GOOD, 0)
        );
        assert!(function(&mut disk).load_image(image()).is_err());
    }
}
