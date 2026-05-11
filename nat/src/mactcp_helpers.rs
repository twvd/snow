//! MacTCP Helpers
//!
//! This module enables helpers that are useful for MacTCP
//! - RARP server

use anyhow::Result;
use smoltcp::wire::{
    ArpHardware, ArpOperation, ArpPacket, EthernetAddress, EthernetFrame, EthernetProtocol,
    Ipv4Address,
};

/// Validate a RARP request and return the reply buffer, or None if not applicable
pub fn handle_rarp_request(
    eth_frame: &EthernetFrame<&[u8]>,
    gateway_ip: Ipv4Address,
    gateway_mac: EthernetAddress,
) -> Result<Option<Vec<u8>>> {
    let arp_packet = ArpPacket::new_checked(eth_frame.payload())
        .map_err(|e| anyhow::anyhow!("Bad RARP packet: {:?}", e))?;

    if arp_packet.operation() != ArpOperation::from(3u16)
        || arp_packet.hardware_type() != ArpHardware::Ethernet
        || arp_packet.hardware_len() != 6
        || arp_packet.protocol_len() != 4
    {
        return Ok(None);
    }

    let client_mac = EthernetAddress::from_bytes(arp_packet.target_hardware_addr());
    let client_ip = Ipv4Address::from(
        u32::from_be_bytes(gateway_ip.octets())
            .wrapping_add(1)
            .to_be_bytes(),
    );
    let buf = build_rarp_reply(client_mac, gateway_mac, gateway_ip);
    log::info!("RARP: assigned {} to {}", client_ip, client_mac);
    Ok(Some(buf))
}

/// Build a RARP reply frame. IP will be Gateway IP + 1 (e.g: GW 10.0.0.1 -> Mac 10.0.0.2)
pub fn build_rarp_reply(
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
