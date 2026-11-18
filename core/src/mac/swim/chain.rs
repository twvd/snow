//! The devices chained on the external drive connector of the floppy controller

use serde::{Deserialize, Serialize};

use super::device::SwimBusDevice;

/// The devices chained on the external drive connector, in front of its drive.
///
/// A chained device sits between the controller and the rest of the chain. The first one
/// answers when the connector is enabled. On a strobe, a device may pass the selection on to
/// the next one (`SwimBusDevice::strobe()` returns true), until the connector is no longer
/// enabled. The drive of the connector is behind the last device.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Chain {
    /// The devices, by their place. The guest finds them in this order: it doesn't see a
    /// place that is empty.
    devices: [Option<Box<dyn SwimBusDevice>>; Self::PLACES],

    /// The number of devices that passed the selection on since the connector was enabled
    passed: usize,
}

impl Chain {
    /// The number of places on the chain
    pub(super) const PLACES: usize = 4;

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

    /// Puts a device at a place
    #[cfg(test)]
    pub(super) fn put(&mut self, place: usize, device: Box<dyn SwimBusDevice>) {
        self.devices[place] = Some(device);
        self.passed = 0;
    }
}
