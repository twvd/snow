//! ADB transceiver of the SE and the Mac II family
//!
//! The transceiver is a small microcontroller between the VIA and the ADB.
//! It runs the ADB bus transactions on behalf of the CPU, which talks to it
//! with:
//! - the state lines ST0/ST1 (VIA PB4/PB5), set by the CPU,
//! - the VIA shift register (SR), clocked by the transceiver (CB1/CB2),
//!   one byte at a time in either direction. The VIA raises an SR interrupt
//!   when a byte is through,
//! - the ADB interrupt line (VIA PB3, active low), driven by the
//!   transceiver. It is not an interrupt to the CPU: the CPU reads it
//!   with each byte it gets from the transceiver, as a flag on that byte.
//!
//! The transceiver acts when the CPU changes the state. A normal Talk, to
//! read a register from a device, goes like this:
//!
//! ```text
//!   CPU                                transceiver
//!   state 3 (Idle)                     idle
//!   SR = command, state 0  -------->   clocks the command byte in
//!                          <--------   SR interrupt
//!   state 1                -------->   runs the Talk on the bus, then
//!                                      clocks the first data byte out,
//!                                      PB3 low if the device didn't reply
//!                          <--------   SR interrupt
//!   reads SR and PB3
//!   state 2                -------->   clocks the second data byte out,
//!                                      PB3 low if another device
//!                                      requested service (1)
//!                          <--------   SR interrupt
//!   reads SR and PB3
//!   state 1, 2, ...        -------->   clocks the next data byte out,
//!                                      PB3 low when there are no more
//!                          <--------   SR interrupt
//!   reads SR and PB3
//!   state 3 (Idle)         -------->   idle, autopoll of this Talk
//!                                      every 11 ms
//! ```
//!
//! (1) PB3 has a different meaning at each phase of the transfer. At reply
//! byte 1 (state 1), it means a timeout (no reply). At reply byte 2 (state 2)
//! it means SRQ, which will be described further down, but basically means
//! another device has something to say. On any byte past the end of the
//! reply, it also means there is no more data.
//!
//! A shortened version of Talk exists: the CPU drops back to idle right
//! after sending the command. This effectively tells the transceiver to
//! start the 11 ms autopoll on that device.
//!
//! When the transceiver does an "autopoll" (unrequested repeat of Talk)
//! it basically just re-sends the last Talk command on the bus, and *if*
//! the device got something to say (ie the device responds), then the
//! transceiver starts talking to the CPU unrequested. This is called
//! a Report. It starts by the transceiver writing a copy of the original
//! Talk command into the SR which triggers a new cycle:
//!
//! ```text
//!   CPU                                transceiver
//!   state 3 (Idle)                     Autopoll: runs the Talk on the bus
//!                                      The device replies: clock the command
//!                                      byte out into SR, PB3 low
//!                          <--------   SR interrupt
//!   reads SR and PB3
//!   state 1                -------->   clocks the first data byte out
//!                                      as for Talk, PB3 low here means that
//!                                      there really was no data (2)
//!                          <--------   SR interrupt
//!   reads SR and PB3
//!   state 2                -------->   clocks the second data byte out
//!                                      PB3 low means there was an SRQ (2)
//!                          <--------   SR interrupt
//!   reads SR and PB3
//!   state 3 (Idle)         -------->   idle, the autopoll goes on
//! ```
//!
//! (2) Again, the SRQ business... normally an autopoll that didn't get
//! data from a device just doesn't talk to the CPU at all.... *except*
//! if another device signaled an SRQ ! In this case, you get the above
//! machinery signaling the CPU with PB3 low at byte 1, telling you
//! that there really wasn't anything to read, and with PB3 low at byte 2:
//! but heh ! somebody else wants to talk.
//!
//! Typically the driver sets the autopoll to poll register 0 which will
//! only have data if the device has something to say (key press, mouse
//! movement).
//!
//! Autopoll only polls a single device (the last one Talk'd from). That's
//! where the SRQ mentioned earlier comes into play. So during whatever
//! command is on the bus, the device that wants to talk holds the bus low
//! for a moment after the stop bit of the command. The transceiver catches
//! that and uses it to signal an SRQ for that same command (PB3 low at
//! byte 2, or a Report if that command was an autopoll).
//!
//! The SRQ doesn't tell which device made it. The CPU sends a Talk to each
//! device in turn until one has data, and the autopoll then follows that
//! device.
//!
//! There is a race here: The transceiver trying to send a report and the
//! CPU trying to send a command at the same time.
//!
//! If the CPU starts a new command (state 0) before it reads the data of a
//! Report, the transceiver doesn't take the command and pulls PB3 low
//! again.
//!
//! A Listen (write to the device) uses the same mechanism, with the data
//! bytes going the other way. The transceiver collects them, and runs the
//! Listen on the bus only when the CPU goes back to the idle state:
//!
//! ```text
//!   CPU                                transceiver
//!   state 3 (Idle)                     idle
//!   SR = command, state 0  -------->   clocks the command byte in
//!                          <--------   SR interrupt
//!   SR = data, state 1     -------->   clocks the first data byte in
//!                          <--------   SR interrupt
//!   SR = data, state 2     -------->   clocks the second data byte in
//!                          <--------   SR interrupt
//!   state 3 (Idle)         -------->   runs the Listen on the bus
//! ```
//!
//! A SendReset or a Flush runs on the bus as soon as its command byte is
//! in.
//!
//! The transceiver is two state machines: The protocol machine and the
//! shifter.
//!
//! The protocol machine is the high level protocol, it reacts to changes
//! of the state lines from the VIA, to a byte in from the shifter (the
//! shifter machine completing its shift), to the end of a command on the
//! bus, and to its timers (the autopoll, the keepalive below).
//!
//! Its own state (`ProtocolState`) is:
//! - `Ready`: no command to run,
//! - `CommandIn`: a command byte is being clocked in,
//! - `TalkReceived`, `ListenReceived`: a command byte is in, but the
//!   command has not run on the bus yet,
//! - `ListenDataIn`: a Listen data byte is being clocked in,
//! - `OnBus`: the command is on the bus,
//! - `Report`: an autopoll got data or saw a service request, and the
//!   command byte has not been sent to the CPU yet,
//! - `ReportSent`: the command byte has been sent, and the CPU has not read
//!   the data yet.
//!
//! It also keeps two flags for the CPU, sent on PB3 with the data bytes:
//! - `timeout`: the last Talk got no reply, with the first data byte,
//! - `srq`: another device requested service during the last command,
//!   with the second data byte.
//!
//! The shifter clocks one byte through the VIA shift register when the
//! protocol machine starts it. The transceiver doesn't look at the state
//! lines while the shifter runs or a command is on the bus.
//!
//! When the CPU leaves the transceiver in the same state for 500 ms, it
//! sends the last byte to the VIA again, in case the CPU missed it.

