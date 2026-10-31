//! Cuda microcontroller, as connected to VIA 1
//!
//! The Cuda is a 68HC05 between the CPU and the ADB, which also has the clock,
//! the PRAM, the IIC bus of the sound chip and the reset line of the CPU. The
//! CPU talks to it in packets, with:
//! - the VIA shift register (SR), always clocked by the Cuda (CB1/CB2), one
//!   byte at a time in either direction. The VIA raises an SR interrupt when
//!   a byte is through,
//! - TIP, transfer in progress (VIA PB5, active low), set by the CPU for as
//!   long as a transfer goes on, whichever way the packet goes,
//! - TACK, byte acknowledge (VIA PB4, active low), which the CPU toggles for
//!   each byte: it is the change that counts, not the level,
//! - TREQ, transfer request (VIA PB3, active low), driven by the Cuda when it
//!   has something to send.
//!
//! This is a high level model of firmware 2.37 (341S0788): bytes are shifted
//! at once and the timings are approximate. The commands are in EgretCommon.
//!
//! A command packet from the CPU. The level TACK is compared with starts low,
//! so with TACK high the first byte goes at once:
//!
//! ```text
//!   CPU                                Cuda
//!   SR = byte 1, TIP low   -------->   clocks byte 1 in
//!                          <--------   SR interrupt
//!   SR = byte 2, toggle TACK ------>   clocks byte 2 in
//!                          <--------   SR interrupt
//!   ...
//!   TIP high, TACK high    -------->   runs the command
//! ```
//!
//! No idle byte follows here. The response comes as a packet of the Cuda, as
//! do the packets it sends by itself, for ADB autopoll and the one second tick:
//!
//! ```text
//!   CPU                                Cuda
//!                          <--------   TREQ low, clocks an attention byte out
//!                          <--------   SR interrupt
//!   reads SR, TIP low      -------->   clocks byte 1 out
//!                          <--------   SR interrupt
//!   reads SR, toggle TACK  -------->   clocks byte 2 out
//!                          <--------   SR interrupt
//!   ...
//!   reads SR, toggle TACK  -------->   TREQ high, clocks the last byte out
//!                          <--------   SR interrupt
//!   sees TREQ high: that was the last one
//!   reads SR, TIP high, TACK high ->   clocks an idle byte (1)
//!                          <--------   SR interrupt
//! ```
//!
//! (1) The CPU waits for TREQ high and this interrupt before anything else.
//!
//! Some responses have no end, such as a read of PRAM: the Cuda keeps TREQ low
//! and sends bytes for as long as TACK toggles. The CPU ends it by negating TIP
//! right after its last toggle, which the firmware sees along with the toggle,
//! so no further byte is clocked. It then negates TREQ and clocks the idle byte:
//!
//! ```text
//!   CPU                                Cuda
//!   reads SR, toggle TACK,
//!   TIP high, TACK high    -------->   TREQ high, clocks an idle byte
//!                          <--------   SR interrupt
//!   waits for TREQ high and the SR interrupt, reads SR
//! ```
//!
//! When both start at once, the CPU finds TREQ low at the SR interrupt of its
//! first byte, which went out as the attention byte went in:
//!
//! ```text
//!   CPU                                Cuda
//!   SR = byte 1, TIP low   -------->   (TREQ low) clocks the attention byte,
//!                                      sampling TIP before the last clock
//!                          <--------   SR interrupt
//!   sees TREQ low, gives up:
//!   reads SR, TIP high     -------->   clocks the attention byte again
//!                          <--------   SR interrupt
//!   goes on as for any packet of the Cuda
//! ```
//!
//! To synchronize, at start up, the CPU asserts TACK without TIP. The firmware
//! checks for this in all its waits, and it also turns autopoll and the one
//! second packets off:
//!
//! ```text
//!   CPU                                Cuda
//!   TACK low, TIP high     -------->   TREQ low, clocks a byte
//!                          <--------   SR interrupt
//!   TACK high              -------->   TREQ high, clocks a byte
//!                          <--------   SR interrupt
//! ```

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::debuggable::Debuggable;
use crate::mac::adb::{AdbDevice, AdbEvent};
use crate::mac::egret_common::{EgretCommon, McuKind};
use crate::mac::rtc::Rtc;
use crate::tickable::Ticks;

/// Time to clock a byte through the shift register, in VIA clock cycles
const BYTE_TICKS: Ticks = 54;

