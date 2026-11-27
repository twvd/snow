//! The Ethernet controller on the logic board of a Mac: a SONIC

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::sonic::Sonic;
use super::{APPLE, low_word_written, mac_prom_byte};
use crate::bus::Address;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::devices::{
    BusKind, DeviceConfig, DeviceModel, DeviceOps, FunctionConfig, FunctionInfo, FunctionMut,
    ModelInfo, built_in, built_in_config,
};
use crate::mac::MacModel;
use crate::net::EthernetPort;

fn fits(mac: MacModel) -> bool {
    mac.has_built_in(DeviceModel::BuiltInSonic)
}

/// Silicon revision of the SONIC. Nobody has read it on a Quadra 700 or 800, so this is a guess:
/// Linux boot logs show revision 4 on the Quadra 650's built-in SONIC and on other Macs, the
/// DP83932C datasheet (and MAME) give 6 for the DP83932CVF, and QEMU's q800 uses 4.
pub(crate) const SONIC_REVISION: u16 = 4;

/// The SONIC on the logic board of a Quadra 700 or 800
pub const INFO: ModelInfo = ModelInfo {
    fits,
    fixed: true,
    ..ModelInfo::base(
        DeviceModel::BuiltInSonic,
        "Built-in Ethernet",
        BusKind::BuiltIn,
        FunctionInfo::Ethernet(APPLE),
    )
};

/// The Ethernet controller of a logic board. Its machine decodes the addresses of the PROM and
/// of the registers, gives them their timing, and wires the chip's interrupt and DMA.
#[derive(Serialize, Deserialize)]
pub struct BuiltInSonic {
    pub(crate) sonic: Sonic,
}

impl BuiltInSonic {
    /// The controller of a machine, with the Ethernet address that the list of device configs
    /// of the machine has for it, and its link down. The emulator gives an address to a config
    /// that has none, and sets the link once the machine is made: a link that can't start
    /// doesn't stop the machine.
    pub(crate) fn new(devices: &[DeviceConfig]) -> Result<Self> {
        let Some(DeviceConfig {
            function: FunctionConfig::Ethernet(ethernet),
            ..
        }) = built_in_config(devices, built_in::ETHERNET)
        else {
            bail!("The built-in Ethernet needs an Ethernet adapter's config");
        };
        let mac = ethernet
            .mac
            .context("An Ethernet adapter needs an Ethernet address")?;
        Ok(Self {
            sonic: Sonic::new(SONIC_REVISION, EthernetPort::new(mac)),
        })
    }

    /// The register at an offset of the I/O area: one 16-bit register per long word from
    /// $A000, on D15-D0 (Designing Cards and Drivers for the Macintosh Family, 3rd ed., table
    /// 16-5, for the Quadra 700; Macintosh Quadra 800 Developer Note, figure 2-4)
    fn register(offset: Address) -> Option<usize> {
        match offset {
            0xA000..=0xB0FF => Some(((offset - 0xA000) >> 2) as usize),
            _ => None,
        }
    }

    /// A read of `len` bytes at an offset of the I/O area, in the registers
    pub(crate) fn read(&self, offset: Address, len: u32) -> Option<u32> {
        let reg = self.sonic.read(Self::register(offset)?);
        // D31-D16 are not connected
        let long = u32::from(reg);
        let shift = 8 * (4 - (offset & 3) - len);
        Some((long >> shift) & (u32::MAX >> (32 - 8 * len)))
    }

    /// A write of `len` bytes of `val` at an offset of the I/O area, in the registers
    pub(crate) fn write(&mut self, offset: Address, len: u32, val: u32) -> Option<()> {
        let reg = Self::register(offset)?;
        if let Some(value) = low_word_written(offset, len, val) {
            self.sonic.write(reg, value);
        }
        Some(())
    }

    /// A byte of the Ethernet address PROM, at an offset of the I/O area from $8000
    pub(crate) fn prom_byte(&self, offset: Address) -> u8 {
        mac_prom_byte(self.sonic.port().mac(), (offset & 7) as usize)
    }
}

/// The device config of a built-in Ethernet, for the tests of the machines that have one
#[cfg(test)]
pub(crate) fn test_config() -> DeviceConfig {
    use crate::devices::{EthernetConfig, EthernetLinkType, bus_id};

    DeviceConfig {
        bus: bus_id::BUILT_IN.into(),
        slot: built_in::ETHERNET.into(),
        model: Some(DeviceModel::BuiltInSonic),
        rom: None,
        function: FunctionConfig::Ethernet(EthernetConfig {
            mac: Some([0x08, 0x00, 0x07, 0x12, 0x34, 0x56]),
            link: EthernetLinkType::Down,
        }),
    }
}

impl DeviceOps for BuiltInSonic {
    fn info(&self) -> &'static ModelInfo {
        &INFO
    }

    fn config(&self) -> FunctionConfig {
        FunctionConfig::Ethernet(self.sonic.port().config())
    }

    fn name(&self) -> Option<String> {
        Some(INFO.name.to_string())
    }

    fn function(&mut self) -> FunctionMut<'_> {
        // The Ethernet function of an adapter is that of its port
        FunctionMut::Ethernet(self.sonic.port_mut())
    }

    fn debug_properties(&self) -> DebuggableProperties {
        self.sonic.get_debug_properties()
    }
}