use serde::{Deserialize, Serialize};

use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::mac::adb::{AdbDeviceResponse, AdbEvent};
use crate::tickable::Ticks;

use super::{AdbDevice, AdbDeviceInstance};

/// VIA ticks per ms
const MS: Ticks = 783_360 / 1000;

/// Time the transceiver takes to clock a byte through the VIA shift register,
/// in VIA ticks (~90 us)
const SHIFT_TIME: Ticks = 70;

/// Time on the bus of a command, before the reply of the device: attention,
/// sync, 8 command bits and the stop bit, then the stop-to-start time
/// (~1.9 ms)
const COMMAND_TIME: Ticks = 1500;

/// Time a device has to start its reply to a Talk, before it times out
/// (260 us)
const REPLY_TIMEOUT: Ticks = 204;

/// Time on the bus of a data byte (8 bits of 100 us)
const DATA_BYTE_TIME: Ticks = 8 * MS / 10;

/// Time the bus is held low for a SendReset (3 ms)
const RESET_TIME: Ticks = 3 * MS;

/// Time in the idle state before the first run of the autopoll, and
/// between two runs. With the time of the command on the bus, the autopoll
/// comes about every 11 ms.
const AUTOPOLL_TIME: Ticks = 4 * MS;

/// Time without a change of state after which the transceiver sends the last byte
/// to the VIA again
const KEEPALIVE_TIME: Ticks = 500 * MS;

