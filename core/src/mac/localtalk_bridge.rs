//! LocalTalk over UDP (LToUDP) bridge
//!
//! This module implements the LocalTalk over UDP protocol, allowing emulated Macs
//! to communicate with each other over a LAN using UDP multicast.
//!
//! Protocol specification: https://windswept.home.blog/2019/12/10/localtalk-over-udp/
//!
//! Key points:
//! - UDP port 1954, multicast group 239.192.76.84
//! - Packets are LLAP frames prefixed with a 4-byte sender ID
//! - RTS/CTS collision avoidance is handled locally (not sent over network)

use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};

use log::*;
use socket2::{Domain, Protocol, Socket, Type};

/// LocalTalk over UDP port
pub const LTOUDP_PORT: u16 = 1954;

/// LocalTalk over UDP multicast address
pub const LTOUDP_MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 192, 76, 84);

/// Maximum LLAP packet size (3 byte header + 597 byte data)
pub const MAX_LLAP_SIZE: usize = 600;

/// LLAP packet types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LlapType {
    /// DDP short header (data packet)
    DdpShort = 0x01,
    /// DDP long header (data packet)
    DdpLong = 0x02,
    /// Node ID probe during address acquisition
    LapEnq = 0x81,
    /// Response to ENQ (address collision)
    LapAck = 0x82,
    /// Request to send (collision avoidance)
    LapRts = 0x84,
    /// Clear to send (collision avoidance)
    LapCts = 0x85,
}

impl TryFrom<u8> for LlapType {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::DdpShort),
            0x02 => Ok(Self::DdpLong),
            0x81 => Ok(Self::LapEnq),
            0x82 => Ok(Self::LapAck),
            0x84 => Ok(Self::LapRts),
            0x85 => Ok(Self::LapCts),
            _ => Err(value),
        }
    }
}

/// Status of the LocalTalk bridge
#[derive(Debug, Clone)]
pub struct LocalTalkStatus {
    /// Our node address (0 = not yet assigned)
    pub node_address: u8,
    /// Number of packets transmitted
    pub tx_packets: u64,
    /// Number of packets received
    pub rx_packets: u64,
}

impl std::fmt::Display for LocalTalkStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LocalTalk (node {}, tx:{} rx:{})",
            if self.node_address == 0 {
                "?".to_string()
            } else {
                self.node_address.to_string()
            },
            self.tx_packets,
            self.rx_packets
        )
    }
}

/// LocalTalk over UDP bridge
pub struct LocalTalkBridge {
    /// UDP socket for multicast communication
    socket: UdpSocket,
    /// Sender ID for loopback detection (typically process ID)
    sender_id: u32,
    /// Our node address (learned from outgoing packets)
    node_address: u8,
    /// Queue of pending CTS responses to inject (dest, src)
    pending_cts: VecDeque<(u8, u8)>,
    /// Buffer for accumulating TX data from SCC
    tx_buffer: Vec<u8>,
    /// Buffer for received packets to inject into SCC
    rx_queue: Vec<Vec<u8>>,
    /// Statistics
    tx_packets: u64,
    rx_packets: u64,
}

impl LocalTalkBridge {
    /// Create a new LocalTalk bridge
    pub fn new() -> io::Result<Self> {
        // Create UDP socket with socket2 so we can set options before binding
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

        // Enable address reuse for multiple instances on same machine
        socket.set_reuse_address(true)?;
        #[cfg(not(target_os = "windows"))]
        if let Err(e) = socket.set_reuse_port(true) {
            warn!("SO_REUSEPORT failed: {}", e);
        }

        // Bind to the LToUDP port
        let addr: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, LTOUDP_PORT);
        socket.bind(&addr.into())?;

        // Convert to std UdpSocket
        let socket: UdpSocket = socket.into();

        // Join the multicast group
        socket.join_multicast_v4(&LTOUDP_MULTICAST, &Ipv4Addr::UNSPECIFIED)?;

        // Set non-blocking for polling
        socket.set_nonblocking(true)?;

        // Use process ID as sender ID for loopback detection
        let sender_id = std::process::id();

        info!(
            "LocalTalk bridge started on port {}, multicast {}, sender_id={}",
            LTOUDP_PORT, LTOUDP_MULTICAST, sender_id
        );

