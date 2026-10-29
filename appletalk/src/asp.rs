//! AppleTalk Session Protocol (ASP)
//!
//! See Inside AppleTalk, 2nd edition, chapter 11.
//!
//! Implements the server side of ASP: session opening and closing, the
//! command/response transactions, GetStatus and Tickle. The ASP header
//! lives in the ATP user bytes, and the protocol rides on top of the
//! ATP module of the node's listening socket.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::atp::{ATP_SOCKET, ATP_TYPE, IncomingRequest, Respond, Responder, TrespOutcome};
use crate::ddp::build_short;
use crate::link::Packet;

/// The ATP function bits the server's tickles use
const FN_TREQ: u8 = 0x40;

/// How often each end sends a tickle (ch11: every 30 seconds)
const TICKLE_INTERVAL: Duration = Duration::from_secs(30);

/// ASP function codes (first byte of the ATP user bytes)
const FN_CLOSE_SESS: u8 = 1;
const FN_COMMAND: u8 = 2;
const FN_GET_STATUS: u8 = 3;
const FN_OPEN_SESS: u8 = 4;
const FN_TICKLE: u8 = 5;

/// An open session
#[derive(Debug)]
struct Session {
    /// The workstation's node (where the tickles go)
    wss_node: u8,
    /// Workstation session socket, where the server's tickles go
    wss_socket: u8,
    /// Sequence number of the last command received
    last_seq: u16,
    /// When the last tickle was sent
    last_tickle: Instant,
}

/// ASP protocol module: the server side
pub struct Asp {
    sessions: HashMap<u8, Session>,
    next_session_id: u8,
    /// Transaction IDs for the node's own ATP requests (its tickles)
    next_tid: u16,
    /// The status block returned by GetStatus (the AFP server
    /// information block once the AFP server exists)
    status: Vec<u8>,
}

impl Asp {
    pub fn new(status: &[u8]) -> Self {
        Self {
            sessions: HashMap::new(),
            next_session_id: 1,
            next_tid: 1,
            status: status.to_vec(),
        }
    }

    /// Handle an ASP request, returning the response
    fn request(&mut self, req: &IncomingRequest, user: &[u8], data: &[u8]) -> Option<Respond> {
        let function = *user.first()?;
        match function {
            FN_OPEN_SESS => {
                // [OpenSess][WSS][ASP version (2 bytes)]
                let wss_socket = *user.get(1)?;
                let session_id = self.next_session_id;
                // Session IDs wrap past 255 back to 1: 0 is never a
                // valid session
                self.next_session_id = match self.next_session_id.wrapping_add(1) {
                    0 => 1,
                    next => next,
                };
                let now = Instant::now();
                self.sessions.insert(
                    session_id,
                    Session {
                        wss_node: req.key.0,
                        wss_socket,
                        last_seq: 0,
                        last_tickle: now,
                    },
                );
                log::debug!(
                    "ASP: opened session {} (WSS socket {})",
                    session_id,
                    wss_socket
                );
                // [SSS][session ID][error code (2 bytes)]
                Some(Respond::Answer(
                    vec![ATP_SOCKET, session_id, 0, 0],
                    Vec::new(),
                ))
            }
            FN_CLOSE_SESS => {
                // [CloseSess][session ID][0 (2 bytes)]
                let session_id = *user.get(1)?;
                if self.sessions.remove(&session_id).is_some() {
                    log::debug!("ASP: closed session {}", session_id);
                }
                Some(Respond::Answer(vec![0, 0, 0, 0], Vec::new()))
            }
            FN_COMMAND => {
                // [Command][session ID][sequence number (2 bytes)]
                let session_id = *user.get(1)?;
                let session = self.sessions.get_mut(&session_id)?;
                let seq = u16::from_be_bytes([*user.get(2)?, *user.get(3)?]);
                session.last_seq = seq;
                log::debug!(
                    "ASP: command on session {} (seq {}, {} bytes), no server above yet",
                    session_id,
                    seq,
                    data.len()
                );
                None
            }
            FN_GET_STATUS => {
                log::debug!("ASP: GetStatus (placeholder block)");
                Some(Respond::Answer(vec![0, 0, 0, 0], self.status.clone()))
            }
            FN_TICKLE => {
                // [Tickle][session ID][0 (2 bytes)]: a heartbeat. The
                // timeout restarts, and a tickle is never answered
                // (ch11; netatalk does not answer them either)
                let session_id = *user.get(1)?;
                if self.sessions.contains_key(&session_id) {
                    log::debug!("ASP: tickle on session {}", session_id);
                }
                None
            }
            _ => {
                log::debug!("ASP: dropped request with function {}", function);
                None
            }
        }
    }

    /// The periodic session housekeeping (from the node's timer tick):
    /// a tickle to every workstation every 30 seconds, and a session
    /// that heard nothing for 2 minutes closes. The session's AFP
    /// state (its forks and negotiated version) closes once the AFP
    /// server lands.
    pub(crate) fn tick(&mut self, now: Instant) -> Vec<Packet> {
        // The periodic session housekeeping (from the node's timer
        // tick): a tickle to every workstation every 30 seconds.
        // Sessions have no host-clock timeout (a paused or restored
        // emulator would trip over it); a vanished guest's state is
        // reclaimed when it reboots and opens a new session from the
        // same node
        let mut tickle_to = Vec::new();
        for (id, session) in &mut self.sessions {
            if now.saturating_duration_since(session.last_tickle) >= TICKLE_INTERVAL {
                session.last_tickle = now;
                tickle_to.push((*id, session.wss_node, session.wss_socket));
            }
        }
        let mut packets = Vec::with_capacity(tickle_to.len());
        for (id, node, socket) in tickle_to {
            packets.push(self.tickle_packet(id, node, socket));
        }
        packets
    }