/// ADB transaction states, from the state lines
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, Default, strum::IntoStaticStr, Serialize, Deserialize,
)]
enum AdbBusState {
    /// Start a new command - ST0 = 0, ST1 = 0
    Command,
    /// Transfer a data byte (even) - ST0 = 1, ST1 = 0
    Data1,
    /// Transfer a data byte (odd) - ST0 = 0, ST1 = 1
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

/// Direction of a byte through the VIA shift register
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
enum Shift {
    /// A byte from the VIA
    #[default]
    FromVia,
    /// A byte to the VIA
    ToVia(u8),
}

/// Shifter: clocks one byte through the VIA shift register, in either
/// direction. It runs when the protocol machine starts it, and the
/// protocol machine waits for it to be done.
#[derive(Default, Serialize, Deserialize)]
struct Shifter {
    /// Direction of the byte
    shift: Shift,
    /// VIA ticks until the byte is through, 0 when done
    time: Ticks,
    /// Last byte from the VIA
    byte_in: u8,
}

impl Shifter {
    fn start(&mut self, shift: Shift) {
        self.shift = shift;
        self.time = SHIFT_TIME;
    }

    fn busy(&self) -> bool {
        self.time > 0
    }

    fn byte_in(&self) -> u8 {
        self.byte_in
    }

    /// Runs for one VIA tick. When the byte is through, exchanges it with
    /// the VIA through `shift`.
    fn tick(&mut self, shift: &mut impl FnMut(u8) -> u8) {
        if self.time == 0 {
            return;
        }
        self.time -= 1;
        if self.time > 0 {
            return;
        }
        match self.shift {
            Shift::FromVia => self.byte_in = shift(0xFF),
            Shift::ToVia(byte) => {
                shift(byte);
            }
        }
    }
}

/// State of the protocol machine
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, Default, strum::IntoStaticStr, Serialize, Deserialize,
)]
enum ProtocolState {
    /// No command to run. In the idle state, the autopoll runs.
    #[default]
    Ready,
    /// Clocking a command byte in from the VIA
    CommandIn,
    /// A Talk is in, but has not run on the bus yet
    TalkReceived,
    /// A Listen is in, but has not run on the bus yet
    ListenReceived,
    /// Clocking a Listen data byte in from the VIA
    ListenDataIn,
    /// The command is on the bus, for that many more VIA ticks
    OnBus(Ticks),
    /// An autopoll got data or saw a service request, and the command byte
    /// has not been sent to the CPU yet
    Report,
    /// The command byte of a Report has been sent to the CPU, which has not
    /// read the data yet
    ReportSent,
}

/// Apple Desktop Bus transceiver
#[derive(Default, Serialize, Deserialize)]
pub struct AdbTransceiver {
    /// Devices on the ADB bus
    devices: Vec<AdbDeviceInstance>,

    /// State lines from the CPU
    state_lines: AdbBusState,
    /// State lines the protocol machine last acted on
    last_state_lines: AdbBusState,
    /// State of the protocol machine
    protocol_state: ProtocolState,
    /// Last command byte received from the VIA
    command: u8,
    /// Talk command of the autopoll
    autopoll: Option<u8>,
    /// Data of the last Talk from the device, or Listen data from the VIA
    input_buf: AdbDeviceResponse,
    /// Next byte of `input_buf` to send to the VIA
    input_pos: usize,
    /// The device didn't reply to the last Talk
    timeout: bool,
    /// Another device requested service during the last command
    srq: bool,
    /// ADB interrupt line asserted (PB3 low when self.int true), this is a bit
    /// weird, but "interrupt" is what Apple calls it even if it's not really
    /// wired as an interrupt.
    int: bool,
    /// Last byte through the VIA shift register, sent again by the keepalive
    last: u8,
    /// VIA ticks since the last run of the autopoll
    autopoll_time: Ticks,
    /// VIA ticks without a change of state from the CPU
    keepalive_time: Ticks,

