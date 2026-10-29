//! AppleTalk Transaction Protocol (ATP)
//!
//! See Inside AppleTalk, 2nd edition, chapter 9.
//!
//! Implements the responder side of ATP transactions: exactly-once (XO)
//! requests with a retransmit cache (a duplicate request gets the cached
//! response back instead of being executed again), transaction release,
//! and multi-packet responses. Each response packet is an 8-byte ATP
//! header (control, bitmap/sequence, TID, 4 user bytes) followed by up
//! to 578 data bytes (the layout netatalk's asp_cmdreply produces): the
//! first packet's user-bytes field carries the responder's user bytes,
//! the following packets carry four zero bytes in its place, the
//! sequence numbers run 0..n-1, and only the last packet has the EOM
//! bit set.
//!
//! The requester side of one pattern is here too: a transaction the
//! responder does not answer right away. ASP Write is the user: the
//! responder holds the request (a `Hold`), sends a server-initiated
//! request (the WriteContinue), and the workstation's TResp completes
//! the held transaction, whose response is built and cached like any
//! other.

use std::collections::HashMap;

use crate::ddp::build_short;
use crate::link::{DdpHeaderKind, Packet};

/// ATP listening socket of the node
pub const ATP_SOCKET: u8 = 0x80;

/// DDP type of ATP packets
pub const ATP_TYPE: u8 = 3;

/// Function codes in the two high bits of the control information byte
const FN_TREQ: u8 = 0x40;
const FN_TRESP: u8 = 0x80;
const FN_TREL: u8 = 0xC0;

/// Control information bits
const XO_BIT: u8 = 0x20;
const EOM_BIT: u8 = 0x10;

/// Maximum data bytes per response packet: the DDP payload limit (586)
/// minus the 8-byte ATP header (control, bitmap/sequence, TID, 4 user
/// bytes). The 4 user bytes are part of the header, not part of the
/// data.
const MAX_RESPONSE_BODY: usize = 578;

/// The data bytes a response packet carries after the ATP header
pub(crate) const MAX_RESPONSE_DATA: usize = MAX_RESPONSE_BODY;

/// The largest reply a single transaction can carry: 8 packets. The
/// ASP WriteContinue buffer size is this value.
pub(crate) const MAX_RESPONSE: usize = MAX_RESPONSE_DATA * 8;

/// ATP protocol module: the responder side
pub struct Atp {
    /// Exactly-once retransmit cache: (requester node, requester socket,
    /// transaction ID) -> the response packets. Entries are removed by
    /// TRel or, when the requester reboots (a new OpenSess from its
    /// node), by the node-scoped purge. No host-clock timer: a paused
    /// or restored emulator must not lose cached responses.
    xo_cache: HashMap<(u8, u8, u16), Vec<Packet>>,
    /// Number of responses to drop without sending (test support for
    /// simulating packet loss, set from SNOW_AT_DROP_RESPONSES)
    #[cfg(feature = "test_module")]
    drop_responses: usize,
}

/// An incoming ATP request: who sent it, and the bits needed to
/// answer (or hold) it
pub struct IncomingRequest {
    /// The requester's (node, socket) and the transaction ID
    pub key: (u8, u8, u16),
    /// The request's control byte (the XO bit is what matters here)
    pub control: u8,
    /// The requester's response buffers (how many response packets it
    /// can receive)
    pub bitmap: u8,
    /// The link-layer header kind of the request (echoed in the
    /// response packets)
    pub kind: DdpHeaderKind,
}

impl IncomingRequest {
    /// The held form: what the eventual answer needs to know
    pub fn held(&self) -> HeldWrite {
        HeldWrite {
            key: self.key,
            control: self.control,
            bitmap: self.bitmap,
            kind: self.kind,
        }
    }
}

/// A request the responder did not answer: the transaction is held
/// until the responder finishes it (an ASP Write, answered once the
/// workstation's WriteContinue data arrives)
pub struct HeldWrite {
    /// The requester's (node, socket) and the transaction ID
    pub key: (u8, u8, u16),
    pub control: u8,
    pub bitmap: u8,
    pub kind: DdpHeaderKind,
}

/// A held transaction the workstation's TResp completed: the responder
/// ran the command and produced the reply
pub struct HeldCompletion {
    pub held: HeldWrite,
    /// The response user bytes (the command result)
    pub ruser: Vec<u8>,
    /// The response data
    pub rdata: Vec<u8>,
}

