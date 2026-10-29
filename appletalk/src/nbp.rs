//! Name Binding Protocol (NBP)
//!
//! See Inside AppleTalk, 2nd edition, chapter 7.
//!
//! The node registers its entities in a names table and answers LookUp
//! (LkUp) and Broadcast Request (BrdRq) packets with a LookUp-Reply
//! (LkUp-Reply) containing the matching entity tuples.

use crate::ddp::build_short;
use crate::link::Packet;

/// NBP socket: well-known socket number of the Name Information Socket
pub const NBP_SOCKET: u8 = 2;

/// DDP type of NBP packets
pub const NBP_TYPE: u8 = 2;

/// NBP packet functions (high nibble of the first byte)
const FN_BRD_RQ: u8 = 1;
const FN_LK_UP: u8 = 2;
const FN_LK_UP_REPLY: u8 = 3;

/// A registered entity name (object:type@zone) with its socket address
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NbpEntity {
    pub object: String,
    pub entity_type: String,
    pub zone: String,
    /// Socket of the entity, in the tuple of a reply
    pub socket: u8,
    /// Enumerator, used by requesters to filter duplicate replies
    pub enumerator: u8,
}

/// NBP protocol module: the node's names table and request handling
pub struct Nbp {
    /// Node ID of this node, used in reply tuples
    node: u8,
    /// Network number of this node (0 = nonextended network)
    net: u16,
    entities: Vec<NbpEntity>,
}

impl Nbp {
    pub fn new(node: u8, net: u16, entities: Vec<NbpEntity>) -> Self {
        Self {
            node,
            net,
            entities,
        }
    }

    /// Handle an NBP packet, returning the reply datagram if any
    pub fn handle(&self, packet: &Packet, data: &[u8]) -> Option<Packet> {
        if data.len() < 2 {
            log::debug!("Dropped short NBP packet ({} bytes)", data.len());
            return None;
        }

        let function = data[0] >> 4;
        if function != FN_BRD_RQ && function != FN_LK_UP {
            log::debug!("Dropped NBP packet with function {}", function);
            return None;
        }
        let nbp_id = data[1];
        let tuple_count = data[0] & 0x0F;

        let mut reply = Vec::new();
        for (object, entity_type, zone) in parse_request_tuples(&data[2..], tuple_count) {
            for entity in &self.entities {
                if nbp_name_match(&object, &entity.object)
                    && nbp_name_match(&entity_type, &entity.entity_type)
                    && nbp_name_match(&zone, &entity.zone)
                {
                    reply.extend_from_slice(&self.net.to_be_bytes());
                    reply.push(self.node);
                    reply.push(entity.socket);
                    reply.push(entity.enumerator);
                    reply.extend_from_slice(&nbp_string(&entity.object));
                    reply.extend_from_slice(&nbp_string(&entity.entity_type));
                    reply.extend_from_slice(&nbp_string(&entity.zone));
                }
            }
        }
        if reply.is_empty() {
            return None;
        }

        log::debug!(
            "Replying to NBP lookup (function {}, {} bytes of tuples)",
            function,
            reply.len()
        );
        let mut nbp = Vec::with_capacity(2 + reply.len());
        nbp.push((FN_LK_UP_REPLY << 4) | 1);
        nbp.push(nbp_id);
        nbp.extend_from_slice(&reply);
        Some(Packet {
            kind: packet.kind,
            node: packet.node,
            datagram: build_short(packet_requester_socket(packet), NBP_SOCKET, NBP_TYPE, &nbp),
        })
    }
}

/// Parse the tuples of a LkUp/BrdRq packet: the object, type and zone
/// name of each tuple. The address fields of the request tuples carry
/// the requester's address and are not needed for matching.
fn parse_request_tuples(mut data: &[u8], count: u8) -> Vec<(String, String, String)> {
    let mut tuples = Vec::new();
    for _ in 0..count {
        // 5-byte address field plus at least one length byte
        if data.len() < 6 {
            break;
        }
        let Some((object, rest)) = parse_nbp_string(&data[5..]) else {
            break;
        };
        let Some((entity_type, rest)) = parse_nbp_string(rest) else {
            break;
        };
        let Some((zone, rest)) = parse_nbp_string(rest) else {
            break;
        };
        tuples.push((object, entity_type, zone));
        data = rest;
    }
    tuples
}

/// The socket to send a reply to: the requester's socket from the DDP
/// header of the request
fn packet_requester_socket(packet: &Packet) -> u8 {
    if packet.datagram.len() >= 4 {
        packet.datagram[3]
    } else {
        0
    }
}

