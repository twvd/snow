//! NAT implementation for Snow
//!
//! This module provides Network Address Translation (NAT) functionality
//! for the emulated Ethernet interface. It receives layer 2 packets from
//! the emulator via a crossbeam channel, processes them through a NAT
//! implementation using smoltcp, and sends responses back.

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr};

/// A layer 2 Ethernet packet
pub type Packet = Vec<u8>;

/// Virtual network device that bridges crossbeam channels with smoltcp
struct VirtualDevice {
    rx: Receiver<Packet>,
    tx: Sender<Packet>,
}

impl VirtualDevice {
    fn new(tx: Sender<Packet>, rx: Receiver<Packet>) -> Self {
        Self { rx, tx }
    }
}

impl Device for VirtualDevice {
    type RxToken<'a>
        = VirtualRxToken
    where
        Self: 'a;
    type TxToken<'a>
        = VirtualTxToken
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // Receive packet with timeout and take ownership
        let packet = self.rx.recv_timeout(Duration::from_millis(100)).ok()?;
        Some((
            VirtualRxToken { buffer: packet },
            VirtualTxToken {
                tx: self.tx.clone(),
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(VirtualTxToken {
            tx: self.tx.clone(),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps
    }
}

struct VirtualRxToken {
    buffer: Packet,
}

impl RxToken for VirtualRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buffer)
    }
}

struct VirtualTxToken {
    tx: Sender<Packet>,
}

impl TxToken for VirtualTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);

        // Send the packet back to the emulator
        self.tx.send(buffer).unwrap();

        result
    }
}

/// NAT instance for handling network address translation
pub struct NatEngine {
    /// Virtual network device
    device: VirtualDevice,

    /// smoltcp network interface
    iface: Interface,

    /// smoltcp socket set
    sockets: SocketSet<'static>,

    /// Gateway MAC address
    gateway_mac: EthernetAddress,

    /// Gateway IP address
    gateway_ip: IpAddress,
}

impl NatEngine {
    /// Creates a new NAT engine instance with the given TX/RX channels
    ///
    /// * `tx` - Sender channel for packets going to the emulator (network -> Mac)
    /// * `rx` - Receiver channel for packets coming from the emulator (Mac -> network)
    /// * `gateway_mac` - MAC address of the NAT gateway
    /// * `gateway_ip` - IP address of the NAT gateway (with subnet mask)
    pub fn new(
        tx: Sender<Packet>,
        rx: Receiver<Packet>,
        gateway_mac: [u8; 6],
        gateway_ip: [u8; 4],
        gateway_subnet: u8,
    ) -> Self {
        let mut device = VirtualDevice::new(tx, rx);

        let gateway_mac_addr = EthernetAddress(gateway_mac);
        let gateway_ip_addr =
            IpAddress::v4(gateway_ip[0], gateway_ip[1], gateway_ip[2], gateway_ip[3]);

        let config = Config::new(gateway_mac_addr.into());
        let mut iface = Interface::new(config, &mut device, Instant::now());

        // Configure the interface with the gateway IP
        iface.update_ip_addrs(|ip_addrs| {
            ip_addrs
                .push(IpCidr::new(gateway_ip_addr, gateway_subnet))
                .unwrap();
        });

        let sockets = SocketSet::new(vec![]);

        Self {
            device,
            iface,
            sockets,
            gateway_mac: gateway_mac_addr,
            gateway_ip: gateway_ip_addr,
        }
    }

    /// Processes incoming packets from the emulator
    ///
    /// This method should be called in a loop to continuously process packets.
    /// It will block waiting for packets on the rx channel.
    pub fn process(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Poll the interface - this will process the packet and generate responses
        let timestamp = Instant::now();

        self.iface
            .poll(timestamp, &mut self.device, &mut self.sockets);

        Ok(())
    }