    /// Shifter, run by the protocol machine
    shifter: Shifter,
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
    /// and ST1 from VIA (PB4/PB5 on all supported machines at the moment).
    /// When it has clocked a byte through the shift register, it calls `shift`
    /// with the byte it drives and gets the VIA's byte back.
    /// Returns the state of the ADB interrupt line to VIA (PB3) true when
    /// asserted (PB3 low).
    pub fn tick(&mut self, st0: bool, st1: bool, mut shift: impl FnMut(u8) -> u8) -> bool {
        self.state_lines = AdbBusState::from_io(st0, st1);
        self.shifter.tick(&mut shift);
        // While a byte is shifted, the transceiver does nothing else
        if !self.shifter.busy() {
            self.step();
        }
        self.int
    }

    /// Check if the the CPU changed the state lines. When they did change,
    /// latch the new value (last_state_line), clear int (raise PB3) and
    /// reset the keepalive timer (CPU is alive !)
    fn new_lines(&mut self) -> Option<AdbBusState> {
        if self.state_lines == self.last_state_lines {
            return None;
        }
        self.last_state_lines = self.state_lines;
        self.int = false;

        // The CPU is alive ! Reset the keepalive timer
        self.keepalive_time = 0;

        Some(self.state_lines)
    }

    /// Runs the protocol machine for one VIA tick
    fn step(&mut self) {
        use AdbBusState::{Command, Data1, Data2, Idle};
        use ProtocolState::*;

        match self.protocol_state {
            // We are talking to the ADB device. This simulates the time it
            // takes to do so, during that time, the transceiver does nothing
            // else. This includes the complete command and reply if any.
            OnBus(time) => {
                if time > 1 {
                    self.protocol_state = OnBus(time - 1);
                } else {
                    self.command_done();
                }
            }

            // We finished shifting a new command. This switches us to
            // TalkReceived, ListenReceived, or OnBus depending on the
            // command
            CommandIn => self.new_command(self.shifter.byte_in()),

            // Some Listen data has arrived, switch back to ListenReceived
            // to wait for a state line change. We don't use Ready for Listen
            // because of the different direction of transfer and because we
            // haven't actually run the command on the ADB bus yet (we latch
            // it all *then* run it).
            ListenDataIn => {
                self.listen_data(self.shifter.byte_in());
                self.protocol_state = ListenReceived;
            }

            // We are waiting for the CPU (either because we are in between
            // commands or between bytes of a command), check if the CPU changed
            // the lines compared to what we last latched and act accordingly.
            Ready => match self.new_lines() {
                // CPU sending a new command, start shifting (switch to CommandIn)
                Some(Command) => self.take_command(),
                Some(Data1) => self.send_first(),
                Some(Data2) => self.send_second(),
                Some(Idle) => (),
                None => self.autopoll(),
            },

            // We received a "Talk": we wait for the CPU to tell us what's next.
            //
            // The CPU can ask for data, in which case we send the command on
            // the bus, state changes to OnBus and later to Ready. Or the CPU
            // goes back to Idle (just configured our Autopoll) in which case
            // we go back to Ready and the autopoll will kick in later.
            TalkReceived => match self.new_lines() {
                Some(Command) => self.take_command(),
                Some(Data1) => {
                    self.autopoll = Some(self.command);
                    self.autopoll_time = 0;
                    self.start_command();
                }
                Some(Idle) => {
                    self.autopoll = Some(self.command);
                    self.autopoll_time = 0;
                    self.protocol_state = Ready;
                }
                // Protocol error: the CPU went straight to Data2, just send
                // whatever we have.
                Some(Data2) => {
                    log::warn!("ADB: state 2 before the Talk {:02X} ran", self.command);
                    self.send_second();
                }
                None => self.keepalive(),
            },

            // In Listen, wait for the CPU to tell us what's next. IE either we
            // start listening for the next data byte from the shifter or we
            // send the Listen command due to switching to Idle or a new command
            // getting in. Sending the Listen command switches us to OnBus. Once
            // it has completed, command_done() will re-check last_state_lines
            // to see if we have a new command.
            ListenReceived => match self.new_lines() {
                Some(Data1 | Data2) => {
                    self.protocol_state = ListenDataIn;
                    self.shifter.start(Shift::FromVia);
                }
                Some(Command | Idle) => self.start_command(),
                None => self.keepalive(),
            },

            // An autopoll got something: the command byte goes to the VIA
            // once the CPU is idle
            Report => match self.new_lines() {
                Some(Command) => self.refuse_command(),
                Some(Data1) => self.read_report(),
                Some(Data2) => self.send_second(),
                Some(Idle) => self.send_report(),
                None if self.last_state_lines == Idle => self.send_report(),
                None => self.keepalive(),
            },

            // Waiting for the CPU to read the data of the Report
            ReportSent => match self.new_lines() {
                Some(Command) => self.refuse_command(),
                Some(Data1) => self.read_report(),
                Some(Data2) => self.send_second(),
                Some(Idle) => (),
                None => self.keepalive(),
            },
        }
    }

