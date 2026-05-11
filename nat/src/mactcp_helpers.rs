//! MacTCP Helpers
//!
//! This module enables helpers that are useful for MacTCP
//! - ICMP delay
//! - RARP server

use anyhow::Result;
use smoltcp::wire::{
    ArpHardware, ArpOperation, ArpPacket, EthernetAddress, EthernetFrame, EthernetProtocol,
    IpProtocol, Ipv4Address, Ipv4Packet,
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
    {
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
    }
    buf
}
