//! A DHCP and BOOTP server for the emulated system: it gives it its address, the netmask, the
//! gateway as router and DNS server, and a host name.
//!
//! BOOTP is RFC 951, with the vendor extensions of RFC 1497; DHCP is RFC 2131 over the same
//! packet, with the options of RFC 2132. A BOOTP request is a DHCP request without the DHCP
//! message type option, and gets a BOOTP reply. A request whose vendor area is in a vendor's
//! own format gets its vendor area back as it was, as the CMU bootpd does in its "auto" mode:
//! such a client only takes the addresses from the reply. One of zeroes gets RFC 1497 options.

use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, IpAddress, IpProtocol, Ipv4Address,
    Ipv4Packet, UdpPacket,
};

/// UDP port of the server
pub const SERVER_PORT: u16 = 67;
/// UDP port of the client
const CLIENT_PORT: u16 = 68;

/// RFC 1497: the start of the vendor area that holds options
const MAGIC: [u8; 4] = [99, 130, 83, 99];

/// RFC 951: the fixed part of a BOOTP packet, up to the vendor area
const FIXED_LEN: usize = 236;
/// RFC 951: the vendor area is 64 bytes, so a BOOTP packet is at least 300 bytes
const MIN_LEN: usize = 300;

/// RFC 2131 2: the flag asking for broadcast replies
const FLAG_BROADCAST: u16 = 0x8000;

const BOOTREQUEST: u8 = 1;
const BOOTREPLY: u8 = 2;

// RFC 2132 options
const OPT_PAD: u8 = 0;
const OPT_SUBNET_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_HOST_NAME: u8 = 12;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MESSAGE_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_END: u8 = 255;

// RFC 2132 9.6: DHCP message types
const DHCPDISCOVER: u8 = 1;
const DHCPOFFER: u8 = 2;
const DHCPREQUEST: u8 = 3;
const DHCPACK: u8 = 5;
const DHCPNAK: u8 = 6;
const DHCPINFORM: u8 = 8;

/// Lease time handed out, in seconds
const LEASE_SECS: u32 = 24 * 60 * 60;

/// What the server hands out
pub struct Lease {
    /// The emulated system's address
    pub client_ip: Ipv4Address,
    pub netmask: Ipv4Address,
    /// The gateway: server, router and DNS server
    pub gateway_ip: Ipv4Address,
    pub gateway_mac: EthernetAddress,
}

/// Whether a frame from the emulated system is a request to a BOOTP or DHCP server
pub fn is_request(packet: &[u8]) -> bool {
    let Ok(eth) = EthernetFrame::new_checked(packet) else {
        return false;
    };
    if eth.ethertype() != EthernetProtocol::Ipv4 {
        return false;
    }
    let Ok(ip) = Ipv4Packet::new_checked(eth.payload()) else {
        return false;
    };
    if ip.next_header() != IpProtocol::Udp {
        return false;
    }
    let Ok(udp) = UdpPacket::new_checked(ip.payload()) else {
        return false;
    };
    udp.dst_port() == SERVER_PORT
}

/// The options of a request: the DHCP message type and the requested address, if any
fn parse_options(bootp: &[u8]) -> (Option<u8>, Option<Ipv4Address>) {
    let mut message_type = None;
    let mut requested = None;
    if bootp.len() < FIXED_LEN + 4 || bootp[FIXED_LEN..FIXED_LEN + 4] != MAGIC {
        return (None, None);
    }
    let mut opts = &bootp[FIXED_LEN + 4..];
    while let [code, rest @ ..] = opts {
        match *code {
            OPT_PAD => opts = rest,
            OPT_END => break,
            _ => {
                let [len, rest @ ..] = rest else { break };
                let len = usize::from(*len);
                if rest.len() < len {
                    break;
                }
                let value = &rest[..len];
                match *code {
                    OPT_MESSAGE_TYPE if len == 1 => message_type = Some(value[0]),
                    OPT_REQUESTED_IP if len == 4 => {
                        requested = Some(Ipv4Address::new(value[0], value[1], value[2], value[3]));
                    }
                    _ => (),
                }
                opts = &rest[len..];
            }
        }
    }
    (message_type, requested)
}

fn push_option(opts: &mut Vec<u8>, code: u8, value: &[u8]) {
    opts.push(code);
    opts.push(value.len() as u8);
    opts.extend_from_slice(value);
}

