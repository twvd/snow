use crate::mac::adb::AdbDeviceResponse;

use super::{AdbDevice, AdbDeviceInstance};

use log::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AdbBusState {
    /// Send command or data - ST0 = 0, ST1 = 0
    Transmit,
    /// Receive data from ADB device - ST0 = 1, ST1 = 0
    Receive1,
    /// Receive data from ADB device - ST0 = 0, ST1 = 1
    Receive2,
    /// Idle - ST0 = 1, ST1 = 1
    Idle,
}

impl AdbBusState {
    pub fn from_io(st0: bool, st1: bool) -> Self {
        match (st0, st1) {
            (false, false) => Self::Transmit,
            (true, false) => Self::Receive1,
            (false, true) => Self::Receive2,
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
    response: AdbDeviceResponse,
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
            AdbBusState::Idle => Some(0xFF),
            AdbBusState::Receive1 | AdbBusState::Receive2 => {
                Some(self.response.pop_at(0).unwrap_or(0xFF))
            }
            _ => None,
        }
    }

    pub fn cmd(&mut self, data: u8) {
        let address = (data >> 4) as usize;
        let cmd = (data >> 2) & 3;
        let reg = data & 3;

        trace!("ADB address: {:02X} cmd: {:02X}", address, cmd);
        self.response.clear();

        if cmd == 0 {
            // Reset (broadcast)
            for dev in self.devices.iter_mut().flatten() {
                dev.reset();
            }
            return;
        }

        let Some(device) = self.devices[address].as_mut() else {
            // No device at this address
            return;
        };
        self.response = match cmd {
            // Flush
            0b01 => {
                device.flush();
                AdbDeviceResponse::default()
            }
            // Listen
            0b10 => device.listen(reg),
            // Talk
            0b11 => device.talk(reg),
            _ => {
                error!(
                    "Unknown ADB command {:02X} for address {:02X}",
                    address, cmd
                );
                AdbDeviceResponse::default()
            }
        };
    }

    pub fn try_recv(&mut self) -> Option<u8> {
        self.response.pop_at(0)
    }
}
