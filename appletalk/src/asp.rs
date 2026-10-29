//! AppleTalk Session Protocol (ASP)
//!
//! See Inside AppleTalk, 2nd edition, chapter 11.
//!
//! Implements the server side of ASP: session opening and closing, the
//! command/response transactions, GetStatus, Tickle, and Write. Every
//! AFP request except FPWrite and FPAddIcon travels as an SPCommand;
//! those two travel as an SPWrite: the request carries only the AFP
//! command block, the server sends a WriteContinue request to the
//! workstation's session socket, and the workstation's replies carry
//! the data, completing the held transaction. The ASP header lives in
//! the ATP user bytes, and the protocol rides on top of the ATP module
//! of the node's listening socket.

use std::collections::HashMap;

use crate::afp::Afp;
use crate::atp::{
    ATP_SOCKET, ATP_TYPE, HeldCompletion, HeldWrite, IncomingRequest, MAX_RESPONSE, Respond,
    Responder, TrespOutcome,
};
use crate::ddp::build_short;
use crate::link::Packet;
use std::time::{Duration, Instant};

/// The ATP function bits the WriteContinue uses
const FN_TREQ: u8 = 0x40;
const XO_BIT: u8 = 0x20;

/// ASP function codes (first byte of the ATP user bytes)
const FN_CLOSE_SESS: u8 = 1;
const FN_COMMAND: u8 = 2;
const FN_GET_STATUS: u8 = 3;
const FN_OPEN_SESS: u8 = 4;
const FN_TICKLE: u8 = 5;
/// The Write request: only the AFP command block travels in it; the
/// data follows through a server-initiated WriteContinue
const FN_WRITE: u8 = 6;
/// The server's WriteContinue (function 7) to the workstation
const FN_WRITE_CONTINUE: u8 = 7;

/// How often each end sends a tickle (ch11: every 30 seconds)
const TICKLE_INTERVAL: Duration = Duration::from_secs(30);

/// An open session
#[derive(Debug)]
struct Session {
    /// The workstation's node (where the tickles go)
    wss_node: u8,
    /// Workstation session socket, where WriteContinue (and the
    /// server's tickles) go
    wss_socket: u8,
    /// Sequence number of the last command received
    last_seq: u16,
    /// When the last tickle was sent
    last_tickle: Instant,
}

/// An ASP Write in progress: the client's held transaction, the
/// WriteContinue the server sent, and the data collected so far
struct PendingWrite {
    /// The client's held request (its node, socket and TID)
    held: HeldWrite,
    /// The session the write came in on
    session: u8,
    /// The Write request's sequence number (ch11: the WriteContinue
    /// carries it, and the workstation matches the two by it)
    seq: u16,
    /// The AFP command block; the workstation's data is appended to it
    /// before the command runs
    command: Vec<u8>,
    /// The write data, one slot per ATP sequence number of the
    /// workstation's reply (a lost packet leaves a hole the re-sent
    /// WriteContinue fills)
    slots: [Option<Vec<u8>>; 8],
    /// The workstation socket the WriteContinue went to
    wss_socket: u8,
    /// The client's node (where the WriteContinue went)
    wss_node: u8,
    /// The server's WriteContinue transaction ID (the matching key)
    my_tid: u16,
    /// The buffer size the WriteContinue asks for (16 bits on the
    /// wire): the FPWrite count from the command block, capped at the
    /// 8-packet ATP maximum
    receive_size: u16,
}

impl PendingWrite {
    /// The WriteContinue TReq: an XO request to the workstation for the
    /// write data, carrying the Write's sequence number and the 16-bit
    /// buffer size
    fn write_continue_packet(&self) -> Packet {
        let mut payload = Vec::with_capacity(8 + 2);
        payload.push(FN_TREQ | XO_BIT);
        payload.push(0xFF); // the node can receive 8 response packets
        payload.extend_from_slice(&self.my_tid.to_be_bytes());
        payload.extend_from_slice(&[
            FN_WRITE_CONTINUE,
            self.session,
            (self.seq >> 8) as u8,
            self.seq as u8,
        ]);
        payload.extend_from_slice(&self.receive_size.to_be_bytes());
        if crate::wire::wire_debug_enabled() {
            log::info!("ATP WriteContinue TReq: {:02x?}", payload);
        }
        Packet {
            kind: self.held.kind,
            node: self.wss_node,
            datagram: build_short(self.wss_socket, ATP_SOCKET, ATP_TYPE, &payload),
        }
    }
}