/// The reply to a request, as an Ethernet frame, or None for a request that gets none
pub fn reply(packet: &[u8], lease: &Lease) -> Option<Vec<u8>> {
    let eth = EthernetFrame::new_checked(packet).ok()?;
    let ip = Ipv4Packet::new_checked(eth.payload()).ok()?;
    let udp = UdpPacket::new_checked(ip.payload()).ok()?;
    let req = udp.payload();
    if req.len() < FIXED_LEN || req[0] != BOOTREQUEST || req[1] != 1 || req[2] != 6 {
        return None;
    }
    let xid = &req[4..8];
    let flags = u16::from_be_bytes([req[10], req[11]]);
    let ciaddr = Ipv4Address::new(req[12], req[13], req[14], req[15]);
    let chaddr = EthernetAddress::from_bytes(&req[28..34]);

    let (message_type, requested) = parse_options(req);
    let reply_type = match message_type {
        None => None,
        Some(DHCPDISCOVER) => Some(DHCPOFFER),
        Some(DHCPREQUEST) => {
            // RFC 2131 4.3.2: a client that asks for another address (from another network)
            // is told no, and starts again
            let asked = requested.unwrap_or(ciaddr);
            if asked == lease.client_ip || asked.is_unspecified() {
                Some(DHCPACK)
            } else {
                Some(DHCPNAK)
            }
        }
        Some(DHCPINFORM) => Some(DHCPACK),
        // DECLINE, RELEASE: nothing to answer
        Some(_) => return None,
    };
    let nak = reply_type == Some(DHCPNAK);
    let inform = message_type == Some(DHCPINFORM);

    let mut bootp = vec![0u8; FIXED_LEN];
    bootp[0] = BOOTREPLY;
    bootp[1] = 1;
    bootp[2] = 6;
    bootp[4..8].copy_from_slice(xid);
    bootp[10..12].copy_from_slice(&flags.to_be_bytes());
    bootp[12..16].copy_from_slice(&ciaddr.octets());
    let yiaddr = if nak || inform {
        Ipv4Address::UNSPECIFIED
    } else {
        lease.client_ip
    };
    bootp[16..20].copy_from_slice(&yiaddr.octets());
    // The server is also the "next server" of BOOTP
    if !nak {
        bootp[20..24].copy_from_slice(&lease.gateway_ip.octets());
    }
    bootp[28..34].copy_from_slice(chaddr.as_bytes());
    bootp[44..48].copy_from_slice(b"snow");

    // A vendor area of zeroes asks for nothing in particular: it gets RFC 1497 options, as
    // from the CMU bootpd
    let magic = req.get(FIXED_LEN..FIXED_LEN + 4).unwrap_or(&[0; 4]);
    if magic != MAGIC && magic != [0; 4] {
        bootp.extend_from_slice(&req[FIXED_LEN..req.len().min(MIN_LEN)]);
        bootp.resize(MIN_LEN, 0);
        return Some(frame(lease, chaddr, lease.client_ip, &bootp));
    }
    bootp.extend_from_slice(&MAGIC);
    let gw = lease.gateway_ip.octets();
    if let Some(t) = reply_type {
        push_option(&mut bootp, OPT_MESSAGE_TYPE, &[t]);
        push_option(&mut bootp, OPT_SERVER_ID, &gw);
    }
    if !nak {
        push_option(&mut bootp, OPT_SUBNET_MASK, &lease.netmask.octets());
        push_option(&mut bootp, OPT_ROUTER, &gw);
        push_option(&mut bootp, OPT_DNS, &gw);
        let mac = chaddr.as_bytes();
        let name = format!("snow-{:02x}{:02x}{:02x}", mac[3], mac[4], mac[5]);
        push_option(&mut bootp, OPT_HOST_NAME, name.as_bytes());
        if reply_type.is_some() && !inform {
            push_option(&mut bootp, OPT_LEASE_TIME, &LEASE_SECS.to_be_bytes());
        }
    }
    bootp.push(OPT_END);
    if bootp.len() < MIN_LEN {
        bootp.resize(MIN_LEN, OPT_PAD);
    }

    // RFC 2131 4.1: to the client's address if it has one, broadcast if it asks for it or
    // for a NAK, else to its hardware address and the address it is given
    let (dst_mac, dst_ip) = if !ciaddr.is_unspecified() {
        (chaddr, ciaddr)
    } else if flags & FLAG_BROADCAST != 0 || nak {
        (EthernetAddress::BROADCAST, Ipv4Address::BROADCAST)
    } else {
        (chaddr, yiaddr)
    };
    Some(frame(lease, dst_mac, dst_ip, &bootp))
}