/// What a workstation's TResp did to the held transaction
pub enum TrespOutcome {
    /// The data completed the write: the client's reply is ready
    Complete(HeldCompletion),
    /// A hole in the reply: re-send the request packet
    Resend(Packet),
    /// Still collecting
    Pending,
}

/// The responder's answer to a request
pub enum Respond {
    /// Answer the request with these user bytes and data
    Answer(Vec<u8>, Vec<u8>),
    /// Hold the transaction and send these packets instead (an ASP
    /// Write's WriteContinue); the eventual answer comes through a
    /// `HeldCompletion`
    Hold(Vec<Packet>),
}

/// A client of ATP: executes transaction requests
pub trait Responder {
    /// Execute a request, returning the response, or None to drop the
    /// request without answering
    fn respond(&mut self, req: &IncomingRequest, user: &[u8], data: &[u8]) -> Option<Respond>;

    /// A TResp to one of the node's own requests (an ASP
    /// WriteContinue): collect the data; when the message completes
    /// (the EOM bit), return the held transaction with the command's
    /// reply
    fn tresp(
        &mut self,
        src_socket: u8,
        tid: u16,
        seq: u8,
        eom: bool,
        data: &[u8],
    ) -> Option<TrespOutcome>;

    /// A hook the responder gets before a request executes: it
    /// reports a guest reboot (a new OpenSess from a node that
    /// already had a session), so the retransmit cache can be purged
    /// before this request's reply is cached. None for everything
    /// else.
    fn rebooted_node(&mut self, node: u8, user: &[u8]) -> Option<u8> {
        let _ = (node, user);
        None
    }
}

impl Atp {
    pub fn new() -> Self {
        #[cfg(feature = "test_module")]
        let drop_responses = std::env::var("SNOW_AT_DROP_RESPONSES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Self {
            xo_cache: HashMap::new(),
            #[cfg(feature = "test_module")]
            drop_responses,
        }
    }