    /// Clocks a new command byte in from the VIA
    fn take_command(&mut self) {
        self.protocol_state = ProtocolState::CommandIn;
        self.shifter.start(Shift::FromVia);
    }

    /// Sends the command byte of the autopoll to the VIA, even when the
    /// Report comes from another command
    fn send_report(&mut self) {
        self.protocol_state = ProtocolState::ReportSent;
        self.send(self.autopoll.unwrap_or(0));
    }

    /// The CPU reads the data of a Report, as in a Talk
    fn read_report(&mut self) {
        self.protocol_state = ProtocolState::Ready;
        self.send_first();
    }

    /// A new command while the CPU has not read a Report: the byte is
    /// clocked in but not taken, and PB3 tells the CPU to read the Report
    /// first
    fn refuse_command(&mut self) {
        self.int = true;
        self.last = 0xFF;
        self.protocol_state = ProtocolState::ReportSent;
        self.shifter.start(Shift::FromVia);
    }

    /// A command byte is in
    fn new_command(&mut self, byte: u8) {
        self.last = 0xFF;
        self.command = byte;
        self.input_buf.clear();
        self.input_pos = 0;
        match (byte >> 2) & 3 {
            // Talk
            0b11 => self.protocol_state = ProtocolState::TalkReceived,
            // Listen
            0b10 => self.protocol_state = ProtocolState::ListenReceived,
            // SendReset, Flush and the reserved commands run right away
            _ => self.start_command(),
        }
    }

    /// A Listen data byte is in
    fn listen_data(&mut self, byte: u8) {
        self.last = byte;
        if !self.input_buf.is_full() {
            self.input_buf.push(byte);
        }
    }

    fn send(&mut self, byte: u8) {
        self.last = byte;
        self.shifter.start(Shift::ToVia(byte));
    }

    /// Sends the next data byte to the VIA, asserting the interrupt line when
    /// there are no more
    fn send_data(&mut self) {
        if self.input_pos >= self.input_buf.len() {
            self.int = true;
        }
        let byte = self.input_buf.get(self.input_pos).copied().unwrap_or(0xFF);
        self.input_pos += 1;
        self.send(byte);
    }

    /// Sends a data byte in state 1, which tells whether the device timed
    /// out
    fn send_first(&mut self) {
        self.int = self.timeout;
        self.timeout = false;
        self.send_data();
    }

    /// Sends a data byte in state 2, which tells whether another device
    /// requested service
    fn send_second(&mut self) {
        self.int = self.srq;
        self.srq = false;
        self.send_data();
    }

    /// Sends the last byte to the VIA again after 500 ms in the same state,
    /// a safety net for an SR interrupt the CPU missed
    fn keepalive(&mut self) {
        self.keepalive_time += 1;
        if self.keepalive_time >= KEEPALIVE_TIME {
            self.keepalive_time = 0;
            self.int = false;
            self.send(self.last);
        }
    }

    /// Runs the autopoll every 11 ms in the idle state
    fn autopoll(&mut self) {
        self.keepalive();
        if self.shifter.busy() || self.last_state_lines != AdbBusState::Idle {
            return;
        }
        let Some(autopoll) = self.autopoll else {
            return;
        };
        self.autopoll_time += 1;
        if self.autopoll_time >= AUTOPOLL_TIME {
            self.autopoll_time = 0;
            self.command = autopoll;
            self.start_command();
        }
    }