/// An Ethernet frame from the gateway's server port to the client port, around a BOOTP packet
fn frame(lease: &Lease, dst_mac: EthernetAddress, dst_ip: Ipv4Address, bootp: &[u8]) -> Vec<u8> {
    let udp_len = 8 + bootp.len();
    let ip_len = 20 + udp_len;
    let mut buf = vec![0u8; 14 + ip_len];

    let mut eth = EthernetFrame::new_unchecked(&mut buf[..]);
    eth.set_dst_addr(dst_mac);
    eth.set_src_addr(lease.gateway_mac);
    eth.set_ethertype(EthernetProtocol::Ipv4);

    let mut ip = Ipv4Packet::new_unchecked(&mut buf[14..]);
    ip.set_version(4);
    ip.set_header_len(20);
    ip.set_total_len(ip_len as u16);
    ip.set_hop_limit(64);
    ip.set_next_header(IpProtocol::Udp);
    ip.set_src_addr(lease.gateway_ip);
    ip.set_dst_addr(dst_ip);
    ip.fill_checksum();

    let mut udp = UdpPacket::new_unchecked(&mut buf[34..]);
    udp.set_src_port(SERVER_PORT);
    udp.set_dst_port(CLIENT_PORT);
    udp.set_len(udp_len as u16);
    udp.payload_mut().copy_from_slice(bootp);
    udp.fill_checksum(&IpAddress::Ipv4(lease.gateway_ip), &IpAddress::Ipv4(dst_ip));
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT_MAC: [u8; 6] = [0x00, 0x00, 0x0F, 0x12, 0x34, 0x56];

    fn lease() -> Lease {
        Lease {
            client_ip: Ipv4Address::new(10, 0, 0, 2),
            netmask: Ipv4Address::new(255, 0, 0, 0),
            gateway_ip: Ipv4Address::new(10, 0, 0, 1),
            gateway_mac: EthernetAddress([0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55]),
        }
    }

    /// A request from an unconfigured client, broadcast, with these options (none: BOOTP)
    fn request(options: Option<&[u8]>) -> Vec<u8> {
        let mut bootp = vec![0u8; FIXED_LEN];
        bootp[0] = BOOTREQUEST;
        bootp[1] = 1;
        bootp[2] = 6;
        bootp[4..8].copy_from_slice(&[1, 2, 3, 4]);
        bootp[28..34].copy_from_slice(&CLIENT_MAC);
        if let Some(opts) = options {
            bootp.extend_from_slice(&MAGIC);
            bootp.extend_from_slice(opts);
            bootp.push(OPT_END);
        }
        bootp.resize(MIN_LEN, 0);
        let client = Lease {
            client_ip: Ipv4Address::UNSPECIFIED,
            netmask: Ipv4Address::UNSPECIFIED,
            gateway_ip: Ipv4Address::UNSPECIFIED,
            gateway_mac: EthernetAddress(CLIENT_MAC),
        };
        let mut f = frame(
            &client,
            EthernetAddress::BROADCAST,
            Ipv4Address::BROADCAST,
            &bootp,
        );
        // From the client port to the server port
        let mut udp = UdpPacket::new_unchecked(&mut f[34..]);
        udp.set_src_port(CLIENT_PORT);
        udp.set_dst_port(SERVER_PORT);
        f
    }

    /// The BOOTP packet of a reply, and its options by code
    fn parse_reply(f: &[u8]) -> (Vec<u8>, Vec<(u8, Vec<u8>)>) {
        let eth = EthernetFrame::new_checked(f).unwrap();
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert!(ip.verify_checksum());
        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        assert_eq!((udp.src_port(), udp.dst_port()), (SERVER_PORT, CLIENT_PORT));
        let bootp = udp.payload().to_vec();
        assert_eq!(bootp[..FIXED_LEN + 4][FIXED_LEN..], MAGIC);
        let mut opts = vec![];
        let mut i = FIXED_LEN + 4;
        while bootp[i] != OPT_END {
            let (code, len) = (bootp[i], usize::from(bootp[i + 1]));
            opts.push((code, bootp[i + 2..i + 2 + len].to_vec()));
            i += 2 + len;
        }
        (bootp, opts)
    }

    fn option(opts: &[(u8, Vec<u8>)], code: u8) -> Option<Vec<u8>> {
        opts.iter()
            .find(|(c, _)| *c == code)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn a_bootp_request_gets_a_bootp_reply() {
        let req = request(None);
        assert!(is_request(&req));
        let reply = reply(&req, &lease()).unwrap();
        let (bootp, opts) = parse_reply(&reply);
        assert!(bootp.len() >= MIN_LEN);
        assert_eq!(bootp[0], BOOTREPLY);
        assert_eq!(bootp[4..8], [1, 2, 3, 4]);
        assert_eq!(bootp[16..20], [10, 0, 0, 2]);
        assert_eq!(bootp[28..34], CLIENT_MAC);
        assert_eq!(option(&opts, OPT_MESSAGE_TYPE), None);
        assert_eq!(option(&opts, OPT_SUBNET_MASK), Some(vec![255, 0, 0, 0]));
        assert_eq!(option(&opts, OPT_ROUTER), Some(vec![10, 0, 0, 1]));
        assert_eq!(option(&opts, OPT_DNS), Some(vec![10, 0, 0, 1]));
        assert_eq!(option(&opts, OPT_HOST_NAME), Some(b"snow-123456".to_vec()));
        // Unicast to the client's hardware address and its new address
        let eth = EthernetFrame::new_checked(&reply[..]).unwrap();
        assert_eq!(eth.dst_addr(), EthernetAddress(CLIENT_MAC));
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert_eq!(ip.dst_addr(), Ipv4Address::new(10, 0, 0, 2));
    }

    #[test]
    fn discover_and_request_get_offer_and_ack() {
        let offer = reply(
            &request(Some(&[OPT_MESSAGE_TYPE, 1, DHCPDISCOVER])),
            &lease(),
        )
        .unwrap();
        let (_, opts) = parse_reply(&offer);
        assert_eq!(option(&opts, OPT_MESSAGE_TYPE), Some(vec![DHCPOFFER]));
        assert_eq!(option(&opts, OPT_SERVER_ID), Some(vec![10, 0, 0, 1]));
        assert_eq!(
            option(&opts, OPT_LEASE_TIME),
            Some(LEASE_SECS.to_be_bytes().to_vec())
        );

        let ack = reply(
            &request(Some(&[
                OPT_MESSAGE_TYPE,
                1,
                DHCPREQUEST,
                OPT_REQUESTED_IP,
                4,
                10,
                0,
                0,
                2,
            ])),
            &lease(),
        )
        .unwrap();
        let (bootp, opts) = parse_reply(&ack);
        assert_eq!(option(&opts, OPT_MESSAGE_TYPE), Some(vec![DHCPACK]));
        assert_eq!(bootp[16..20], [10, 0, 0, 2]);
    }

    #[test]
    fn a_request_for_another_address_gets_a_nak() {
        let nak = reply(
            &request(Some(&[
                OPT_MESSAGE_TYPE,
                1,
                DHCPREQUEST,
                OPT_REQUESTED_IP,
                4,
                192,
                168,
                1,
                50,
            ])),
            &lease(),
        )
        .unwrap();
        let (bootp, opts) = parse_reply(&nak);
        assert_eq!(option(&opts, OPT_MESSAGE_TYPE), Some(vec![DHCPNAK]));
        assert_eq!(bootp[16..20], [0, 0, 0, 0]);
        let eth = EthernetFrame::new_checked(&nak[..]).unwrap();
        assert_eq!(eth.dst_addr(), EthernetAddress::BROADCAST);
    }

    #[test]
    fn another_vendor_area_comes_back_as_it_was() {
        let mut req = request(None);
        // A vendor's own format: its tag, then a version
        req[42 + FIXED_LEN..42 + FIXED_LEN + 5].copy_from_slice(b"VEND\x01");
        let reply = reply(&req, &lease()).unwrap();
        let eth = EthernetFrame::new_checked(&reply[..]).unwrap();
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        let bootp = udp.payload();
        assert_eq!(bootp.len(), MIN_LEN);
        assert_eq!(bootp[16..20], [10, 0, 0, 2]);
        assert_eq!(&bootp[FIXED_LEN..FIXED_LEN + 5], b"VEND\x01");
        assert_eq!(eth.dst_addr(), EthernetAddress(CLIENT_MAC));
    }

    #[test]
    fn release_gets_no_reply() {
        assert!(reply(&request(Some(&[OPT_MESSAGE_TYPE, 1, 7])), &lease()).is_none());
    }
}
