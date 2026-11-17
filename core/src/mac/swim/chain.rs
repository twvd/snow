//! The devices chained on the external drive connector of the floppy controller

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::device::SwimBusDevice;
use crate::devices::{BusOps, DeviceOps, MadeDevice};
use crate::tickable::Ticks;

/// The devices chained on the external drive connector, in front of its drive.
///
/// A chained device sits between the controller and the rest of the chain. The first one
/// answers when the connector is enabled. On a strobe, a device may pass the selection on to
/// the next one (`SwimBusDevice::strobe()` returns true), until the connector is no longer
/// enabled. The drive of the connector is behind the last device.
///
/// The chain is the `HD20` bus of the machine.
#[derive(Serialize, Deserialize)]
pub(crate) struct Chain {
    /// The devices, by their place. The guest finds them in this order: it doesn't see a
    /// place that is empty.
    devices: [Option<Box<dyn SwimBusDevice>>; Self::PLACES],

    /// The number of devices that passed the selection on since the connector was enabled
    passed: usize,

    /// The ticks of the controller in one second, which a device is told when it is attached
    tick_rate: Ticks,
}

/// The place on the chain of a slot
fn slot_number(slot: &str) -> Option<usize> {
    Chain::SLOT_IDS.iter().position(|&id| id == slot)
}

impl BusOps for Chain {
    fn device(&mut self, slot: &str) -> Option<&mut dyn DeviceOps> {
        let device = self.devices[slot_number(slot)?].as_mut()?;
        // A device of the chain is a DeviceOps too
        Some(&mut **device)
    }

    fn attach(&mut self, slot: &str, device: MadeDevice) -> Result<()> {
        let place = slot_number(slot).context("No such place on the chain")?;
        let MadeDevice::Swim(mut device) = device else {
            bail!("Not a device for the chain of the floppy port");
        };
        if self.devices[place].is_some() {
            bail!("This place on the chain already has a device");
        }
        device.connected(place, self.tick_rate);
        self.devices[place] = Some(device);
        self.passed = 0;
        Ok(())
    }

    fn detach(&mut self, slot: &str) -> Result<()> {
        let place = slot_number(slot).context("No such place on the chain")?;
        if self.devices[place].take().is_none() {
            bail!("This place on the chain has no device");
        }
        self.passed = 0;
        Ok(())
    }
}

impl Chain {
    /// The number of places on the chain: the Mac's driver finds four HD20s at most
    /// (docs/HD20.md)
    pub(super) const PLACES: usize = 4;

    /// The ids of the slots of the bus: the places on the chain, counted from 1
    pub const SLOT_IDS: [&str; Self::PLACES] = ["1", "2", "3", "4"];

    /// An empty chain, on a controller that is ticked at `tick_rate`
    pub(super) fn new(tick_rate: Ticks) -> Self {
        Self {
            devices: Default::default(),
            passed: 0,
            tick_rate,
        }
    }

    /// The number of devices that passed the selection on, for the debugger
    pub(super) fn passed(&self) -> usize {
        self.passed
    }

    pub(super) fn is_empty(&self) -> bool {
        self.devices.iter().all(Option::is_none)
    }

    /// The devices of the chain, in their order
    pub(super) fn devices_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn SwimBusDevice>> {
        self.devices.iter_mut().flatten()
    }

    /// The place of the device that the selection has reached. None: it went past the last
    /// device, to the drive of the connector.
    pub(super) fn reached(&self) -> Option<usize> {
        (0..Self::PLACES)
            .filter(|&place| self.devices[place].is_some())
            .nth(self.passed)
    }

    /// The device at a place
    pub(super) fn device_at(&mut self, place: usize) -> Option<&mut Box<dyn SwimBusDevice>> {
        self.devices[place].as_mut()
    }

    /// The selected device passed the selection on to the next one
    pub(super) fn pass(&mut self) {
        self.passed += 1;
    }

    /// The connector is no longer enabled: the selection goes back to the first device
    pub(super) fn rewind(&mut self) {
        self.passed = 0;
    }
}