/// Time from the end of a command packet to the response
const RESPONSE_TICKS: Ticks = 160;

/// Time from the end of a transfer to the start of a packet of the Cuda's own
const REST_TICKS: Ticks = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, strum::IntoStaticStr)]
enum State {
    Idle,
    /// After a transfer, before looking at the host again
    Rest(Ticks),
    /// Command packet: waiting for the host to toggle TACK
    RxWait,
    /// Command packet: clocking a byte
    RxShift(Ticks),
    /// Preparing the response packet
    TxDelay(Ticks),
    /// Response packet: clocking the attention byte
    TxAttention(Ticks),
    /// The host had started a command packet at the same time: waiting for
    /// it to give up, before clocking the attention byte again
    TxCollision,
    /// Response packet: waiting for the host to assert TIP
    TxSession,
    /// Response packet: waiting for the host to toggle TACK
    TxWait,
    /// Response packet: clocking a byte
    TxShift(Ticks),
    /// Response packet: waiting for the host to negate TIP
    TxDone,
    /// End of a transfer: clocking an idle byte for the host to see it
    IdleAck(Ticks),
    /// Sync with the host: clocking a byte
    SyncStart(Ticks),
    /// Sync with the host: waiting for it to negate TACK
    SyncWait,
    /// Sync with the host: clocking a byte
    SyncEnd(Ticks),
}

#[derive(Serialize, Deserialize)]
pub struct Cuda {
    common: EgretCommon,
    state: State,
    rx: Vec<u8>,
    tx: VecDeque<u8>,

    /// TREQ line (true = asserted, which is low on the pin)
    treq: bool,

    /// TACK at the last byte (true = asserted): a change asks for the next one.
    /// It starts as asserted, so the first byte goes as soon as TIP is.
    last_tack: bool,
}

impl Default for Cuda {
    fn default() -> Self {
        Self {
            common: EgretCommon::new(McuKind::Cuda),
            state: State::Idle,
            rx: vec![],
            tx: VecDeque::new(),
            treq: false,
            last_tack: true,
        }
    }
}

impl Cuda {
    /// TREQ line (true = asserted, which is low on the pin)
    pub fn treq(&self) -> bool {
        self.treq
    }

    pub fn adb_add_device<T>(&mut self, device: T)
    where
        T: AdbDevice + Send + 'static,
    {
        self.common.adb_add_device(device);
    }

    pub fn adb_event(&mut self, event: &AdbEvent) {
        self.common.adb_event(event);
    }

