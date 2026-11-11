//! AppleTalk Session Protocol (ASP)
//!
//! See Inside AppleTalk, 2nd edition, chapter 11.
//!
//! Implements the server side of ASP: session opening and closing, the
//! command/response transactions, GetStatus and Tickle. The ASP header
//! lives in the ATP user bytes, and the protocol rides on top of the ATP
//! module of the node's listening socket.

use std::collections::HashMap;

use crate::atp::{ATP_SOCKET, ATP_TYPE, IncomingRequest, Respond, Responder, TrespOutcome};
use crate::ddp::build_short;
use crate::link::Packet;
use crate::wire::wire_log;

/// The ATP function bits the server's tickles use
const FN_TREQ: u8 = 0x40;

/// ASP function codes (first byte of the ATP user bytes)
const FN_CLOSE_SESS: u8 = 1;
const FN_COMMAND: u8 = 2;
const FN_GET_STATUS: u8 = 3;
const FN_OPEN_SESS: u8 = 4;
const FN_TICKLE: u8 = 5;

/// An open session, if the node asking is the workstation that opened it:
/// with several workstations on the network, a session ID alone would let
/// one use or close another's session
fn open_session(sessions: &mut HashMap<u8, Session>, id: u8, node: u8) -> Option<&mut Session> {
    sessions.get_mut(&id).filter(|s| s.wss_node == node)
}

/// The ASP version, the only one there is
const ASP_VERSION: u16 = 0x0100;
/// ch11's OpenSessReply errors: a version the server does not support, and
/// a server that cannot open another session
const ERR_BAD_VERS_NUM: i16 = -1066;
const ERR_SERVER_BUSY: i16 = -1071;

/// How often each end sends a tickle, in seconds (ch11: every 30 seconds)
const TICKLE_INTERVAL: u64 = 30;

/// An open session
#[derive(Debug)]
struct Session {
    /// The workstation's node (where the tickles go)
    wss_node: u8,
    /// Workstation session socket, where the server's tickles go
    wss_socket: u8,
    /// The second the last tickle was sent in (see `Asp::seconds`)
    last_tickle: u64,
}

/// ASP protocol module: the server side
pub struct Asp {
    sessions: HashMap<u8, Session>,
    next_session_id: u8,
    /// Transaction IDs for the node's own ATP requests (its tickles)
    next_tid: u16,
    /// The seconds of the link's time counted so far (one per tick)
    seconds: u64,
    /// The status block GetStatus returns (the AFP server information
    /// block once the AFP server exists)
    status: Vec<u8>,
}

impl Asp {
    pub fn new(status: &[u8]) -> Self {
        Self {
            sessions: HashMap::new(),
            next_session_id: 1,
            next_tid: 1,
            seconds: 0,
            status: status.to_vec(),
        }
    }

    /// The next session ID no open session holds, from 1 to 255 (0 is never
    /// a session) and wrapping round, or None when all of them are open
    fn free_session_id(&mut self) -> Option<u8> {
        for _ in 0..255 {
            let id = self.next_session_id;
            self.next_session_id = id % 255 + 1;
            if !self.sessions.contains_key(&id) {
                return Some(id);
            }
        }
        None
    }