    /// Runs the NAT processing loop
    ///
    /// This method will continuously process packets until an error occurs
    /// or the channel is closed.
    pub fn run(&mut self) {
        log::info!(
            "NAT started - Gateway: {} ({})",
            self.gateway_ip,
            self.gateway_mac
        );

        loop {
            match self.process() {
                Ok(()) => {}
                Err(e) => {
                    log::error!("NAT error: {}", e);
                    break;
                }
            }
        }

        log::info!("NAT stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper function to create an ARP request packet
    fn create_arp_request(sender_mac: [u8; 6], sender_ip: [u8; 4], target_ip: [u8; 4]) -> Vec<u8> {
        let mut packet = Vec::new();

        // Ethernet header (14 bytes)
        packet.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]); // Destination MAC (broadcast)
        packet.extend_from_slice(&sender_mac); // Source MAC
        packet.extend_from_slice(&[0x08, 0x06]); // EtherType: ARP

        // ARP packet (28 bytes)
        packet.extend_from_slice(&[0x00, 0x01]); // Hardware type: Ethernet
        packet.extend_from_slice(&[0x08, 0x00]); // Protocol type: IPv4
        packet.push(6); // Hardware size
        packet.push(4); // Protocol size
        packet.extend_from_slice(&[0x00, 0x01]); // Opcode: Request

        packet.extend_from_slice(&sender_mac); // Sender MAC
        packet.extend_from_slice(&sender_ip); // Sender IP
        packet.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // Target MAC (unknown)
        packet.extend_from_slice(&target_ip); // Target IP

        packet
    }

    /// Helper function to parse an ARP reply
    fn parse_arp_reply(packet: &[u8]) -> Option<([u8; 6], [u8; 4])> {
        if packet.len() < 42 {
            return None;
        }

        // Check EtherType is ARP
        if packet[12] != 0x08 || packet[13] != 0x06 {
            return None;
        }

        // Check opcode is Reply (2)
        if packet[20] != 0x00 || packet[21] != 0x02 {
            return None;
        }

        let mut sender_mac = [0u8; 6];
        sender_mac.copy_from_slice(&packet[22..28]);

        let mut sender_ip = [0u8; 4];
        sender_ip.copy_from_slice(&packet[28..32]);

        Some((sender_mac, sender_ip))
    }

    #[test]
    fn test_arp_response() {
        // Set up channels
        let (nat_tx, emulator_rx) = crossbeam_channel::unbounded();
        let (emulator_tx, nat_rx) = crossbeam_channel::unbounded();

        // Gateway configuration
        let gateway_mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let gateway_ip = [10, 0, 2, 2];
        let gateway_subnet = 24;

        // Create NAT instance
        let mut nat = NatEngine::new(nat_tx, nat_rx, gateway_mac, gateway_ip, gateway_subnet);

        // Client configuration
        let client_mac = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let client_ip = [10, 0, 2, 15];

        // Create and send ARP request for the gateway IP
        let arp_request = create_arp_request(client_mac, client_ip, gateway_ip);
        emulator_tx.send(arp_request).unwrap();

        // Process the packet
        nat.process().unwrap();

        // Check if we received an ARP reply
        let reply = emulator_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("Should receive ARP reply");

        // Parse the reply
        let (reply_mac, reply_ip) = parse_arp_reply(&reply).expect("Should be valid ARP reply");

        // Verify the reply contains the gateway's MAC and IP
        assert_eq!(reply_mac, gateway_mac, "Reply MAC should match gateway MAC");
        assert_eq!(reply_ip, gateway_ip, "Reply IP should match gateway IP");
    }

    #[test]
    fn test_arp_no_response_for_different_ip() {
        // Set up channels
        let (nat_tx, emulator_rx) = crossbeam_channel::unbounded();
        let (emulator_tx, nat_rx) = crossbeam_channel::unbounded();

        // Gateway configuration
        let gateway_mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let gateway_ip = [10, 0, 2, 2];
        let gateway_subnet = 24;

        // Create NAT instance
        let mut nat = NatEngine::new(nat_tx, nat_rx, gateway_mac, gateway_ip, gateway_subnet);

        // Client configuration
        let client_mac = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let client_ip = [10, 0, 2, 15];
        let different_ip = [10, 0, 2, 99]; // Not the gateway IP

        // Create and send ARP request for a different IP
        let arp_request = create_arp_request(client_mac, client_ip, different_ip);
        emulator_tx.send(arp_request).unwrap();

        // Process the packet
        nat.process().unwrap();

        // Should NOT receive a reply for a different IP
        let result = emulator_rx.recv_timeout(std::time::Duration::from_millis(100));
        assert!(
            result.is_err(),
            "Should not receive ARP reply for different IP"
        );
    }
}