    /// Handle an ATP packet, returning the response packets if any.
    /// The responder executes the request.
    pub fn handle(
        &mut self,
        packet: &Packet,
        data: &[u8],
        responder: &mut dyn Responder,
    ) -> Option<Vec<Packet>> {
        if data.len() < 8 {
            log::debug!("Dropped short ATP packet ({} bytes)", data.len());
            return None;
        }
        let control = data[0];
        let bitmap = data[1]; // the requester's response buffers
        let tid = u16::from_be_bytes([data[2], data[3]]);
        let user = &data[4..8];
        let payload = &data[8..];
        let requester_socket = packet_requester_socket(packet);
        let key = (packet.node, requester_socket, tid);
        if crate::wire::wire_debug_enabled() {
            log::info!("ATP request: {:02x?}", data);
        }

        match control & 0xC0 {
            FN_TREQ => {
                // A duplicate XO request gets the cached response back.
                // A retransmitted request's bitmap marks the packets it
                // is still missing: resend only those (a fresh request
                // misses the cache and executes)
                if control & XO_BIT != 0
                    && let Some(response) = self.xo_cache.get(&key)
                {
                    let wanted: Vec<Packet> = response
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| usize::from(bitmap) & (1 << i) != 0)
                        .map(|(_, p)| p.clone())
                        .collect();
                    log::debug!(
                        "Retransmitting {} cached ATP response packet{} for TID {}",
                        wanted.len(),
                        if wanted.len() == 1 { "" } else { "s" },
                        tid
                    );
                    return Some(wanted);
                }

                // A guest reboot (the responder saw a new OpenSess from
                // a node that already had a session) drops the node's
                // cached responses BEFORE this request executes, so the
                // OpenSess's own reply survives in the cache
                if let Some(node) = responder.rebooted_node(packet.node, user) {
                    self.purge_node(node);
                }
                // Execute the request
                let req = IncomingRequest {
                    key,
                    control,
                    bitmap,
                    kind: packet.kind,
                };
                let result = responder.respond(&req, user, payload)?;
                match result {
                    Respond::Answer(ruser, rdata) => {
                        let responses = self.build_response(&req.held(), &ruser, &rdata);
                        if control & XO_BIT != 0 {
                            self.xo_cache.insert(key, responses.clone());
                        }
                        log::debug!(
                            "Answering ATP TReq (TID {}, {} bytes{} in {} packet{})",
                            tid,
                            payload.len(),
                            if control & XO_BIT != 0 { ", XO" } else { "" },
                            responses.len(),
                            if responses.len() == 1 { "" } else { "s" }
                        );
                        #[cfg(feature = "test_module")]
                        if self.drop_responses > 0 {
                            self.drop_responses -= 1;
                            log::debug!("Dropping ATP response (test support)");
                            return None;
                        }
                        Some(responses)
                    }
                    Respond::Hold(packets) => {
                        log::debug!(
                            "Holding ATP transaction (TID {}): {} packet{} sent instead",
                            tid,
                            packets.len(),
                            if packets.len() == 1 { "" } else { "s" }
                        );
                        Some(packets)
                    }
                }
            }
            FN_TRESP => {
                // A response to one of the node's own requests (an ASP
                // WriteContinue): the responder collects the data and,
                // when the message completes, finishes the held
                // transaction
                let seq = data[1];
                let eom = control & EOM_BIT != 0;
                let outcome = responder.tresp(requester_socket, tid, seq, eom, payload)?;
                match outcome {
                    TrespOutcome::Pending => None,
                    TrespOutcome::Resend(packet) => Some(vec![packet]),
                    TrespOutcome::Complete(completion) => {
                        let mut responses = self.build_response(
                            &completion.held,
                            &completion.ruser,
                            &completion.rdata,
                        );
                        if completion.held.control & XO_BIT != 0 {
                            self.xo_cache.insert(completion.held.key, responses.clone());
                        }
                        // Our request was XO: release the workstation's
                        // response cache with a TRel
                        responses.push(self.trel_packet(
                            packet.node,
                            requester_socket,
                            tid,
                            packet.kind,
                        ));
                        log::debug!("Completed held ATP transaction (TID {})", tid);
                        Some(responses)
                    }
                }
            }
            FN_TREL => {
                if self.xo_cache.remove(&key).is_some() {
                    log::debug!("Released ATP transaction {}", tid);
                }
                None
            }
            _ => {
                log::debug!("Dropped ATP packet with control {:02X}", control);
                None
            }
        }
    }

    /// Build the response packets for a request (or a held
    /// transaction): one packet per 578 data bytes (the 4 user bytes
    /// are part of the 8-byte ATP header: the first packet carries the
    /// responder's user bytes in that field, the following ones carry
    /// four zero bytes), at least one, and never more than the
    /// requester can receive. When the offered buffers run out
    /// mid-message, the last packet sent still carries EOM so the
    /// requester's transaction completes with the (short) message.
    /// Drop a requester's cache entries: the node calls this when the
    /// guest reboots (its ASP layer saw a new OpenSess from a node
    /// that already had a session), so a recycled TID never gets an
    /// old response
    pub fn purge_node(&mut self, node: u8) {
        self.xo_cache.retain(|(n, _, _), _| *n != node);
    }

    /// A TRel for one of the node's own XO requests: release the
    /// responder's cached response
    fn trel_packet(&self, node: u8, socket: u8, tid: u16, kind: DdpHeaderKind) -> Packet {
        let mut payload = Vec::with_capacity(8);
        payload.push(FN_TREL);
        payload.push(0);
        payload.extend_from_slice(&tid.to_be_bytes());
        payload.extend_from_slice(&[0; 4]);
        Packet {
            kind,
            node,
            datagram: build_short(socket, ATP_SOCKET, ATP_TYPE, &payload),
        }
    }

    fn build_response(&self, req: &HeldWrite, ruser: &[u8], rdata: &[u8]) -> Vec<Packet> {
        let chunk_count = rdata.len().div_ceil(MAX_RESPONSE_DATA);
        let offered = usize::from(response_buffers(req.bitmap));
        let sent = chunk_count.max(1).min(offered);
        let mut responses = Vec::with_capacity(sent);
        for i in 0..sent {
            let start = i * MAX_RESPONSE_DATA;
            let take = (rdata.len() - start).min(MAX_RESPONSE_DATA);
            let last = i + 1 == chunk_count || i + 1 == sent;
            let mut ci = FN_TRESP;
            if req.control & XO_BIT != 0 {
                ci |= XO_BIT;
            }
            if last {
                ci |= EOM_BIT;
            }
            let mut response = Vec::with_capacity(8 + take);
            response.push(ci);
            // The second byte of a TResp is the ATP sequence number of
            // this response packet
            response.push(i as u8);
            response.extend_from_slice(&req.key.2.to_be_bytes());
            if i == 0 {
                response.extend_from_slice(ruser);
            } else {
                response.extend_from_slice(&[0, 0, 0, 0]);
            }
            response.extend_from_slice(&rdata[start..start + take]);
            let datagram = build_short(req.key.1, ATP_SOCKET, ATP_TYPE, &response);
            if crate::wire::wire_debug_enabled() {
                log::info!("ATP response datagram: {:02x?}", datagram);
            }
            responses.push(Packet {
                kind: req.kind,
                node: req.key.0,
                datagram,
            });
        }
        responses
    }
}

