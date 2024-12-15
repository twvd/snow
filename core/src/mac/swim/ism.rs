use log::*;

use crate::bus::Address;
use crate::types::Byte;

use super::Swim;

impl Swim {
    /// A memory-mapped I/O address was read
    pub(super) fn ism_read(&mut self, addr: Address) -> Option<Byte> {
        let offset = addr - 0xDFE1FF;
        debug!("ISM read {:04X}", offset);
        Some(0)
    }

    pub(super) fn ism_write(&mut self, addr: Address, value: Byte) {
        let offset = addr - 0xDFE1FF;
        debug!("ISM write {:04X} = {:02X}", offset, value);
    }
}