    /// Puts the command on the bus, for the time it takes there
    fn start_command(&mut self) {
        let address = self.command >> 4;
        let reg = self.command & 3;
        let time = match (self.command >> 2) & 3 {
            // SendReset
            0b00 if self.command & 0b11 == 0 => RESET_TIME,
            // Talk
            0b11 => {
                self.input_buf = self
                    .devices
                    .iter_mut()
                    .find(|d| d.get_address() == address)
                    .map(|d| d.talk(reg))
                    .unwrap_or_default();
                self.input_pos = 0;
                if self.input_buf.is_empty() {
                    COMMAND_TIME + REPLY_TIMEOUT
                } else {
                    COMMAND_TIME + self.input_buf.len() as Ticks * DATA_BYTE_TIME
                }
            }
            // Listen with its data, Flush and the reserved commands without
            _ => COMMAND_TIME + self.input_buf.len() as Ticks * DATA_BYTE_TIME,
        };
        self.protocol_state = ProtocolState::OnBus(time);
    }

    /// The command has been on the bus: carries it out
    fn command_done(&mut self) {
        self.protocol_state = ProtocolState::Ready;
        let address = self.command >> 4;
        let reg = self.command & 3;
        self.srq = self
            .devices
            .iter()
            .any(|d| d.get_address() != address && d.get_srq());

        let device = self.devices.iter_mut().find(|d| d.get_address() == address);
        match (self.command >> 2) & 3 {
            // SendReset
            0b00 if self.command & 0b11 == 0 => {
                for dev in &mut self.devices {
                    dev.reset();
                }
                // The transceiver restarts too, and forgets everything. It
                // starts from the idle state, so it sees the state the CPU
                // is in now as a change.
                let devices = std::mem::take(&mut self.devices);
                *self = Self {
                    devices,
                    ..Default::default()
                };
            }
            // Flush
            0b00 => {
                if let Some(dev) = device {
                    dev.flush();
                }
                // No data: the CPU reads a timeout. A service request makes
                // a Report.
                self.timeout = true;
                if self.srq {
                    self.protocol_state = ProtocolState::Report;
                    self.int = true;
                }
            }
            // Listen
            0b10 => {
                // A register takes 2 to 8 bytes: a device ignores fewer
                if self.input_buf.len() < 2 {
                    log::warn!(
                        "ADB: short Listen {:02X} with {} bytes",
                        self.command,
                        self.input_buf.len()
                    );
                } else if let Some(dev) = device {
                    dev.listen(reg, &self.input_buf);
                }
                // A service request during a Listen is not passed on
                self.srq = false;
                self.timeout = false;
                if self.last_state_lines == AdbBusState::Command {
                    // The CPU went straight from the data bytes to a new
                    // command, without going back to idle: the Listen ran
                    // first, now take the new command
                    self.take_command();
                }
            }
            // Talk
            0b11 => {
                // A single byte is no reply either. On a timeout, the CPU
                // reads two bytes of $FF.
                self.timeout = self.input_buf.len() < 2;
                if self.timeout {
                    self.input_buf.clear();
                    self.input_buf.extend([0xFF, 0xFF]);
                }
                self.input_pos = 0;
                if self.last_state_lines == AdbBusState::Data1 {
                    // The CPU set state 1 after the command byte, and is
                    // waiting for the first byte
                    self.send_first();
                } else if !self.timeout || self.srq {
                    // Autopoll, run in the idle state: tell the CPU
                    self.protocol_state = ProtocolState::Report;
                    self.int = true;
                }
            }
            // Reserved commands
            _ => (),
        }
    }
}

impl Debuggable for AdbTransceiver {
    fn get_debug_properties(&self) -> DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_enum, dbgprop_string, dbgprop_udec};

        let mut result = vec![
            dbgprop_enum!("State lines", self.last_state_lines),
            dbgprop_enum!("Protocol state", self.protocol_state),
            dbgprop_udec!("Command", self.command),
            dbgprop_string!("Buffer", hex::encode(&self.input_buf)),
            dbgprop_bool!("Interrupt line", self.int),
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