/// Parse a Pascal string (length byte followed by the bytes)
fn parse_nbp_string(data: &[u8]) -> Option<(String, &[u8])> {
    let len = usize::from(*data.first()?);
    if data.len() < len + 1 {
        return None;
    }
    let s = String::from_utf8_lossy(&data[1..=len]).into_owned();
    Some((s, &data[len + 1..]))
}

/// Encode a name as a Pascal string
fn nbp_string(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
    out
}

/// Match an NBP name against a registered one. A request name of "="
/// (object and type fields) or "*" (zone field) is a wildcard, and a
/// null zone matches any zone.
fn nbp_name_match(request: &str, registered: &str) -> bool {
    request == "=" || request == "*" || request.is_empty() || request == registered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity() -> NbpEntity {
        NbpEntity {
            object: "Snow".to_string(),
            entity_type: "AFPServer".to_string(),
            zone: "*".to_string(),
            socket: NBP_SOCKET,
            enumerator: 1,
        }
    }

    fn nbp(net: u16) -> Nbp {
        Nbp::new(0xD7, net, vec![entity()])
    }

    fn request(function: u8, object: &str, entity_type: &str, zone: &str) -> Packet {
        let mut payload = vec![(function << 4) | 1, 0x42];
        payload.extend_from_slice(&0u16.to_be_bytes());
        payload.push(1);
        payload.push(0x64);
        payload.push(0);
        payload.extend_from_slice(&nbp_string(object));
        payload.extend_from_slice(&nbp_string(entity_type));
        payload.extend_from_slice(&nbp_string(zone));
        Packet {
            kind: crate::link::DdpHeaderKind::Short,
            node: 1,
            datagram: build_short(NBP_SOCKET, 0x64, NBP_TYPE, &payload),
        }
    }

    fn handle(nbp: &Nbp, packet: &Packet) -> Option<Packet> {
        nbp.handle(packet, &packet.datagram[5..])
    }

    #[test]
    fn test_brd_rq_gets_reply() {
        let packet = request(FN_BRD_RQ, "=", "=", "*");
        let reply = handle(&nbp(0), &packet).unwrap();
        // The reply datagram: DDP dst socket = requester socket, src = NBP
        assert_eq!(reply.datagram[2], 0x64);
        assert_eq!(reply.datagram[3], NBP_SOCKET);
        assert_eq!(reply.datagram[4], NBP_TYPE);
        // NBP header: LkUp-Reply, one tuple, same NBP ID
        let data = &reply.datagram[5..];
        assert_eq!(data[0], (FN_LK_UP_REPLY << 4) | 1);
        assert_eq!(data[1], 0x42);
        // Tuple: network 0, node 0xD7, socket 2, enumerator 1
        assert_eq!(&data[2..6], &[0, 0, 0xD7, NBP_SOCKET]);
        assert_eq!(data[6], 1);
        // Names: object "Snow", type "AFPServer", zone "*"
        assert_eq!(data[7], 4);
        assert_eq!(&data[8..12], b"Snow");
        assert_eq!(data[12], 9);
        assert_eq!(&data[13..22], b"AFPServer");
        assert_eq!(data[22], 1);
        assert_eq!(&data[23..24], b"*");
        assert_eq!(data.len(), 24);
    }

    #[test]
    fn test_lk_up_exact_match() {
        let packet = request(FN_LK_UP, "Snow", "AFPServer", "*");
        assert!(handle(&nbp(0), &packet).is_some());
    }

    #[test]
    fn test_lk_up_no_match() {
        let packet = request(FN_LK_UP, "Other", "AFPServer", "*");
        assert!(handle(&nbp(0), &packet).is_none());
    }

    #[test]
    fn test_lk_up_wildcard_match() {
        let packet = request(FN_LK_UP, "=", "AFPServer", "*");
        assert!(handle(&nbp(0), &packet).is_some());
    }

    #[test]
    fn test_lk_up_null_zone_matches() {
        let packet = request(FN_LK_UP, "Snow", "AFPServer", "");
        assert!(handle(&nbp(0), &packet).is_some());
    }

    #[test]
    fn test_name_matching() {
        assert!(nbp_name_match("=", "anything"));
        assert!(nbp_name_match("*", "anyzone"));
        assert!(nbp_name_match("", "anyzone"));
        assert!(nbp_name_match("Snow", "Snow"));
        assert!(!nbp_name_match("Other", "Snow"));
    }
}
