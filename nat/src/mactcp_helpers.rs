//! MacTCP Helpers
//!
//! This module enables helpers that are useful for MacTCP
//! - ICMP delay
//! - ICMP proxy-ish (handle Address Mask Requests)
//! - RARP server

use anyhow::Result;
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    ArpHardware, ArpOperation, ArpPacket, EthernetAddress, EthernetFrame, EthernetProtocol,
    Icmpv4Message, Icmpv4Packet, IpAddress, IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr,
    TcpPacket, TcpSeqNumber,
};
use std::time::Duration;

/// Amount of time we will throttle smoltcp TX before sending the ICMP reply
const ICMP_REPLY_DELAY: Duration = Duration::from_millis(4);

/// Put the TX thread into sleep for x ms if it's an ICMP reply
pub fn delay_icmp_reply(buffer: &[u8]) {
    let eth_hlen = EthernetFrame::<&[u8]>::header_len();
    if buffer.len() <= eth_hlen {
        return;
    }
    if EthernetFrame::new_unchecked(buffer).ethertype() != EthernetProtocol::Ipv4 {
        return;
    }
    let ipv4 = match Ipv4Packet::new_checked(&buffer[eth_hlen..]) {
        Ok(p) => p,
        Err(_) => return,
    };
    if ipv4.next_header() == IpProtocol::Icmp {
        std::thread::sleep(ICMP_REPLY_DELAY);
    }
}

/// Validate a RARP request and return the reply buffer, or None if not applicable
pub fn handle_rarp_request(
    eth_frame: &EthernetFrame<&[u8]>,
    gateway_ip: Ipv4Address,
    gateway_mac: EthernetAddress,
) -> Result<Option<Vec<u8>>> {
    let rarp_packet = ArpPacket::new_checked(eth_frame.payload()).expect("Bad RARP packet");

    if rarp_packet.operation() != ArpOperation::from(3u16)
        || rarp_packet.hardware_type() != ArpHardware::Ethernet
        || rarp_packet.hardware_len() != 6
        || rarp_packet.protocol_len() != 4
    {
        return Ok(None);
    }

    let client_mac = EthernetAddress::from_bytes(rarp_packet.target_hardware_addr());

    // IP will be Gateway IP + 1 (e.g: GW 10.0.0.1 -> Mac 10.0.0.2)
    let client_ip = Ipv4Address::from(
        u32::from_be_bytes(gateway_ip.octets())
            .wrapping_add(1)
            .to_be_bytes(),
    );

    let buf = build_rarp_reply(client_mac, gateway_mac, gateway_ip);
    log::info!("RARP: assigned {} to {}", client_ip, client_mac);
    Ok(Some(buf))
}

/// Returns true if the packet is an ICMP Address Mask Request (type 17)
pub fn needs_icmp_proxy(packet: &[u8]) -> bool {
    let eth_hlen = EthernetFrame::<&[u8]>::header_len();
    if packet.len() <= eth_hlen {
        return false;
    }
    if EthernetFrame::new_unchecked(packet).ethertype() != EthernetProtocol::Ipv4 {
        return false;
    }
    let ipv4 = match Ipv4Packet::new_checked(&packet[eth_hlen..]) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if ipv4.next_header() != IpProtocol::Icmp {
        return false;
    }
    Icmpv4Packet::new_checked(ipv4.payload())
        .map(|icmp| icmp.msg_type() == Icmpv4Message::Unknown(17))
        .unwrap_or(false)
}

/// Validate an ICMP Address Mask Request (type 17) and return the reply frame if ok
pub fn handle_icmp_address_mask_request(
    eth_frame: &EthernetFrame<&[u8]>,
    gateway_ip: Ipv4Address,
    gateway_mac: EthernetAddress,
    subnet_prefix: u8,
) -> Result<Option<Vec<u8>>> {
    let ipv4 = match Ipv4Packet::new_checked(eth_frame.payload()) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    if ipv4.next_header() != IpProtocol::Icmp {
        return Ok(None);
    }
    let icmp = match Icmpv4Packet::new_checked(ipv4.payload()) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    if icmp.msg_type() != Icmpv4Message::Unknown(17) || icmp.msg_code() != 0 {
        return Ok(None);
    }

    let identifier = icmp.echo_ident();
    let seq_no = icmp.echo_seq_no();

    // It would be a better idea to be classful here, but the code in ethernet.rs is
    // classless so let's follow that
    let address_mask: u32 = if subnet_prefix == 0 {
        0
    } else {
        !0u32 << (32 - subnet_prefix as u32)
    };

    let buf = build_icmp_address_mask_reply(
        eth_frame.src_addr(),
        gateway_ip,
        gateway_mac,
        address_mask,
        identifier,
        seq_no,
    );
    log::info!(
        "ICMP: address mask request from {}, replying with {}",
        ipv4.src_addr(),
        Ipv4Address::from(address_mask.to_be_bytes())
    );
    Ok(Some(buf))
}

