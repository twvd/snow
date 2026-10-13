//! Egret microcontroller, as connected to VIA 1
//!
//! The host and the Egret exchange packets through the shift register of the
//! VIA, which the Egret clocks, with a handshake on three lines of port B.
//!
//! This is a high level model: bytes are shifted at once and the timings are
//! approximate. The Egret also holds the reset line of the CPU at power on,
//! which is not modelled.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::debuggable::Debuggable;
use crate::mac::egret_common::EgretCommon;
use crate::mac::rtc::Rtc;
use crate::tickable::Ticks;

/// Time to clock a byte through the shift register, in VIA clock cycles.
/// The firmware takes 18 cycles of its own clock for a bit.
const BYTE_TICKS: Ticks = 54;

/// Time from the end of a command packet to the response
const RESPONSE_TICKS: Ticks = 160;

/// Time from the end of a transfer to the start of a packet of the Egret's
/// own, which the firmware goes back to its main loop to send
const REST_TICKS: Ticks = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, strum::IntoStaticStr)]
enum State {
    Idle,
    /// After a transfer: the session stays negated for the host to see it
    Rest(Ticks),
    /// Command packet: waiting for the host to load a byte
    RxWait,
    /// Command packet: clocking a byte
    RxShift(Ticks),
    /// Command packet: waiting for the host to negate VIA full
    RxAck,
    /// Preparing the response packet
    TxDelay(Ticks),
    /// Response packet: clocking the attention byte
    TxAttention(Ticks),
    /// Response packet: waiting for the host to negate the system session,
    /// which it has asserted if it started a command packet at the same time
    TxCollision,
    /// Response packet: waiting for the host to assert the system session
    TxSession,
    /// Response packet: waiting for the host to negate VIA full
    TxWait,
    /// Response packet: clocking a byte
    TxShift(Ticks),
    /// Response packet: waiting for the host to assert VIA full
    TxAck,
    /// Response packet: waiting for the host to negate the system session
    TxDone,
}

#[derive(Serialize, Deserialize)]
pub struct Egret {
    common: EgretCommon,
    state: State,
    rx: Vec<u8>,
    tx: VecDeque<u8>,

    /// Transceiver session line (true = asserted, which is low on the pin)
    xcvr_ses: bool,
}

impl Default for Egret {
    fn default() -> Self {
        Self {
            common: EgretCommon::default(),
            state: State::Idle,
            rx: vec![],
            tx: VecDeque::new(),
            xcvr_ses: false,
        }
    }
}

impl Egret {
    /// Transceiver session line (true = asserted, which is low on the pin)
    pub fn xcvr_ses(&self) -> bool {
        self.xcvr_ses
    }

    pub fn rtc(&self) -> &Rtc {
        &self.common.rtc
    }

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.common.rtc
    }

    /// Advances by a number of VIA clock cycles.
    ///
    /// `sys_ses` and `via_full` are the lines from the host (true = asserted).
    /// `shift` clocks a byte through the shift register of the VIA.
    pub fn tick(
        &mut self,
        ticks: Ticks,
        sys_ses: bool,
        via_full: bool,
        mut shift: impl FnMut(u8) -> u8,
    ) {
        self.common.tick(ticks);

        self.state = match self.state {
            // The host asserts the system session to start a command packet
            State::Idle if sys_ses => {
                self.rx.clear();
                State::RxWait
            }

            // The host may start a command packet at any time
            State::Rest(_) if sys_ses => {
                self.rx.clear();
                State::RxWait
            }
            State::Rest(t) if t > ticks => State::Rest(t - ticks),
            State::Rest(_) => State::Idle,

            // The Egret starts a transfer by itself when it has a packet to send
            State::Idle => match self.common.packet() {
                Some(packet) => {
                    self.tx = packet.into();
                    self.xcvr_ses = true;
                    State::TxAttention(BYTE_TICKS)
                }
                None => State::Idle,
            },

            // The host negates the system session at the end of the command packet
            State::RxWait | State::RxAck if !sys_ses => self.command(),

            // The host has loaded a byte and asserts VIA full
            State::RxWait if via_full => State::RxShift(BYTE_TICKS),

            // The byte is still being clocked in
            State::RxShift(t) if t > ticks => State::RxShift(t - ticks),

            // The byte is clocked in
            State::RxShift(_) => {
                self.rx.push(shift(0xFF));
                State::RxAck
            }

            // The host negates VIA full, ready to load the next byte
            State::RxAck if !via_full => State::RxWait,

            // The response is still being prepared
            State::TxDelay(t) if t > ticks => State::TxDelay(t - ticks),

            // The response is ready
            State::TxDelay(_) => {
                self.xcvr_ses = true;
                State::TxAttention(BYTE_TICKS)
            }

            // The attention byte is still being clocked
            State::TxAttention(t) if t > ticks => State::TxAttention(t - ticks),

            // The attention byte is clocked
            State::TxAttention(_) => {
                // The data line is not driven for the attention byte
                shift(0xFF);
                State::TxCollision
            }

            // The host may have started a command packet at the same time. It
            // has then asserted the system session and loaded its first byte,
            // which the attention byte has clocked out. When it sees the
            // transceiver session, it gives up its packet and negates the
            // system session, then asserts it again to read this packet.
            // Waiting for the negation first keeps the session of the packet
            // of the host from being taken for the one of this packet.
            State::TxCollision if !sys_ses => State::TxSession,

            // The host asserts the system session to read the packet
            State::TxSession if sys_ses => State::TxWait,

            // The host negates the system session to end the transfer
            State::TxWait | State::TxAck if !sys_ses => {
                self.tx.clear();
                self.xcvr_ses = false;
                State::Rest(REST_TICKS)
            }

            // The host negates VIA full, ready for the next byte
            State::TxWait if !via_full => {
                // A response is a packet, or goes on until the host ends it.
                // The session ends before the last byte of a packet is clocked.
                if self.tx.len() == 1 && !self.common.response_continues() {
                    self.xcvr_ses = false;
                }
                State::TxShift(BYTE_TICKS)
            }

            // The byte is still being clocked out
            State::TxShift(t) if t > ticks => State::TxShift(t - ticks),

            // The byte is clocked out
            State::TxShift(_) => {
                let byte = self.tx.pop_front().or_else(|| self.common.response_next());
                shift(byte.unwrap_or(0xFF));
                if self.tx.is_empty() && !self.common.response_continues() {
                    State::TxDone
                } else {
                    State::TxAck
                }
            }

            // The host asserts VIA full when it has read the byte
            State::TxAck if via_full => State::TxWait,

            // The host negates the system session after the last byte
            State::TxDone if !sys_ses => State::Rest(REST_TICKS),

            // Waiting for the host
            state => state,
        };
    }

    /// End of a command packet
    fn command(&mut self) -> State {
        if self.rx.is_empty() {
            return State::Idle;
        }
        self.tx = self.common.command(&self.rx).into();
        State::TxDelay(RESPONSE_TICKS)
    }
}