        Ok(Self {
            socket,
            sender_id,
            node_address: 0,
            pending_cts: VecDeque::new(),
            tx_buffer: Vec::with_capacity(MAX_LLAP_SIZE),
            rx_queue: Vec::new(),
            tx_packets: 0,
            rx_packets: 0,
        })
    }

    /// Get current bridge status
    pub fn status(&self) -> LocalTalkStatus {
        LocalTalkStatus {
            node_address: self.node_address,
            tx_packets: self.tx_packets,
            rx_packets: self.rx_packets,
        }
    }

    /// Handle a complete LLAP packet from the SCC TX path
    pub fn handle_tx_packet(&mut self, llap: &[u8]) {
        if llap.len() < 3 {
            warn!("LocalTalk: TX packet too small ({} bytes)", llap.len());
            return;
        }

        let dest = llap[0];
        let src = llap[1];
        let ptype = llap[2];

        // Track our node address from outgoing packets
        if src != 0 && src != 0xFF {
            self.node_address = src;
        }

        match ptype {
            0x84 => {
                // lapRTS - Request to send
                // Don't send over network - synthesize CTS response locally
                if dest != 0xFF {
                    // Unicast RTS: synthesize CTS response
                    // CTS packet: dest=original_src, src=original_dest, type=0x85
                    self.pending_cts.push_back((src, dest));
                }
                // Broadcast RTS is ignored
            }
            0x85 => {
                // lapCTS - Clear to send
                // Don't send CTS over network
            }
            _ => {
                // All other packets (data, ENQ, ACK) are sent over UDP
                self.send_udp(llap);
            }
        }
    }

    /// Send an LLAP packet over UDP multicast
    fn send_udp(&mut self, llap: &[u8]) {
        // Build UDP packet: 4-byte sender ID (big-endian) + LLAP data
        let mut packet = Vec::with_capacity(4 + llap.len());
        packet.extend_from_slice(&self.sender_id.to_be_bytes());
        packet.extend_from_slice(llap);

        let dest = SocketAddr::V4(SocketAddrV4::new(LTOUDP_MULTICAST, LTOUDP_PORT));

        match self.socket.send_to(&packet, dest) {
            Ok(_) => {
                self.tx_packets += 1;
            }
            Err(e) => {
                warn!("LocalTalk: UDP send error: {}", e);
            }
        }
    }

    /// Poll for incoming UDP packets and state changes
    /// Returns true if there's data available for the SCC
    pub fn poll(&mut self) -> bool {
        let mut buf = [0u8; 4 + MAX_LLAP_SIZE + 64]; // Extra space for safety
        let mut received_any = false;

        // Receive all pending UDP packets
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((len, _)) => {
                    if len < 4 + 3 {
                        // Too small: need at least sender_id (4) + LLAP header (3)
                        continue;
                    }

                    // Extract sender ID
                    let packet_sender_id = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);

                    // Loopback detection: skip packets from ourselves
                    if packet_sender_id == self.sender_id {
                        continue;
                    }

                    // Extract LLAP packet
                    let llap = &buf[4..len];
                    self.handle_rx_packet(llap);
                    received_any = true;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    break;
                }
                Err(e) => {
                    warn!("LocalTalk: UDP recv error: {}", e);
                    break;
                }
            }
        }

        received_any || !self.pending_cts.is_empty() || !self.rx_queue.is_empty()
    }

    /// Handle a received LLAP packet from the network
    fn handle_rx_packet(&mut self, llap: &[u8]) {
        if llap.len() < 3 {
            return;
        }

        let dest = llap[0];

        // Filter packets not destined for us
        // Accept: broadcast (0xFF), our node address, or if we don't have an address yet
        if dest != 0xFF && self.node_address != 0 && dest != self.node_address {
            // Update our node address to match what the network expects
            self.node_address = dest;
        }

        // Queue the packet for injection into SCC
        self.rx_queue.push(llap.to_vec());
        self.rx_packets += 1;
    }

    /// Read data to inject into the SCC RX path
    /// Returns LLAP packet data (one packet at a time)
    pub fn read_to_scc(&mut self) -> Option<Vec<u8>> {
        // First, return any pending CTS response
        if let Some((dest, src)) = self.pending_cts.pop_front() {
            let cts = vec![dest, src, 0x85]; // CTS packet
            return Some(cts);
        }

        // Then return queued packets from the network
        if !self.rx_queue.is_empty() {
            return Some(self.rx_queue.remove(0));
        }

        None
    }

    /// Get the number of packets waiting in the RX queue
    pub fn rx_queue_len(&self) -> usize {
        self.rx_queue.len()
    }

    /// Write data from the SCC TX path
    /// Accumulates bytes and extracts LLAP packets
    pub fn write_from_scc(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        self.tx_buffer.extend_from_slice(data);
        self.try_extract_packets();
    }

    /// Try to extract complete LLAP packets from the TX buffer
    fn try_extract_packets(&mut self) {
        while self.tx_buffer.len() >= 3 {
            let ptype = self.tx_buffer[2];

            // Determine expected packet length
            let packet_len = if ptype >= 0x80 {
                // Control packet (RTS, CTS, ENQ, ACK) - always 3 bytes
                3
            } else {
                // Data packet - need to parse DDP length
                // Check if we have enough for DDP header
                if self.tx_buffer.len() < 5 {
                    break;
                }

                // DDP length is in the first 10 bits of the 2-byte field at offset 3
                let ddp_len =
                    (((self.tx_buffer[3] as usize) & 0x03) << 8) | (self.tx_buffer[4] as usize);

                // Total packet = 3 (LLAP) + ddp_len
                if ddp_len == 0 || ddp_len > MAX_LLAP_SIZE - 3 {
                    // Invalid length - skip byte and retry
                    self.tx_buffer.remove(0);
                    continue;
                }

                3 + ddp_len
            };

            // Check if we have the complete packet
            if self.tx_buffer.len() < packet_len {
                break;
            }

            // Extract the packet
            let packet: Vec<u8> = self.tx_buffer.drain(..packet_len).collect();
            self.handle_tx_packet(&packet);
        }

        // If buffer gets too large without extracting packets, something is wrong
        if self.tx_buffer.len() > MAX_LLAP_SIZE * 2 {
            warn!(
                "LocalTalk: TX buffer overflow ({} bytes), clearing",
                self.tx_buffer.len()
            );
            self.tx_buffer.clear();
        }
    }

    /// Flush any pending TX data (called on frame boundary)
    pub fn flush_tx(&mut self) {
        if !self.tx_buffer.is_empty() {
            let packet = std::mem::take(&mut self.tx_buffer);
            if packet.len() >= 3 {
                self.handle_tx_packet(&packet);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llap_type_conversion() {
        assert_eq!(LlapType::try_from(0x81), Ok(LlapType::LapEnq));
        assert_eq!(LlapType::try_from(0x84), Ok(LlapType::LapRts));
        assert!(LlapType::try_from(0x99).is_err());
    }
}