    pub fn rtc(&self) -> &Rtc {
        &self.common.rtc
    }

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.common.rtc
    }

    /// Returns whether the host has asked for a reset of the whole machine
    pub fn take_reset_request(&mut self) -> bool {
        self.common.take_reset_request()
    }

    /// Back to the start of the firmware, keeping the clock, PRAM and ADB
    pub fn reset(&mut self) {
        self.common.warm_start();
        self.state = State::Idle;
        self.rx.clear();
        self.tx.clear();
        self.treq = false;
        self.last_tack = true;
    }

    /// Advances by a number of VIA clock cycles.
    ///
    /// `tip` and `tack` are the lines from the host (true = asserted).
    /// `shift` clocks a byte through the shift register of the VIA.
    pub fn tick(&mut self, ticks: Ticks, tip: bool, tack: bool, mut shift: impl FnMut(u8) -> u8) {
        self.common.tick(ticks);

        self.state = match self.state {
            // TACK without TIP asks to synchronize, in any of the waits
            State::Idle
            | State::Rest(_)
            | State::RxWait
            | State::TxCollision
            | State::TxSession
            | State::TxWait
            | State::TxDone
                if tack && !tip =>
            {
                self.treq = true;
                self.tx.clear();
                State::SyncStart(BYTE_TICKS)
            }

            // The CPU asserts TIP to start a command packet, with TACK negated:
            // that differs from the level TACK is compared with, so the first
            // byte is clocked at once
            State::Idle | State::Rest(_) if tip => {
                self.rx.clear();
                self.last_tack = true;
                State::RxWait
            }

            // Back to looking for work after a transfer
            State::Rest(t) if t > ticks => State::Rest(t - ticks),
            State::Rest(_) => State::Idle,

            // Autopoll data or a one second tick, sent without a command
            State::Idle => match self.common.packet() {
                Some(packet) => {
                    self.tx = packet.into();
                    self.treq = true;
                    State::TxAttention(BYTE_TICKS)
                }
                None => State::Idle,
            },

            // The CPU negates TIP at the end of the command packet
            State::RxWait if !tip => self.command(),

            // A change of TACK: the next byte is in the SR
            State::RxWait if tack != self.last_tack => {
                self.last_tack = tack;
                State::RxShift(BYTE_TICKS)
            }

            // The byte is still being clocked in
            State::RxShift(t) if t > ticks => State::RxShift(t - ticks),

            // The byte is clocked in
            State::RxShift(_) => {
                self.rx.push(shift(0xFF));
                State::RxWait
            }

            // The response is still being prepared
            State::TxDelay(t) if t > ticks => State::TxDelay(t - ticks),

            // The response is ready
            State::TxDelay(_) => {
                self.treq = true;
                State::TxAttention(BYTE_TICKS)
            }

            // The attention byte is still being clocked
            State::TxAttention(t) if t > ticks => State::TxAttention(t - ticks),

            // The attention byte is clocked, with the data line not driven.
            // TIP already low means that the CPU started a command packet.
            State::TxAttention(_) => {
                shift(0xFF);
                if tip {
                    State::TxCollision
                } else {
                    State::TxSession
                }
            }

            // The CPU has given up its packet: the attention byte again
            State::TxCollision if !tip => State::TxAttention(BYTE_TICKS),

            // The CPU asserts TIP to read the packet, with TACK negated, which
            // asks for the first byte as for a command packet
            State::TxSession if tip => {
                self.last_tack = true;
                State::TxWait
            }

            // The CPU negates TIP to end the transfer, in a response with no
            // end or early in any other
            State::TxWait if !tip => {
                self.treq = false;
                self.tx.clear();
                State::IdleAck(BYTE_TICKS)
            }

            // A change of TACK asks for the next byte. TREQ goes before the
            // last byte of a packet is clocked.
            State::TxWait if tack != self.last_tack => {
                self.last_tack = tack;
                if self.tx.len() == 1 && !self.common.response_continues() {
                    self.treq = false;
                }
                State::TxShift(BYTE_TICKS)
            }

            // The CPU negated TIP right after asking for a byte: no byte comes,
            // as the firmware sees the two together
            State::TxShift(_) if !tip => {
                self.treq = false;
                self.tx.clear();
                State::IdleAck(BYTE_TICKS)
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
                    State::TxWait
                }
            }

            // The CPU negates TIP after the last byte
            State::TxDone if !tip => State::IdleAck(BYTE_TICKS),

            // The idle byte is still being clocked
            State::IdleAck(t) if t > ticks => State::IdleAck(t - ticks),

            // The idle byte is clocked. If the CPU has asserted TIP by now, the
            // rest of the transfer picks it up.
            State::IdleAck(_) => {
                shift(0xFF);
                State::Rest(REST_TICKS)
            }

            // The first sync byte is still being clocked
            State::SyncStart(t) if t > ticks => State::SyncStart(t - ticks),

            // The first sync byte is clocked
            State::SyncStart(_) => {
                shift(0xFF);
                State::SyncWait
            }

            // The CPU negates TACK to finish the sync
            State::SyncWait if !tack => {
                self.treq = false;
                State::SyncEnd(BYTE_TICKS)
            }

            // The second sync byte is still being clocked
            State::SyncEnd(t) if t > ticks => State::SyncEnd(t - ticks),

            // The second sync byte is clocked, and the sync is done
            State::SyncEnd(_) => {
                shift(0xFF);
                self.common.warm_start();
                State::Rest(REST_TICKS)
            }

            // Waiting for the CPU
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

impl Debuggable for Cuda {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_enum, dbgprop_udec};

        vec![
            dbgprop_enum!("State", self.state),
            dbgprop_bool!("TREQ", self.treq),
            dbgprop_udec!("Command bytes", self.rx.len()),
            dbgprop_udec!("Response bytes", self.tx.len()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Host side of the VIA, written from the sequences of the Linux driver
    #[derive(Default)]
    struct Host {
        cuda: Cuda,
        tip: bool,
        tack: bool,
        sr: u8,
        shifted: bool,
    }

    impl Host {
        fn run(&mut self, ticks: Ticks) {
            for _ in 0..ticks {
                let (sr, shifted) = (&mut self.sr, &mut self.shifted);
                self.cuda.tick(1, self.tip, self.tack, |input| {
                    *shifted = true;
                    std::mem::replace(sr, input)
                });
            }
        }

        fn wait_shift(&mut self) -> u8 {
            for _ in 0..BYTE_TICKS + RESPONSE_TICKS + REST_TICKS + 10 {
                self.run(1);
                if std::mem::take(&mut self.shifted) {
                    return self.sr;
                }
            }
            panic!("No shift: {:?}", self.cuda.state);
        }

        fn send(&mut self, packet: &[u8]) {
            assert!(!self.cuda.treq());
            self.sr = packet[0];
            self.tip = true;
            self.wait_shift();
            for &b in &packet[1..] {
                self.sr = b;
                self.tack = !self.tack;
                self.wait_shift();
            }
            self.tip = false;
            self.tack = false;
        }

        fn receive(&mut self) -> Vec<u8> {
            // Attention byte
            self.wait_shift();
            assert!(self.cuda.treq());
            self.tip = true;

            let mut packet = vec![];
            loop {
                let byte = self.wait_shift();
                packet.push(byte);
                if !self.cuda.treq() {
                    break;
                }
                self.tack = !self.tack;
            }
            self.tip = false;
            self.tack = false;
            // The idle byte
            self.wait_shift();
            assert!(matches!(self.cuda.state, State::Rest(_)));
            packet
        }
    }

    #[test]
    fn sync() {
        let mut host = Host::default();

        host.tack = true;
        host.wait_shift();
        assert!(host.cuda.treq());
        host.tack = false;
        host.wait_shift();
        assert!(!host.cuda.treq());
    }

    #[test]
    fn command_response() {
        let mut host = Host::default();

        host.send(&[0x01, 0x01, 0x00]);
        assert_eq!(host.receive(), [0x01, 0x00, 0x01]);

        host.send(&[0x01, 0xFE]);
        assert_eq!(host.receive(), [0x02, 0x02, 0x01, 0xFE]);
    }

    #[test]
    fn read_until_host_ends() {
        let mut host = Host::default();

        host.send(&[0x01, 0x07, 0x00, 0xF9]);
        host.wait_shift();
        host.tip = true;
        for expected in [0x01, 0x00, 0x07, 0x00, 0x00] {
            assert_eq!(host.wait_shift(), expected);
            assert!(host.cuda.treq());
            host.tack = !host.tack;
        }
        host.tip = false;
        host.tack = false;
        host.wait_shift();
        assert!(!host.cuda.treq());
        assert!(matches!(host.cuda.state, State::Rest(_)));

        host.send(&[0x01, 0x01, 0x00]);
        assert_eq!(host.receive(), [0x01, 0x00, 0x01]);
    }

    /// The sequence of the LC 475 ROM: TIP goes right after the last toggle
    #[test]
    fn read_ended_with_toggle() {
        let mut host = Host::default();

        host.send(&[0x01, 0x07, 0x00, 0xF9]);
        host.wait_shift();
        host.tip = true;
        for expected in [0x01, 0x00, 0x07, 0x00] {
            assert_eq!(host.wait_shift(), expected);
            host.tack = !host.tack;
        }
        host.tip = false;
        host.tack = false;
        host.run(1);
        assert!(!host.cuda.treq());
        host.wait_shift();
        assert!(matches!(host.cuda.state, State::Rest(_)));

        host.send(&[0x01, 0x01, 0x00]);
        assert_eq!(host.receive(), [0x01, 0x00, 0x01]);
    }

    #[test]
    fn collision() {
        let mut host = Host::default();

        host.send(&[0x01, 0x1B, 0x03]);
        host.receive();
        while !host.cuda.treq() {
            host.run(1);
        }
        host.receive();

        // The host starts a command packet while the Cuda clocks the
        // attention byte of a tick packet
        while !host.cuda.treq() {
            host.run(1);
        }
        host.shifted = false;
        host.sr = 0x01;
        host.tip = true;
        host.wait_shift();
        assert!(host.cuda.treq());

        // It sees TREQ and gives up: the attention byte comes again
        host.tip = false;
        host.wait_shift();
        host.tip = true;
        assert_eq!(host.wait_shift(), 0x03);
        assert!(!host.cuda.treq());
        host.tip = false;
        host.wait_shift();
        assert!(matches!(host.cuda.state, State::Rest(_)));
    }
}
