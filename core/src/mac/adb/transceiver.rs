use super::{AdbDevice, AdbDeviceInstance};

use log::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AdbBusState {
    /// Send command or data - ST0 = 0, ST1 = 0
    Transmit,
    /// Wait for acknowledgement - ST0 = 1, ST1 = 0
    Ack,
    /// Receive data from ADB device - ST0 = 0, ST1 = 1
    Receive,
    /// Idle - ST0 = 1, ST1 = 1
    Idle,
}

impl AdbBusState {
    pub fn from_io(st0: bool, st1: bool) -> Self {
        match (st0, st1) {
            (false, false) => Self::Transmit,
            (true, false) => Self::Ack,
            (false, true) => Self::Receive,
            (true, true) => Self::Idle,
        }
    }
}

impl Default for AdbBusState {
    fn default() -> Self {
        Self::Idle
    }
}

/// Apple Desktop Bus transceiver
#[derive(Default)]
pub struct AdbTransceiver {
    state: AdbBusState,
    devices: [Option<AdbDeviceInstance>; 16],
}

impl AdbTransceiver {
    pub fn add_device<T>(&mut self, address: usize, device: T)
    where
        T: AdbDevice + Send + 'static,
    {
        assert!(self.devices[address].is_none());
        self.devices[address] = Some(Box::new(device));
    }

    pub fn get_irq(&self) -> bool {
        false
    }

    pub fn io(&mut self, st0: bool, st1: bool) -> Option<u8> {
        let newstate = AdbBusState::from_io(st0, st1);

        if newstate == self.state {
            return None;
        }

        self.state = newstate;
        trace!("ADB state: {:?}", self.state);
        match self.state {
            AdbBusState::Ack => Some(0),
            AdbBusState::Receive => Some(0),
            _ => None,
        }
    }

    pub fn cmd(&mut self, data: u8) {
        let address = data >> 4;
        let cmd = data & 0x0F;
        trace!("ADB address: {:02X} cmd: {:02X}", address, cmd);
    }
}