impl Debuggable for Egret {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_enum, dbgprop_udec};

        vec![
            dbgprop_enum!("State", self.state),
            dbgprop_bool!("Transceiver session", self.xcvr_ses),
            dbgprop_udec!("Command bytes", self.rx.len()),
            dbgprop_udec!("Response bytes", self.tx.len()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Host side of the VIA
    #[derive(Default)]
    struct Host {
        egret: Egret,
        sys_ses: bool,
        via_full: bool,
        sr: u8,
        shifted: bool,
    }

    impl Host {
        fn run(&mut self, ticks: Ticks) {
            for _ in 0..ticks {
                let (sr, shifted) = (&mut self.sr, &mut self.shifted);
                self.egret.tick(1, self.sys_ses, self.via_full, |input| {
                    *shifted = true;
                    std::mem::replace(sr, input)
                });
            }
        }

        fn wait_shift(&mut self) -> u8 {
            self.run(BYTE_TICKS + RESPONSE_TICKS + 10);
            assert!(std::mem::take(&mut self.shifted), "{:?}", self.egret.state);
            self.sr
        }

        fn send(&mut self, packet: &[u8]) {
            assert!(!self.egret.xcvr_ses());
            self.sys_ses = true;
            for &b in packet {
                self.sr = b;
                self.via_full = true;
                self.wait_shift();
                self.via_full = false;
                self.run(10);
            }
            self.sys_ses = false;
        }

        fn receive(&mut self) -> Vec<u8> {
            // Attention byte
            self.wait_shift();
            assert!(self.egret.xcvr_ses());
            self.sys_ses = true;

            let mut packet = vec![];
            loop {
                packet.push(self.wait_shift());
                self.via_full = true;
                self.run(10);
                self.via_full = false;
                if !self.egret.xcvr_ses() {
                    break;
                }
            }
            self.sys_ses = false;
            self.run(10);
            assert!(matches!(self.egret.state, State::Rest(_)));
            packet
        }
    }

    #[test]
    fn command_response() {
        let mut host = Host::default();

        host.send(&[0x01, 0x01, 0x00]);
        assert_eq!(host.receive(), [0x01, 0x00, 0x01]);

        // An error response has one more byte
        host.send(&[0x01, 0xFE]);
        assert_eq!(host.receive(), [0x02, 0x02, 0x01, 0xFE]);
    }

    #[test]
    fn response_until_host_ends() {
        let mut host = Host::default();

        host.send(&[0x01, 0x07, 0x00, 0xF9]);
        host.wait_shift();
        host.sys_ses = true;
        for expected in [0x01, 0x00, 0x07, 0x00, 0x00] {
            assert_eq!(host.wait_shift(), expected);
            assert!(host.egret.xcvr_ses());
            host.via_full = true;
            host.run(10);
            host.via_full = false;
        }
        host.sys_ses = false;
        host.run(10);
        assert!(!host.egret.xcvr_ses());
        assert!(matches!(host.egret.state, State::Rest(_)));

        host.send(&[0x01, 0x01, 0x00]);
        assert_eq!(host.receive(), [0x01, 0x00, 0x01]);
    }

    #[test]
    fn host_ends_response() {
        let mut host = Host::default();

        host.send(&[0x01, 0xFE]);
        host.wait_shift();
        host.sys_ses = true;
        host.wait_shift();
        host.sys_ses = false;
        host.run(10);
        assert!(!host.egret.xcvr_ses());
        assert!(matches!(host.egret.state, State::Rest(_)));
    }
}