/// ASP protocol module: the server side, with the AFP server on top
pub struct Asp {
    sessions: HashMap<u8, Session>,
    next_session_id: u8,
    /// Transaction IDs for the node's own ATP requests (WriteContinue
    /// today; tickles and attention later)
    next_tid: u16,
    /// The pending writes: the WriteContinue TID → the held
    /// transaction
    pending_writes: HashMap<u16, PendingWrite>,
    afp: Afp,
}

impl Asp {
    pub fn new(afp: Afp) -> Self {
        Self {
            sessions: HashMap::new(),
            next_session_id: 1,
            next_tid: 1,
            pending_writes: HashMap::new(),
            afp,
        }
    }

    /// Handle an ASP request, returning the response
    fn request(&mut self, req: &IncomingRequest, user: &[u8], data: &[u8]) -> Option<Respond> {
        // The requester's offered response buffers bound what a
        // command may answer (FPRead caps its read at it)
        self.afp
            .set_request_capacity(crate::atp::response_buffers(req.bitmap));
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
                    // The session's forks and its negotiated AFP
                    // version go with it
                    self.afp.close_session(session_id);
                }
                // The session's outstanding writes go with it
                self.pending_writes.retain(|_, w| w.session != session_id);
                Some(Respond::Answer(vec![0, 0, 0, 0], Vec::new()))
            }
            FN_COMMAND => {
                // [Command][session ID][sequence number (2 bytes)]
                let session_id = *user.get(1)?;
                let session = self.sessions.get_mut(&session_id)?;
                let seq = u16::from_be_bytes([*user.get(2)?, *user.get(3)?]);
                session.last_seq = seq;
                log::debug!(
                    "ASP: command on session {} (seq {}, {} bytes)",
                    session_id,
                    seq,
                    data.len()
                );
                // The command reply user bytes carry the command result
                let (user, reply) = self.afp.command(session_id, data);
                Some(Respond::Answer(user, reply))
            }
            FN_GET_STATUS => {
                // The AFP server information block
                log::debug!("ASP: GetStatus (server info)");
                Some(Respond::Answer(vec![0, 0, 0, 0], self.afp.server_info()))
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
            FN_WRITE => {
                // [Write][session ID][sequence number (2 bytes)]: the
                // request carries only the AFP command block; the data
                // follows in the workstation's WriteContinue replies
                let session_id = *user.get(1)?;
                let session = self.sessions.get_mut(&session_id)?;
                let seq = u16::from_be_bytes([*user.get(2)?, *user.get(3)?]);
                session.last_seq = seq;
                log::debug!(
                    "ASP: write on session {} (seq {}, {} bytes)",
                    session_id,
                    seq,
                    data.len()
                );
                // A retransmitted Write: re-send the WriteContinue
                if let Some(pending) = self.pending_writes.values().find(|p| p.held.key == req.key)
                {
                    let my_tid = pending.my_tid;
                    log::debug!("ASP: retransmitting WriteContinue for TID {}", my_tid);
                    return Some(Respond::Hold(vec![pending.write_continue_packet()]));
                }
                let my_tid = self.next_tid;
                self.next_tid = self.next_tid.wrapping_add(1);
                // The buffer size to request: the FPWrite count from
                // the command block ([fn][flag][ref 2][offset 4]
                // [count 4]), capped at the 8-packet ATP maximum
                let receive_size = if data.first() == Some(&33) && data.len() >= 12 {
                    let count = u32::from_be_bytes(data[8..12].try_into().unwrap_or_default());
                    count.min(MAX_RESPONSE as u32)
                } else {
                    MAX_RESPONSE as u32
                } as u16;
                let pending = PendingWrite {
                    held: req.held(),
                    session: session_id,
                    seq,
                    command: data.to_vec(),
                    slots: [None, None, None, None, None, None, None, None],
                    wss_socket: session.wss_socket,
                    wss_node: req.key.0,
                    my_tid,
                    receive_size,
                };
                let packet = pending.write_continue_packet();
                self.pending_writes.insert(my_tid, pending);
                Some(Respond::Hold(vec![packet]))
            }
            _ => {
                log::debug!("ASP: dropped request with function {}", function);
                None
            }
        }
    }

    /// Collect a WriteContinue reply; when the message completes, run
    /// the held command and hand back its reply
    fn write_data(
        &mut self,
        src_socket: u8,
        tid: u16,
        seq: u8,
        eom: bool,
        data: &[u8],
    ) -> Option<TrespOutcome> {
        let pending = self.pending_writes.get_mut(&tid)?;
        if src_socket != pending.wss_socket {
            log::debug!("ASP: WriteContinue reply from a foreign socket");
            return None;
        }
        // The ATP sequence number slots the packet: a lost packet
        // leaves a hole the re-sent WriteContinue fills
        if let Some(slot) = pending.slots.get_mut(usize::from(seq)) {
            *slot = Some(data.to_vec());
        }
        if !eom {
            return Some(TrespOutcome::Pending);
        }
        // Complete when every slot up to the EOM packet is filled
        let complete = pending.slots[..=usize::from(seq)]
            .iter()
            .all(Option::is_some);
        if !complete {
            let packet = pending.write_continue_packet();
            log::debug!("ASP: WriteContinue reply has holes; re-requesting");
            return Some(TrespOutcome::Resend(packet));
        }
        let mut pending = self.pending_writes.remove(&tid)?;
        let mut all = Vec::new();
        for slot in pending.slots.iter_mut().flatten() {
            all.append(slot);
        }
        log::debug!(
            "ASP: write on session {} complete ({} bytes)",
            pending.session,
            all.len()
        );
        // The write data completes the AFP command block
        pending.command.extend_from_slice(&all);
        let (ruser, rdata) = self.afp.command(pending.session, &pending.command);
        Some(TrespOutcome::Complete(HeldCompletion {
            held: pending.held,
            ruser,
            rdata,
        }))
    }

    /// The periodic session housekeeping (from the node's timer tick):
    /// a tickle to every workstation every 30 seconds. Sessions have
    /// no host-clock timeout (a paused or restored emulator would
    /// trip over it); a vanished guest's state is reclaimed when it
    /// reboots and opens a new session from the same node.
    pub(crate) fn tick(&mut self, now: Instant) -> Vec<Packet> {
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
    /// close (their forks and the node's ATP retransmit cache go with
    /// them), and the node is reported only when sessions were
    /// actually closed, so a first OpenSess never purges anything —
    /// and the purge runs before the request executes, so the
    /// OpenSess's own reply stays cached.
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
            self.pending_writes.retain(|_, w| w.session != id);
            self.afp.close_session(id);
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
        src_socket: u8,
        tid: u16,
        seq: u8,
        eom: bool,
        data: &[u8],
    ) -> Option<TrespOutcome> {
        self.write_data(src_socket, tid, seq, eom, data)
    }

    fn rebooted_node(&mut self, node: u8, user: &[u8]) -> Option<u8> {
        Self::rebooted_node(self, node, user)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::atp::HeldCompletion;
    use crate::link::DdpHeaderKind;

    fn asp() -> Asp {
        Asp::new(Afp::with_root(PathBuf::from("/var/tmp/afp-share")))
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
        let (user, data) = answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        assert_eq!(user, [ATP_SOCKET, 1, 0, 0]); // SSS, session ID 1, noErr
        assert!(data.is_empty());
        assert_eq!(asp.sessions.len(), 1);
    }

    #[test]
    fn test_command_on_session() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        // FPGetSrvrParms: the server time and the volume list
        let (user, data) = answer(&mut asp, &[FN_COMMAND, 1, 0, 1], &[16, 0]);
        assert_eq!(user, [0, 0, 0, 0]); // command result: noErr
        assert_eq!(data[4], 1); // one volume
        assert_eq!(data[6], 4); // name length
        assert_eq!(&data[7..11], b"Snow");
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
    fn test_get_status_returns_server_info() {
        let mut asp = asp();
        let (user, data) = answer(&mut asp, &[FN_GET_STATUS, 0, 0], &[]);
        assert_eq!(user, [0, 0, 0, 0]);
        // The block starts with the offsets and flags, then the name
        let machine_type = u16::from_be_bytes([data[0], data[1]]);
        let versions = u16::from_be_bytes([data[2], data[3]]);
        let uams = u16::from_be_bytes([data[4], data[5]]);
        assert!(machine_type > 10);
        assert!(versions > machine_type);
        assert!(uams > versions);
        assert_eq!(data[10], 4);
        assert_eq!(&data[11..15], b"Snow");
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

    #[test]
    fn test_write_requests_data_with_write_continue() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        // An ASP Write carrying an FPGetSrvrParms command block: the
        // transaction is held and a WriteContinue TReq goes out to the
        // workstation's session socket
        let request = req(0x77);
        let held = match asp.request(&request, &[FN_WRITE, 1, 0, 1], &[16, 0]) {
            Some(Respond::Hold(packets)) => packets,
            _other => panic!("expected a hold"),
        };
        assert_eq!(held.len(), 1);
        let packet = &held[0];
        assert_eq!(packet.node, 0x20);
        let datagram = &packet.datagram;
        assert_eq!(datagram[2], 0x64); // dst = the WSS
        assert_eq!(datagram[3], ATP_SOCKET);
        assert_eq!(datagram[4], ATP_TYPE);
        let body = &datagram[5..];
        assert_eq!(body[0], FN_TREQ | XO_BIT); // an XO transaction
        assert_eq!(body[1], 0xFF); // 8 response buffers offered
        let my_tid = u16::from_be_bytes([body[2], body[3]]);
        assert_eq!(body[4], FN_WRITE_CONTINUE); // the user bytes
        assert_eq!(body[5], 1); // the session ID
        assert_eq!(body[6..8], [0, 1]); // the Write's sequence number
        assert_eq!(body[8..10], 4624u16.to_be_bytes()); // the 16-bit buffer size
        // A retransmitted Write re-sends the same WriteContinue
        let held_again = match asp.request(&request, &[FN_WRITE, 1, 0, 1], &[16, 0]) {
            Some(Respond::Hold(packets)) => packets,
            _other => panic!("expected a hold"),
        };
        assert_eq!(held_again[0].datagram, held[0].datagram);
        // The workstation's TResp (with EOM) completes the held
        // transaction: the command runs and its reply comes back
        let outcome = asp.tresp(0x64, my_tid, 0, true, &[]).unwrap();
        let TrespOutcome::Complete(HeldCompletion { held, ruser, rdata }) = outcome else {
            panic!("expected a completion");
        };
        assert_eq!(held.key, (0x20, 0x64, 0x77));
        assert_eq!(ruser, [0, 0, 0, 0]); // the command result
        assert_eq!(rdata[4], 1); // the server-info volume count
        assert!(asp.pending_writes.is_empty());
        // Closing the session drops its outstanding writes
        asp.request(&req(3), &[FN_WRITE, 1, 0, 2], &[16, 0])
            .unwrap();
        assert_eq!(asp.pending_writes.len(), 1);
        answer(&mut asp, &[FN_CLOSE_SESS, 1, 0, 0], &[]);
        assert!(asp.pending_writes.is_empty());
        // A Write on an unknown session is dropped
        assert!(
            asp.request(&req(2), &[FN_WRITE, 9, 0, 1], &[16, 0])
                .is_none()
        );
    }

    #[test]
    fn test_writecontinue_size_follows_the_fpwrite_count() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        // An FPWrite block ([fn][flag][ref 2][offset 4][count 4])
        // asking for 3 bytes
        let block = [33, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 3];
        let held = match asp.request(&req(0x31), &[FN_WRITE, 1, 0, 1], &block) {
            Some(Respond::Hold(packets)) => packets,
            _other => panic!("expected a hold"),
        };
        let body = &held[0].datagram[5..];
        assert_eq!(&body[8..10], &3u16.to_be_bytes());
    }

    #[test]
    fn test_write_data_collects_until_eom() {
        let mut asp = asp();
        answer(&mut asp, &[FN_OPEN_SESS, 0x64, 0, 1], &[]);
        let request = req(0x42);
        let held = match asp.request(&request, &[FN_WRITE, 1, 0, 1], &[16, 0]) {
            Some(Respond::Hold(packets)) => packets,
            _other => panic!("expected a hold"),
        };
        let my_tid = u16::from_be_bytes([held[0].datagram[7], held[0].datagram[8]]);
        // Two data packets, no EOM yet: nothing completes
        assert!(matches!(
            asp.tresp(0x64, my_tid, 0, false, b"hello"),
            Some(TrespOutcome::Pending)
        ));
        assert!(matches!(
            asp.tresp(0x64, my_tid, 1, false, b" "),
            Some(TrespOutcome::Pending)
        ));
        assert_eq!(asp.pending_writes.len(), 1);
        // A reply from a foreign socket is ignored
        assert!(asp.tresp(0x65, my_tid, 2, true, b"world").is_none());
        assert_eq!(asp.pending_writes.len(), 1);
        // An EOM packet with a hole in the reply re-requests the data
        // (the missing slot 2) instead of completing
        let outcome = asp.tresp(0x64, my_tid, 3, true, b"world").unwrap();
        assert!(matches!(outcome, TrespOutcome::Resend(_)));
        assert_eq!(asp.pending_writes.len(), 1);
        // The missing packet arrives and completes the write
        let outcome = asp.tresp(0x64, my_tid, 2, true, b"x").unwrap();
        let TrespOutcome::Complete(completion) = outcome else {
            panic!("expected a completion");
        };
        assert_eq!(completion.ruser, [0, 0, 0, 0]);
        assert!(asp.pending_writes.is_empty());
    }
}