    /// The ATP layer's pre-request hook: a new OpenSess from a node
    /// that already has a session is a guest reboot. The old sessions
    /// close (their AFP state closes once the AFP server lands, and
    /// the node's ATP retransmit cache goes with them). The node is
    /// reported only when sessions were actually closed, so a first
    /// OpenSess never purges anything — and the purge runs before the
    /// request executes, so the OpenSess's own reply stays cached.
    fn rebooted_node(&mut self, node: u8, user: &[u8]) -> Option<u8> {
        if user.first() != Some(&FN_OPEN_SESS) {
            return None;
        }
        let old: Vec<u8> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.wss_node == node)
            .map(|(id, _)| *id)
            .collect();
        if old.is_empty() {
            return None;
        }
        for id in old {
            self.sessions.remove(&id);
            log::debug!("ASP: guest reboot: closed old session {}", id);
        }
        Some(node)
    }

    /// The server's tickle: an ALO TReq (no XO bit) to the workstation's
    /// session socket. Tickles are never answered.
    fn tickle_packet(&mut self, session_id: u8, wss_node: u8, wss_socket: u8) -> Packet {
        let mut payload = Vec::with_capacity(8);
        payload.push(FN_TREQ);
        payload.push(0x01); // one response buffer, never used
        payload.extend_from_slice(&self.next_tid.to_be_bytes());
        self.next_tid = self.next_tid.wrapping_add(1);
        payload.extend_from_slice(&[FN_TICKLE, session_id, 0, 0]);
        if crate::wire::wire_debug_enabled() {
            log::info!("ASP tickle TReq: {:02x?}", payload);
        }
        Packet {
            kind: crate::link::DdpHeaderKind::Short,
            node: wss_node,
            datagram: build_short(wss_socket, ATP_SOCKET, ATP_TYPE, &payload),
        }
    }
}

impl Responder for Asp {
    fn respond(&mut self, req: &IncomingRequest, user: &[u8], data: &[u8]) -> Option<Respond> {
        self.request(req, user, data)
    }

    fn tresp(
        &mut self,
        _src_socket: u8,
        _tid: u16,
        _seq: u8,
        _eom: bool,
        _data: &[u8],
    ) -> Option<TrespOutcome> {
        None
    }

    fn rebooted_node(&mut self, node: u8, user: &[u8]) -> Option<u8> {
        Self::rebooted_node(self, node, user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asp() -> Asp {
        Asp::new(&[])
    }

    fn req(tid: u16) -> IncomingRequest {
        IncomingRequest {
            key: (0x20, 0x64, tid),
            control: 0,
            bitmap: 0x01,
            kind: crate::link::DdpHeaderKind::Short,
        }
    }

    /// Run a request that must answer directly
    fn answer(asp: &mut Asp, user: &[u8], data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let req = req(1);
        match asp.request(&req, user, data).unwrap() {
            Respond::Answer(user, data) => (user, data),
            Respond::Hold(_) => panic!("unexpected hold"),
        }
    }

    #[test]
    fn test_open_sess() {
        let mut asp = asp();
        let (user, data) = answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        assert_eq!(user, [ATP_SOCKET, 1, 0, 0]); // SSS, session ID 1, noErr
        assert!(data.is_empty());
        assert_eq!(asp.sessions.len(), 1);
    }

    #[test]
    fn test_command_dropped_without_a_server() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        // No AFP server above ASP yet: commands are dropped
        assert!(
            asp.request(&req(1), &[FN_COMMAND, 1, 0, 1], &[16, 0])
                .is_none()
        );
        assert_eq!(asp.sessions[&1].last_seq, 1);
    }

    #[test]
    fn test_command_on_unknown_session() {
        let mut asp = asp();
        assert!(
            asp.request(&req(1), &[FN_COMMAND, 9, 0, 1], &[16, 0])
                .is_none()
        );
    }

    #[test]
    fn test_close_sess() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        let (user, _) = answer(&mut asp, &[FN_CLOSE_SESS, 1, 0, 0], &[]);
        assert_eq!(user, [0, 0, 0, 0]);
        assert!(asp.sessions.is_empty());
        // Commands on the closed session are dropped
        assert!(
            asp.request(&req(1), &[FN_COMMAND, 1, 0, 1], &[16, 0])
                .is_none()
        );
    }

    #[test]
    fn test_get_status_returns_the_configured_block() {
        let mut asp = Asp::new(b"placeholder");
        let (user, data) = answer(&mut asp, &[FN_GET_STATUS, 0, 0], &[]);
        assert_eq!(user, [0, 0, 0, 0]);
        assert_eq!(data, b"placeholder");
    }

    #[test]
    fn test_tickle() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        // A tickle is a heartbeat: it is never answered, known session
        // or not (ch11; netatalk does not answer them either)
        assert!(asp.request(&req(1), &[FN_TICKLE, 1, 0, 0], &[]).is_none());
        assert!(asp.request(&req(1), &[FN_TICKLE, 9, 0, 0], &[]).is_none());
    }

    #[test]
    fn test_unknown_function() {
        let mut asp = asp();
        assert!(asp.request(&req(1), &[9, 0, 0, 0], &[]).is_none());
    }
}
