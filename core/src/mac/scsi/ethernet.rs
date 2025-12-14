//! Daynaport SCSI Ethernet adapter

use crate::mac::scsi::target::{ScsiTarget, ScsiTargetEvent, ScsiTargetType};
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::STATUS_CHECK_CONDITION;
use crate::mac::scsi::STATUS_GOOD;

use anyhow::Result;
use rand::Rng;
use serde::{Deserialize, Serialize};

use std::path::Path;

#[derive(Serialize, Deserialize)]
pub(crate) struct ScsiTargetEthernet {
    /// Check condition code
    cc_code: u8,

    /// Check condition ASC
    cc_asc: u16,

    /// MAC address
    macaddress: [u8; 6],
}

impl Default for ScsiTargetEthernet {
    fn default() -> Self {
        let mut rand = rand::rng();
        
        Self {
            cc_code: 0,
            cc_asc: 0,
            macaddress: [0x00, 0x80, 0x19, rand.random(), rand.random(), rand.random()],
        }
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetEthernet {
    #[cfg(feature = "savestates")]
    fn after_deserialize(&mut self, _imgfn: &Path) -> Result<()> {
        todo!()
    }

    fn set_blocksize(&mut self, _blocksize: usize) -> bool {
        false
    }

    fn take_event(&mut self) -> Option<ScsiTargetEvent> {
        None
    }

    fn target_type(&self) -> ScsiTargetType {
        ScsiTargetType::Ethernet
    }

    fn unit_ready(&mut self) -> Result<ScsiCmdResult> {
        Ok(ScsiCmdResult::Status(STATUS_GOOD))
    }

    fn inquiry(&mut self, cmd: &[u8]) -> Result<ScsiCmdResult> {
        log::debug!("Eth inquiry: {:02X?}", cmd);
        let mut result = vec![0; 36];

        // 0 Peripheral qualifier (5-7), peripheral device type (4-0)
        result[0] = 3; // Processor
        result[1] = 0;

        // SCSI version compliance
        result[2] = 0x01;
        result[3] = 0x02;

        // 4 Additional length (N-4), min. 32
        result[4] = 31; //result.len() as u8 - 4;
        result[7] = 0x18;

        // 8..16 Vendor identification
        result[8..16].copy_from_slice(b"Dayna   ");

        // 16..32 Product identification
        result[16..32].copy_from_slice(b"SCSI/Link       ");

        // 32..36 Revision
        result[32..36].copy_from_slice(b"2.0f");

        result.resize(cmd[4].min(36).into(), 0);
        log::debug!("Result {} {} {:02X?}", cmd[4], result.len(), result);
        Ok(ScsiCmdResult::DataIn(result))
    }

    fn mode_sense(&mut self, page: u8) -> Option<Vec<u8>> {
        log::debug!("Mode sense: {:02X}", page);
        None
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

    fn set_cc(&mut self, code: u8, asc: u16) {
        self.cc_code = code;
        self.cc_asc = asc;
    }

    fn req_sense(&mut self) -> (u8, u16) {
        (self.cc_code, self.cc_asc)
    }

    fn blocksize(&self) -> Option<usize> {
        None
    }

    fn blocks(&self) -> Option<usize> {
        None
    }

    fn read(&self, _block_offset: usize, _block_count: usize) -> Vec<u8> {
        unreachable!()
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {
        unreachable!()
    }

    fn image_fn(&self) -> Option<&Path> {
        None
    }

    fn load_media(&mut self, _path: &Path) -> Result<()> {
        unreachable!()
    }

    fn branch_media(&mut self, _path: &Path) -> Result<()> {
        unreachable!()
    }

    fn media(&self) -> Option<&[u8]> {
        None
    }

    fn specific_cmd(&mut self, cmd: &[u8], _outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        match cmd[0] {
            0x08 => {
                // READ(6)
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            0x09 => {
                // Stats
                let mut result = vec![0; 18];
                result[0..6].copy_from_slice(&self.macaddress);
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x0A => {
                // WRITE(6)
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            0x0E => {
                // Enable/disable interface
                let enable = cmd[5] & 0x80 != 0;
                log::debug!("Interface enable: {}", enable);
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            _ => Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION)),
        }
    }
}