/// The number of response packets the requester can receive, read from
/// the TReq bitmap byte: bit n set means n+1 response buffers, so 0x03
/// is two buffers and 0xFF is eight
pub(crate) fn response_buffers(bitmap: u8) -> u8 {
    (0..8)
        .rev()
        .find(|&n| bitmap & (1 << n) != 0)
        .map_or(1, |n| n + 1)
}

/// The socket to send a response to: the requester's socket from the DDP
/// header of the request
fn packet_requester_socket(packet: &Packet) -> u8 {
    if packet.datagram.len() >= 4 {
        packet.datagram[3]
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test responder echoing the request user bytes and data
    struct EchoResponder;

    impl Responder for EchoResponder {
        fn respond(&mut self, _req: &IncomingRequest, user: &[u8], data: &[u8]) -> Option<Respond> {
            Some(Respond::Answer(user.to_vec(), data.to_vec()))
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
    }

    fn treq(tid: u16, control: u8, bitmap: u8, data: &[u8]) -> Packet {
        let mut payload = Vec::with_capacity(8 + data.len());
        payload.push(control);
        payload.push(bitmap); // response buffers
        payload.extend_from_slice(&tid.to_be_bytes());
        payload.extend_from_slice(&[0; 4]); // user bytes
        payload.extend_from_slice(data);
        Packet {
            kind: crate::link::DdpHeaderKind::Short,
            node: 0x20,
            datagram: build_short(ATP_SOCKET, 0x64, ATP_TYPE, &payload),
        }
    }

    fn atp_data(packet: &Packet) -> &[u8] {
        &packet.datagram[5..]
    }

    fn handle(atp: &mut Atp, packet: &Packet) -> Vec<Packet> {
        atp.handle(packet, atp_data(packet), &mut EchoResponder)
            .unwrap_or_default()
    }

    #[test]
    fn test_treq_gets_echo_response() {
        let mut atp = Atp::new();
        let request = treq(0x1234, FN_TREQ, 0x01, b"hello");
        let reply = &handle(&mut atp, &request)[0];
        assert_eq!(reply.datagram[2], 0x64); // dst = requester socket
        assert_eq!(reply.datagram[3], ATP_SOCKET);
        assert_eq!(reply.datagram[4], ATP_TYPE);
        let data = atp_data(reply);
        assert_eq!(data[0], FN_TRESP | EOM_BIT);
        assert_eq!(data[1], 0x00); // sequence number 0: single packet
        assert_eq!(data[2..4], [0x12, 0x34]);
        assert_eq!(&data[8..], b"hello");
    }

    #[test]
    fn test_multipacket_response() {
        let mut atp = Atp::new();
        let body = vec![0xAB; 800];
        let request = treq(0x55, FN_TREQ | XO_BIT, 0x03, &body); // two buffers
        let replies = handle(&mut atp, &request);
        assert_eq!(replies.len(), 2);
        // First packet: sequence 0, no EOM, the user bytes, 578 data
        // bytes (the full packet is 8-byte ATP header + 578 = 586)
        let first = atp_data(&replies[0]);
        assert_eq!(first[0], FN_TRESP | XO_BIT);
        assert_eq!(first[1], 0);
        assert_eq!(first[2..4], [0x00, 0x55]);
        assert_eq!(&first[4..8], &[0, 0, 0, 0]);
        assert_eq!(first.len(), 8 + 578); // ATP header + data
        assert_eq!(&first[8..], &body[..578]);
        // Second packet: sequence 1, EOM, zero user bytes, the rest
        let second = atp_data(&replies[1]);
        assert_eq!(second[0], FN_TRESP | XO_BIT | EOM_BIT);
        assert_eq!(second[1], 1);
        assert_eq!(second[2..4], [0x00, 0x55]);
        assert_eq!(&second[4..8], &[0, 0, 0, 0]);
        assert_eq!(second.len(), 8 + 222); // 800 - 578
        assert_eq!(&second[8..], &body[578..]);
        // A duplicate request replays both cached packets
        let replay = handle(&mut atp, &request);
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].datagram, replies[0].datagram);
        assert_eq!(replay[1].datagram, replies[1].datagram);
    }

    #[test]
    fn test_multipacket_response_capped_at_offered_buffers() {
        let mut atp = Atp::new();
        let body = vec![0xCD; 578 * 3 + 1];
        let request = treq(0x56, FN_TREQ, 0x03, &body); // two buffers only
        let replies = handle(&mut atp, &request);
        assert_eq!(replies.len(), 2);
        assert_eq!(atp_data(&replies[1])[0] & EOM_BIT, EOM_BIT);
    }

    #[test]
    fn test_xo_duplicate_gets_cached_response() {
        let mut atp = Atp::new();
        let request = treq(7, FN_TREQ | XO_BIT, 0x01, b"once");
        let first = handle(&mut atp, &request);
        assert_eq!(atp.xo_cache.len(), 1);
        let second = handle(&mut atp, &request);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].datagram, second[0].datagram);
        assert_eq!(atp_data(&second[0])[0] & 0xC0, FN_TRESP);
        assert_eq!(atp_data(&second[0])[0] & XO_BIT, XO_BIT);
    }

    #[test]
    fn test_retransmit_resends_only_the_missing_packets() {
        let mut atp = Atp::new();
        // A two-packet response (a payload over 578 bytes)
        let payload = vec![b'x'; 700];
        let request = treq(0x42, FN_TREQ | XO_BIT, 0x03, &payload);
        let first = handle(&mut atp, &request);
        assert_eq!(first.len(), 2);
        // The duplicate asks only for packet 1 (bitmap bit 1)
        let dup = treq(0x42, FN_TREQ | XO_BIT, 0x02, &payload);
        let retransmit = handle(&mut atp, &dup);
        assert_eq!(retransmit.len(), 1);
        assert_eq!(atp_data(&retransmit[0])[1], 1); // sequence number 1
        // A duplicate asking for everything gets everything
        let retransmit = handle(&mut atp, &request);
        assert_eq!(retransmit.len(), 2);
    }

    #[test]
    fn test_guest_reboot_purges_that_nodes_cache() {
        let mut atp = Atp::new();
        // node 0x20 (the treq helper) caches a response; another
        // node's entry is separate
        let request = treq(5, FN_TREQ | XO_BIT, 0x01, b"once");
        handle(&mut atp, &request);
        assert_eq!(atp.xo_cache.len(), 1);
        let mut other = treq(9, FN_TREQ | XO_BIT, 0x01, b"twice");
        other.node = 0x21;
        handle(&mut atp, &other);
        assert_eq!(atp.xo_cache.len(), 2);
        // The 0x20 guest reboots: only its entries go
        atp.purge_node(0x20);
        assert_eq!(atp.xo_cache.len(), 1);
        // A recycled TID from the rebooted node executes again
        let reply = handle(&mut atp, &request);
        assert_eq!(reply.len(), 1);
        assert_eq!(atp.xo_cache.len(), 2);
    }

    #[cfg(feature = "test_module")]
    #[test]
    fn test_dropped_response_is_cached_for_retransmit() {
        let mut atp = Atp {
            xo_cache: HashMap::new(),
            drop_responses: 1,
        };
        let request = treq(3, FN_TREQ | XO_BIT, 0x01, b"data");
        // The first response is dropped, as if lost in transit
        assert!(handle(&mut atp, &request).is_empty());
        // The retransmitted request is answered from the cache
        let reply = handle(&mut atp, &request);
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].node, 0x20);
        assert_eq!(&atp_data(&reply[0])[8..], b"data");
    }

    #[test]
    fn test_trel_releases_cached_response() {
        let mut atp = Atp::new();
        let request = treq(9, FN_TREQ | XO_BIT, 0x01, b"data");
        handle(&mut atp, &request);
        let rel = treq(9, FN_TREL, 0x01, b"");
        assert!(handle(&mut atp, &rel).is_empty());
        // A fresh request with the same TID is executed again
        let again = treq(9, FN_TREQ | XO_BIT, 0x01, b"new");
        let reply = handle(&mut atp, &again);
        assert_eq!(&atp_data(&reply[0])[8..], b"new");
    }

    #[test]
    fn test_non_xo_duplicate_is_executed_again() {
        let mut atp = Atp::new();
        let request = treq(5, FN_TREQ, 0x01, b"data");
        handle(&mut atp, &request);
        let reply = handle(&mut atp, &request);
        assert_eq!(atp_data(&reply[0])[0] & XO_BIT, 0);
    }

    #[test]
    fn test_unknown_function_dropped() {
        let mut atp = Atp::new();
        let request = treq(5, 0x00, 0x01, b"data");
        assert!(handle(&mut atp, &request).is_empty());
    }

    /// A responder that holds every request (an ASP Write) and
    /// completes it when the workstation's TResp arrives
    struct HoldResponder {
        key: (u8, u8, u16),
    }

    impl Responder for HoldResponder {
        fn respond(
            &mut self,
            req: &IncomingRequest,
            _user: &[u8],
            _data: &[u8],
        ) -> Option<Respond> {
            self.key = req.key;
            // The WriteContinue: a server-initiated request packet
            Some(Respond::Hold(vec![Packet {
                kind: req.kind,
                node: req.key.0,
                datagram: build_short(req.key.1, ATP_SOCKET, ATP_TYPE, &[0xAA, 0xBB]),
            }]))
        }

        fn tresp(
            &mut self,
            _src_socket: u8,
            _tid: u16,
            _seq: u8,
            eom: bool,
            data: &[u8],
        ) -> Option<TrespOutcome> {
            if !eom {
                return None;
            }
            Some(TrespOutcome::Complete(HeldCompletion {
                held: HeldWrite {
                    key: self.key,
                    control: FN_TREQ | XO_BIT,
                    bitmap: 0x01,
                    kind: DdpHeaderKind::Short,
                },
                ruser: vec![0, 0, 0, 0],
                rdata: data.to_vec(),
            }))
        }
    }

    #[test]
    fn test_held_request_completed_by_a_tresp() {
        let mut atp = Atp::new();
        let mut responder = HoldResponder { key: (0, 0, 0) };
        // The request is held: the WriteContinue packet comes back
        // instead of a response
        let request = treq(0x77, FN_TREQ | XO_BIT, 0x01, b"write");
        let held = atp
            .handle(&request, atp_data(&request), &mut responder)
            .unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(atp_data(&held[0]), &[0xAA, 0xBB]);
        assert!(atp.xo_cache.is_empty());
        // The workstation's TResp completes it: the response carries
        // the command reply (user bytes + data), XO-cached, and a TRel
        // releases the workstation's response cache
        let resp = treq(0x77, FN_TRESP | EOM_BIT, 0x01, b"chunk");
        let reply = atp.handle(&resp, atp_data(&resp), &mut responder).unwrap();
        assert_eq!(reply.len(), 2);
        let body = atp_data(&reply[0]);
        assert_eq!(body[0], FN_TRESP | XO_BIT | EOM_BIT);
        assert_eq!(body[2..4], [0x00, 0x77]); // the held TID
        assert_eq!(&body[4..8], &[0, 0, 0, 0]); // the command result
        assert_eq!(&body[8..], b"chunk");
        // The TRel goes to the workstation with the WriteContinue TID
        let trel = atp_data(&reply[1]);
        assert_eq!(trel[0], FN_TREL);
        assert_eq!(trel[2..4], [0x00, 0x77]);
        assert_eq!(reply[1].datagram[2], 0x64); // dst = the WSS
        assert_eq!(atp.xo_cache.len(), 1);
        // A duplicate of the original request replays the cached
        // response instead of holding again
        let replay = atp
            .handle(&request, atp_data(&request), &mut responder)
            .unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].datagram, reply[0].datagram);
    }

    #[test]
    fn test_responder_drop_gets_no_response() {
        struct NoResponder;

        impl Responder for NoResponder {
            fn respond(
                &mut self,
                _req: &IncomingRequest,
                _user: &[u8],
                _data: &[u8],
            ) -> Option<Respond> {
                None
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
        }

        let mut atp = Atp::new();
        let request = treq(5, FN_TREQ, 0x01, b"data");
        assert!(
            atp.handle(&request, atp_data(&request), &mut NoResponder)
                .is_none()
        );
    }
}
