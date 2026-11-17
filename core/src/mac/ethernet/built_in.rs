//! The Ethernet controller on the logic board of a Mac: a SONIC

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::APPLE;
use super::sonic::Sonic;
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

/// The SONIC on the logic board of a Quadra 700
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

/// The Ethernet controller of a logic board. Its machine wires the chip to its bus and to its
/// interrupts.
#[derive(Serialize, Deserialize)]
pub struct BuiltInSonic {
    pub(crate) sonic: Sonic,
}

impl BuiltInSonic {
    /// The controller of a machine, with the Ethernet address that the list of device configs
    /// of the machine has for it, and its link down. The emulator gives an address to a config
    /// that has none, and sets the link once the machine is made: a link that can't start
    /// doesn't stop the machine.
    pub(crate) fn new(revision: u16, devices: &[DeviceConfig]) -> Result<Self> {
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
            sonic: Sonic::new(revision, EthernetPort::new(mac)),
        })
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
