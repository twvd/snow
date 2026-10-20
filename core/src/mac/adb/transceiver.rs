use serde::{Deserialize, Serialize};

use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::mac::adb::{AdbDeviceResponse, AdbEvent};
use crate::tickable::Ticks;

use super::{AdbDevice, AdbDeviceInstance};

/// Time to shift a byte through the VIA shift register, in VIA ticks (3 ms)
const SHIFT_DELAY: Ticks = 783_360 * 3 / 1000;

/// ADB Bus/transceiver states
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, Default, strum::IntoStaticStr, Serialize, Deserialize,
)]
enum AdbBusState {
    /// Send command - ST0 = 0, ST1 = 0
    Command,
    /// Send/receive data from ADB device - ST0 = 1, ST1 = 0
    Data1,
    /// Send/receive data from ADB device - ST0 = 0, ST1 = 1
    Data2,
    /// Idle - ST0 = 1, ST1 = 1
    #[default]
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

/// Apple Desktop Bus transceiver
#[derive(Default, Serialize, Deserialize)]
pub struct AdbTransceiver {
    /// Current bus state
    state: AdbBusState,

    /// Devices on the ADB bus
    devices: Vec<AdbDeviceInstance>,

    /// Response that is currently being clocked out
    response: AdbDeviceResponse,

    /// Command data being clocked in
    cmd: Vec<u8>,

    /// VIA ticks until the byte being shifted is through
    shift_time: Ticks,
    /// Byte to shift into the VIA
    shift_in: u8,
}

impl AdbTransceiver {
    pub fn event(&mut self, event: &AdbEvent) {
        for d in &mut self.devices {
            d.event(event);
        }
    }

    pub fn add_device<T>(&mut self, device: T)
    where
        T: AdbDevice + Send + 'static,
    {
        self.devices.push(Box::new(device));
    }

    /// Runs the transceiver for one VIA tick, with the ADB state lines ST0
    /// and ST1 from VIA PB4/PB5. When it has clocked a byte through the shift
    /// register, it calls `shift` with the byte it drives and gets the VIA's
    /// byte back. Returns the state of the ADB interrupt line to VIA PB3:
    /// true when asserted (PB3 low).
    pub fn tick(&mut self, st0: bool, st1: bool, mut shift: impl FnMut(u8) -> u8) -> bool {
        let state = self.state;
        if let Some(b) = self.io(st0, st1) {
            self.shift_in = b;
        }
        if self.state != state {
            // A new state starts the shift of a byte
            self.shift_time = SHIFT_DELAY;
        }

        if self.shift_time > 0 {
            self.shift_time -= 1;
            if self.shift_time == 0 {
                let listen_data = matches!(self.state, AdbBusState::Data1 | AdbBusState::Data2)
                    && self.is_listen();
                if self.state == AdbBusState::Command || listen_data {
                    // A command or Listen data from the VIA
                    let data = shift(0);
                    self.data_in(data);
                } else {
                    shift(self.shift_in);
                    self.shift_time = SHIFT_DELAY;
                }
            }
        }

        self.get_int()
    }

    /// The command being clocked in is a Listen
    fn is_listen(&self) -> bool {
        self.cmd.first().is_some_and(|c| (c >> 2) & 3 == 0b10)
    }

    fn get_int(&self) -> bool {
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

    fn io(&mut self, st0: bool, st1: bool) -> Option<u8> {
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

    fn data_in(&mut self, data: u8) {
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
