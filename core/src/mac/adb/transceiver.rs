use crate::mac::adb::AdbDeviceResponse;

use super::{AdbDevice, AdbDeviceInstance};

use crate::debuggable::{Debuggable, DebuggableProperties};

/// ADB Bus/transceiver states
#[derive(Clone, Copy, PartialEq, Eq, Debug, strum::IntoStaticStr)]
enum AdbBusState {
    /// Send command - ST0 = 0, ST1 = 0
    Command,
    /// Send/receive data from ADB device - ST0 = 1, ST1 = 0
    Data1,
    /// Send/receive data from ADB device - ST0 = 0, ST1 = 1
    Data2,
    /// Idle - ST0 = 1, ST1 = 1
    Idle,
}

impl AdbBusState {
    pub fn from_io(st0: bool, st1: bool) -> Self {
        match (st0, st1) {
            (false, false) => Self::Command,
            (true, false) => Self::Data1,
            (false, true) => Self::Data2,
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
    /// Current bus state
    state: AdbBusState,

    /// Devices on the ADB bus
    devices: Vec<AdbDeviceInstance>,

    /// Response that is currently being clocked out
    response: AdbDeviceResponse,

    /// Command data being clocked in
    cmd: Vec<u8>,
}

impl AdbTransceiver {
    pub fn add_device<T>(&mut self, device: T)
    where
        T: AdbDevice + Send + 'static,
    {
        self.devices.push(Box::new(device));
    }

    pub fn get_int(&self) -> bool {
        // The INT line has a different meaning in each phase.
        // In Data1, it signals that the command response has ended or that the
        // device did not respond (e.g. a keyboard Talk with no events pending).
        // In Idle and Data2, it signals a pending SRQ.
        // This is also the reason why ADB command responses must always
        // be an even number in length.

        match self.state {
            AdbBusState::Command => false,
            AdbBusState::Data1 => self.cmd.is_empty() && self.response.is_empty(),
            AdbBusState::Data2 | AdbBusState::Idle => self.device_has_srq(),
        }
    }

    /// A device has asserted Service Request
    fn device_has_srq(&self) -> bool {
        self.devices.iter().any(|d| d.get_srq())
    }

    pub fn io(&mut self, st0: bool, st1: bool) -> Option<u8> {
        let newstate = AdbBusState::from_io(st0, st1);

        if newstate == self.state {
            // No state change
            return None;
        }

        // Bus state has changed
        self.state = newstate;

        match self.state {
            AdbBusState::Idle => None,
            AdbBusState::Command => {
                if !self.cmd.is_empty() {
                    // Finish off a multiple byte command
                    self.process_cmd(true);
                    self.cmd.clear();
                }
                None
            }
            AdbBusState::Data1 | AdbBusState::Data2 => {
                if !self.cmd.is_empty() {
                    if self.process_cmd(false) {
                        self.cmd.clear();
                        // Command processed, response ready
                    } else {
                        // Wait for command to be clocked out
                        return None;
                    }
                }

                Some(self.response.pop_at(0).unwrap_or(0))
            }
        }
    }

    pub fn data_in(&mut self, data: u8) {
        if self.state == AdbBusState::Command && self.cmd.is_empty() {
            // New command, clear response buffer
            self.response.clear();
        }
        self.cmd.push(data);
    }

    /// Process a received ADB command.
    ///
    ///  * `finish` - set if the bus state transitioned out of data in/out.
    fn process_cmd(&mut self, finish: bool) -> bool {
        let address = self.cmd[0] >> 4;
        let cmd = (self.cmd[0] >> 2) & 3;
        let reg = self.cmd[0] & 3;

        self.response.clear();

        if cmd == 0 {
            // Reset (broadcast)
            for dev in &mut self.devices {
                dev.reset();
            }
            return true;
        }

        let Some(device) = self.devices.iter_mut().find(|d| d.get_address() == address) else {
            // No device at this address
            return true;
        };

        match cmd {
            // Flush
            0b01 => {
                device.flush();
            }
            // Listen
            0b10 => {
                if finish {
                    device.listen(reg, &self.cmd[1..]);
                } else {
                    // Delay until command is complete
                    return false;
                }
            }
            // Talk
            0b11 => {
                self.response = device.talk(reg);
            }
            _ => {
                log::error!(
                    "Unknown ADB command {:02X} for address {:02X}",
                    address,
                    cmd
                );
            }
        };
        true
    }
}

impl Debuggable for AdbTransceiver {
    fn get_debug_properties(&self) -> DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_enum, dbgprop_string, dbgprop_udec};

        let mut result = vec![
            dbgprop_enum!("State", self.state),
            dbgprop_udec!("Command buffer len", self.cmd.len()),
            dbgprop_string!("Command", hex::encode(&self.cmd)),
            dbgprop_udec!("Response buffer len", self.response.len()),
            dbgprop_string!("Response", hex::encode(&self.response)),
            dbgprop_bool!("Interrupt/status line", self.get_int()),
            dbgprop_bool!("Global SRQ", self.device_has_srq()),
        ];

        for d in &self.devices {
            result.push(dbgprop_bool!(
                format!("Device {} SRQ", d.get_address()),
                d.get_srq()
            ));
        }

        result
    }
}