    /// Handle an ASP request, returning the response
    fn request(&mut self, req: &IncomingRequest, user: &[u8], data: &[u8]) -> Option<Respond> {
        let function = *user.first()?;
        match function {
            FN_OPEN_SESS => {
                // [OpenSess][WSS][ASP version (2 bytes)]
                let wss_socket = *user.get(1)?;
                // Appendix C: "The ASP version number ... is version $0100";
                // ch11: BadVersNum when the "server cannot support the
                // offered version number"
                if user.get(2..4)? != ASP_VERSION.to_be_bytes() {
                    return Some(Respond::Answer(
                        [[ATP_SOCKET, 0], ERR_BAD_VERS_NUM.to_be_bytes()].concat(),
                        Vec::new(),
                    ));
                }
                // ch11: the session ID is unique per SLS, and ServerBusy
                // means the "server cannot open another session"
                let Some(session_id) = self.free_session_id() else {
                    return Some(Respond::Answer(
                        [[ATP_SOCKET, 0], ERR_SERVER_BUSY.to_be_bytes()].concat(),
                        Vec::new(),
                    ));
                };
                let now = self.seconds;
                self.sessions.insert(
                    session_id,
                    Session {
                        wss_node: req.key.0,
                        wss_socket,
                        last_tickle: now,
                    },
                );
                wire_log!(
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
                if open_session(&mut self.sessions, session_id, req.key.0).is_some() {
                    self.sessions.remove(&session_id);
                    wire_log!("ASP: closed session {}", session_id);
                }
                Some(Respond::Answer(vec![0, 0, 0, 0], Vec::new()))
            }
            FN_COMMAND => {
                // [Command][session ID][sequence number (2 bytes)]
                let session_id = *user.get(1)?;
                open_session(&mut self.sessions, session_id, req.key.0)?;
                let seq = u16::from_be_bytes([*user.get(2)?, *user.get(3)?]);
                wire_log!(
                    "ASP: command on session {} (seq {}, {} bytes)",
                    session_id,
                    seq,
                    data.len()
                );
                // No AFP server above ASP yet: commands are dropped
                None
            }
            FN_GET_STATUS => {
                wire_log!("ASP: GetStatus");
                Some(Respond::Answer(vec![0, 0, 0, 0], self.status.clone()))
            }
            FN_TICKLE => {
                // [Tickle][session ID][0 (2 bytes)]: a heartbeat, never
                // answered (ch11; netatalk does not answer them either).
                // The server has no session timeout to restart.
                let session_id = *user.get(1)?;
                if self.sessions.contains_key(&session_id) {
                    wire_log!("ASP: tickle on session {}", session_id);
                }
                None
            }
            _ => {
                wire_log!("ASP: dropped request with function {}", function);
                None
            }
        }
    }

    /// The session housekeeping, once per second of the link's time: a
    /// tickle to every workstation every 30 seconds. Sessions have no
    /// timeout; a vanished guest's state is reclaimed when it reboots
    /// and opens a new session from the same node.
    pub(crate) fn tick(&mut self) -> Vec<Packet> {
        self.seconds += 1;
        let now = self.seconds;
        let mut tickle_to = Vec::new();
        for (id, session) in &mut self.sessions {
            if now - session.last_tickle >= TICKLE_INTERVAL {
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

    /// The ATP layer's pre-request hook: an OpenSess naming a session
    /// socket one of the node's sessions already holds is a guest reboot,
    /// because a live session still has that socket open. A second session
    /// on another socket is left alone. The node is reported only when
    /// sessions were actually closed, so an ordinary OpenSess never purges
    /// anything — and the purge runs before the request executes, so the
    /// OpenSess's own reply stays cached.
    fn rebooted_node(&mut self, node: u8, user: &[u8]) -> Option<u8> {
        if user.first() != Some(&FN_OPEN_SESS) {
            return None;
        }
        let wss = *user.get(1)?;
        self.sessions
            .values()
            .any(|s| s.wss_node == node && s.wss_socket == wss)
            .then(|| self.close_node_sessions(node))
            .flatten()
    }

    /// Close every session of a node that is known to have restarted.
    /// Reports the node when there was anything to close.
    pub(crate) fn close_node_sessions(&mut self, node: u8) -> Option<u8> {
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
            wire_log!("ASP: guest restart: closed old session {}", id);
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

    /// No transaction is ever held: there is nothing to complete
    fn tresp(
        &mut self,
        _src: (u8, u8),
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
    use crate::link::DdpHeaderKind;

    fn asp() -> Asp {
        Asp::new(&[])
    }

    /// A request as the node's ATP layer would build it (the client
    /// node 0x20, socket 0x64)
    fn req(tid: u16) -> IncomingRequest {
        IncomingRequest {
            key: (0x20, 0x64, tid),
            control: 0,
            bitmap: 0x01,
            kind: DdpHeaderKind::Short,
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
        let (user, data) = answer(&mut asp, &[FN_OPEN_SESS, 0x64, 1, 0], &[]);
        assert_eq!(user, [ATP_SOCKET, 1, 0, 0]); // SSS, session ID 1, noErr
        assert!(data.is_empty());
        assert_eq!(asp.sessions.len(), 1);
    }

    #[test]
    fn test_another_node_cannot_use_a_session() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 1, 0], &[]);
        let mut other = req(2);
        other.key.0 = 0x21;
        // Its command is not answered, and its CloseSess closes nothing
        assert!(
            asp.request(&other, &[FN_COMMAND, 1, 0, 1], &[16, 0])
                .is_none()
        );
        assert!(
            asp.request(&other, &[FN_CLOSE_SESS, 1, 0, 0], &[])
                .is_some()
        );
        assert_eq!(asp.sessions.len(), 1);
    }

    #[test]
    fn test_open_sess_checks_the_version() {
        let mut asp = asp();
        let (user, _) = answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        assert_eq!(user, [ATP_SOCKET, 0, 0xFB, 0xD6]);
        assert!(asp.sessions.is_empty());
    }

    #[test]
    fn test_session_ids_skip_open_sessions() {
        let mut asp = asp();
        for id in 1..=255u8 {
            let (user, _) = answer(&mut asp, &[FN_OPEN_SESS, id, 1, 0], &[]);
            assert_eq!(user, [ATP_SOCKET, id, 0, 0]);
        }
        // All 255 are open: ServerBusy
        let (user, _) = answer(&mut asp, &[FN_OPEN_SESS, 0x64, 1, 0], &[]);
        assert_eq!(user, [ATP_SOCKET, 0, 0xFB, 0xD1]);
        // A closed one is reused, and an open one never is
        answer(&mut asp, &[FN_CLOSE_SESS, 7, 0, 0], &[]);
        let (user, _) = answer(&mut asp, &[FN_OPEN_SESS, 0x64, 1, 0], &[]);
        assert_eq!(user, [ATP_SOCKET, 7, 0, 0]);
        assert_eq!(asp.sessions.len(), 255);
    }

    #[test]
    fn test_command_dropped_without_a_server() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 1, 0], &[]);
        // No AFP server above ASP yet: commands are dropped
        assert!(
            asp.request(&req(1), &[FN_COMMAND, 1, 0, 1], &[16, 0])
                .is_none()
        );
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
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 1, 0], &[]);
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
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 1, 0], &[]);
        // A tickle is a heartbeat: it is never answered, known session
        // or not (ch11; netatalk does not answer them either)
        assert!(asp.request(&req(1), &[FN_TICKLE, 1, 0, 0], &[]).is_none());
        assert!(asp.request(&req(1), &[FN_TICKLE, 9, 0, 0], &[]).is_none());
        // The server tickles the session every 30 ticks of the link's
        // time, and only then
        for second in 1..=60 {
            let tickles = asp.tick();
            assert_eq!(tickles.len(), usize::from(second % 30 == 0), "{second}");
        }
    }

    #[test]
    fn test_unknown_function() {
        let mut asp = asp();
        assert!(asp.request(&req(1), &[9, 0, 0, 0], &[]).is_none());
    }

    #[test]
    fn test_a_second_session_on_another_socket_stays_open() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 1, 0], &[]);
        // Another socket: a second session, and the first keeps working
        assert_eq!(asp.rebooted_node(0x20, &[FN_OPEN_SESS, 0x65, 1, 0]), None);
        answer(&mut asp, &[FN_OPEN_SESS, 0x65, 1, 0], &[]);
        assert_eq!(asp.sessions.len(), 2);
        // Another node's socket number means nothing here
        assert_eq!(asp.rebooted_node(0x21, &[FN_OPEN_SESS, 0x64, 1, 0]), None);
        // A socket a session holds can only be a restart: all of the
        // node's sessions go
        assert_eq!(
            asp.rebooted_node(0x20, &[FN_OPEN_SESS, 0x64, 1, 0]),
            Some(0x20)
        );
        assert!(asp.sessions.is_empty());
    }
}