/// Build a full Ethernet + IPv4 + ICMP Address Mask Reply frame because smoltcp
/// doesn't handle deprecated prehistoric stuff
/// No idea if the answer should be unicast or broadcast, broadcast works for MacTCP
fn build_icmp_address_mask_reply(
    eth_dst: EthernetAddress,
    gateway_ip: Ipv4Address,
    gateway_mac: EthernetAddress,
    address_mask: u32,
    identifier: u16,
    seq_no: u16,
) -> Vec<u8> {
    const ICMP_LEN: usize = 12;
    let eth_hlen = EthernetFrame::<&[u8]>::header_len();
    let ip_repr = Ipv4Repr {
        src_addr: gateway_ip,
        dst_addr: Ipv4Address::BROADCAST,
        next_header: IpProtocol::Icmp,
        payload_len: ICMP_LEN,
        hop_limit: 1,
    };
    let total = eth_hlen + ip_repr.buffer_len() + ICMP_LEN;
    let mut buf = vec![0u8; total];

    let mut eth = EthernetFrame::new_unchecked(&mut buf);
    eth.set_dst_addr(eth_dst);
    eth.set_src_addr(gateway_mac);
    eth.set_ethertype(EthernetProtocol::Ipv4);

    let mut ipv4 = Ipv4Packet::new_unchecked(&mut buf[eth_hlen..]);
    ip_repr.emit(&mut ipv4, &ChecksumCapabilities::default());

    let icmp_start = eth_hlen + ip_repr.buffer_len();
    let icmp_buf = &mut buf[icmp_start..];
    icmp_buf[0] = 18;
    icmp_buf[1] = 0;
    icmp_buf[2] = 0;
    icmp_buf[3] = 0;
    icmp_buf[4..6].copy_from_slice(&identifier.to_be_bytes());
    icmp_buf[6..8].copy_from_slice(&seq_no.to_be_bytes());
    icmp_buf[8..12].copy_from_slice(&address_mask.to_be_bytes());
    Icmpv4Packet::new_unchecked(icmp_buf).fill_checksum();

    buf
}

/// Build a spoofed TCP RST+ACK frame from the remote endpoint, so MacTCP doesn't
/// hang while doing SYN retransmits
pub fn build_tcp_rst(
    eth_dst: EthernetAddress,
    gateway_mac: EthernetAddress,
    src_ip: Ipv4Address,
    dst_ip: Ipv4Address,
    src_port: u16,
    dst_port: u16,
    ack_num: u32,
) -> Vec<u8> {
    const TCP_HEADER_LEN: usize = 20;
    let eth_hlen = EthernetFrame::<&[u8]>::header_len();
    let ip_repr = Ipv4Repr {
        src_addr: src_ip,
        dst_addr: dst_ip,
        next_header: IpProtocol::Tcp,
        payload_len: TCP_HEADER_LEN,
        hop_limit: 64,
    };
    let total = eth_hlen + ip_repr.buffer_len() + TCP_HEADER_LEN;
    let mut buf = vec![0u8; total];

    let mut eth = EthernetFrame::new_unchecked(&mut buf);
    eth.set_dst_addr(eth_dst);
    eth.set_src_addr(gateway_mac);
    eth.set_ethertype(EthernetProtocol::Ipv4);

    let mut ipv4 = Ipv4Packet::new_unchecked(&mut buf[eth_hlen..]);
    ip_repr.emit(&mut ipv4, &ChecksumCapabilities::default());

    let tcp_start = eth_hlen + ip_repr.buffer_len();
    let mut tcp = TcpPacket::new_unchecked(&mut buf[tcp_start..]);
    tcp.set_src_port(src_port);
    tcp.set_dst_port(dst_port);
    tcp.set_seq_number(TcpSeqNumber(0));
    tcp.set_ack_number(TcpSeqNumber(ack_num as i32));
    tcp.set_header_len(20);
    tcp.set_rst(true);
    tcp.set_ack(true);
    tcp.set_window_len(0);
    tcp.fill_checksum(&IpAddress::Ipv4(src_ip), &IpAddress::Ipv4(dst_ip));

    buf
}

/// Build a RARP reply frame with the specified IP.
fn build_rarp_reply(
    client_mac: EthernetAddress,
    gateway_mac: EthernetAddress,
    gateway_ip: Ipv4Address,
) -> Vec<u8> {
    let client_ip = u32::from_be_bytes(gateway_ip.octets())
        .wrapping_add(1)
        .to_be_bytes();

    let mut buf = vec![0u8; 42];

    let mut frame = EthernetFrame::new_unchecked(&mut buf);
    frame.set_dst_addr(client_mac);
    frame.set_src_addr(gateway_mac);
    frame.set_ethertype(EthernetProtocol::Unknown(0x8035));

    let mut reply = ArpPacket::new_unchecked(frame.payload_mut());
    reply.set_hardware_type(ArpHardware::Ethernet);
    reply.set_protocol_type(EthernetProtocol::Ipv4);
    reply.set_hardware_len(6);
    reply.set_protocol_len(4);
    reply.set_operation(ArpOperation::from(4u16));
    reply.set_source_hardware_addr(gateway_mac.as_bytes());
    reply.set_source_protocol_addr(&gateway_ip.octets());
    reply.set_target_hardware_addr(client_mac.as_bytes());
    reply.set_target_protocol_addr(&client_ip);

    buf
}
