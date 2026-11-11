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
#[cfg(feature = "afp_server")]
use rand::Rng;
use socket2::{Domain, Protocol, Socket, Type};

#[cfg(feature = "afp_server")]
use snow_appletalk::link::{DdpHeaderKind, LinkEvent, Packet};

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
    /// Address of the built-in AppleTalk node (0 = none)
    pub server_node: u8,
    /// Number of packets transmitted
    pub tx_packets: u64,
    /// Number of packets received
    pub rx_packets: u64,
}

impl std::fmt::Display for LocalTalkStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LocalTalk (node {}{}, tx:{} rx:{})",
            if self.node_address == 0 {
                "?".to_string()
            } else {
                self.node_address.to_string()
            },
            if self.server_node == 0 {
                String::new()
            } else {
                format!(", server node {}", self.server_node)
            },
            self.tx_packets,
            self.rx_packets
        )
    }
}

/// LocalTalk over UDP bridge
pub struct LocalTalkBridge {
    /// UDP socket for multicast communication (None in private mode)
    socket: Option<UdpSocket>,
    /// Sender ID for loopback detection (typically process ID)
    sender_id: u32,
    /// Our node address (set from SCC WR6, also learned from outgoing packets)
    node_address: u8,
    /// Address of the built-in AppleTalk node (0 = none)
    server_node: u8,
    /// Z8530 SDLC address search mode (hardware destination filter)
    /// When ON: only accept packets for our address or broadcast (0xFF)
    /// When OFF: accept all packets (Mac firmware handles its own filtering)
    address_search_mode: bool,
    /// Queue of pending CTS responses to inject (dest, src)
    pending_cts: VecDeque<(u8, u8)>,
    /// Buffer for accumulating TX data from SCC
    tx_buffer: Vec<u8>,
    /// Whether a packet came out of the frame the guest is sending
    tx_extracted: bool,
    /// Buffer for received packets to inject into SCC
    rx_queue: VecDeque<Vec<u8>>,
    /// Packets and events from the SCC to the AppleTalk node
    #[cfg(feature = "afp_server")]
    node_tx: Option<crossbeam_channel::Sender<LinkEvent>>,
    /// The emulated second the node was last told about
    #[cfg(feature = "afp_server")]
    seconds: Option<u64>,
    /// Packets from the AppleTalk node to inject into the SCC
    #[cfg(feature = "afp_server")]
    node_rx: Option<crossbeam_channel::Receiver<Packet>>,
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

        info!(
            "LocalTalk bridge started on port {}, multicast {}, sender_id={}",
            LTOUDP_PORT,
            LTOUDP_MULTICAST,
            std::process::id()
        );
        Self::with_socket(socket)
    }

    /// Create a bridge on an already bound socket
    fn with_socket(socket: UdpSocket) -> io::Result<Self> {
        // Set non-blocking for polling
        socket.set_nonblocking(true)?;

        // Use process ID as sender ID for loopback detection
        let sender_id = std::process::id();

        Ok(Self {
            socket: Some(socket),
            sender_id,
            node_address: 0,
            server_node: 0,
            address_search_mode: false,
            pending_cts: VecDeque::new(),
            tx_buffer: Vec::with_capacity(MAX_LLAP_SIZE),
            tx_extracted: false,
            rx_queue: VecDeque::new(),
            #[cfg(feature = "afp_server")]
            node_tx: None,
            #[cfg(feature = "afp_server")]
            seconds: None,
            #[cfg(feature = "afp_server")]
            node_rx: None,
            tx_packets: 0,
            rx_packets: 0,
        })
    }

    /// Create a private bridge for the built-in AppleTalk node, with no
    /// connection to the outside network. The node runs on its own thread
    /// and starts at node address 254, in the range for servers.
    #[cfg(feature = "afp_server")]
    pub fn new_private() -> Self {
        // The same address at every start, so traces compare between runs.
        // Inside AppleTalk, 2nd ed., chapter 1: node IDs 128 to 254 are
        // for servers. A guest that holds it moves the node elsewhere.
        let server_node = 254;
        let (node_tx, thread_rx) = crossbeam_channel::bounded(256);
        let (thread_tx, node_rx) = crossbeam_channel::bounded(256);
        std::thread::spawn(move || {
            snow_appletalk::node::Node::new(thread_rx, thread_tx).run();
        });

        Self {
            socket: None,
            sender_id: std::process::id(),
            node_address: 0,
            server_node,
            address_search_mode: false,
            pending_cts: VecDeque::new(),
            tx_buffer: Vec::with_capacity(MAX_LLAP_SIZE),
            tx_extracted: false,
            rx_queue: VecDeque::new(),
            node_tx: Some(node_tx),
            seconds: None,
            node_rx: Some(node_rx),
            tx_packets: 0,
            rx_packets: 0,
        }
    }

    /// Pick a random server node address, avoiding the given one
    #[cfg(feature = "afp_server")]
    fn pick_server_node(avoid: u8) -> u8 {
        loop {
            let node = rand::rng().random_range(128..=254);
            if node != avoid {
                return node;
            }
        }
    }

    /// Get current bridge status
    pub fn status(&self) -> LocalTalkStatus {
        LocalTalkStatus {
            node_address: self.node_address,
            server_node: self.server_node,
            tx_packets: self.tx_packets,
            rx_packets: self.rx_packets,
        }
    }

    /// Dump an LLAP frame, with wire debug on
    #[cfg(feature = "afp_server")]
    fn dump_llap(dir: &str, llap: &[u8]) {
        if !snow_appletalk::wire::wire_debug_enabled() {
            return;
        }
        let mut hex = String::with_capacity(llap.len() * 3);
        for (i, b) in llap.iter().enumerate() {
            if i > 0 {
                hex.push(' ');
            }
            hex.push_str(&format!("{b:02x}"));
        }
        let t = std::time::Instant::now()
            .duration_since(Self::debug_start())
            .as_millis();
        log::info!(
            "LLAP {dir} t={t}ms: dst={:02x} src={:02x} type={:02x} len={} | {hex}",
            llap.first().copied().unwrap_or(0),
            llap.get(1).copied().unwrap_or(0),
            llap.get(2).copied().unwrap_or(0),
            llap.len()
        );
    }

    /// The start time for wire_debug timestamps
    #[cfg(feature = "afp_server")]
    fn debug_start() -> std::time::Instant {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        *START.get_or_init(std::time::Instant::now)
    }

    /// Whether the per-frame logs print: the bridge reports every frame,
    /// so they follow the built-in node's wire debug switch
    pub fn wire_debug() -> bool {
        #[cfg(feature = "afp_server")]
        let on = snow_appletalk::wire::wire_debug_enabled();
        #[cfg(not(feature = "afp_server"))]
        let on = false;
        on
    }

    /// Handle an LLAP packet transmitted by the guest
    pub fn handle_tx_packet(&mut self, llap: &[u8]) -> Vec<Vec<u8>> {
        if llap.len() < 3 {
            warn!("LocalTalk: TX packet too small ({} bytes)", llap.len());
            return Vec::new();
        }
        #[cfg(feature = "afp_server")]
        Self::dump_llap("<- guest", llap);

        let dest = llap[0];
        let src = llap[1];
        let ptype = llap[2];

        // The Mac is acquiring an address: it has just started, or just
        // turned AppleTalk on, so whatever sessions it had are gone
        #[cfg(feature = "afp_server")]
        if ptype == 0x81
            && dest == src
            && dest != self.server_node
            && let Some(node_tx) = &self.node_tx
        {
            let _ = node_tx.try_send(LinkEvent::NodeStarted(src));
        }

        // The Mac tries the AppleTalk node's address: an ENQ carries the
        // address being tried as both destination and source. The answer
        // comes back at once, as a CTS does, and the Mac tries another
        // (Inside AppleTalk ch1: the node using an address acknowledges).
        if ptype == 0x81 && self.server_node != 0 && dest == self.server_node {
            let ack = vec![src, self.server_node, 0x82];
            #[cfg(feature = "afp_server")]
            Self::dump_llap("-> guest (ack)", &ack);
            return vec![ack];
        }

        // Track our node address from outgoing packets
        if src != 0 && src != 0xFF {
            self.node_address = src;
            // The Mac already holds the AppleTalk node's address: the node
            // moves, and NBP has to answer with the new one
            #[cfg(feature = "afp_server")]
            if src == self.server_node {
                self.server_node = Self::pick_server_node(src);
                if let Some(node_tx) = &self.node_tx {
                    let _ = node_tx.try_send(LinkEvent::AddressChanged(self.server_node));
                }
            }
        }

        match ptype {
            0x84 => {
                // lapRTS - Request to send
                // Don't send over network - synthesize CTS response locally.
                // On the private link, only answer RTS frames addressed to the
                // AppleTalk node, and hand the CTS back immediately so it can
                // be injected into the SCC right away (the driver's CTS poll
                // window is too short for the next tick batch). A broadcast
                // RTS gets no reply on a real bus, and the Mac treats an
                // unexpected CTS as a collision (LLAP access control).
                // The UDP bridge keeps answering every RTS via pending_cts.
                if self.socket.is_none() && dest == self.server_node {
                    #[cfg(feature = "afp_server")]
                    Self::dump_llap("-> guest (cts)", &[src, dest, 0x85]);
                    return vec![vec![src, dest, 0x85]];
                }
                if self.socket.is_some() {
                    self.pending_cts.push_back((src, dest));
                }
            }
            0x85 => {
                // lapCTS - Clear to send
                // Don't send CTS over network
            }
            _ => {
                // All other packets (data, ENQ, ACK) are sent over UDP
                self.send_udp(llap);

                // Forward data packets for the AppleTalk node (or everyone)
                #[cfg(feature = "afp_server")]
                if (ptype == 0x01 || ptype == 0x02)
                    && (dest == self.server_node || dest == 0xFF)
                    && let Some(node_tx) = &self.node_tx
                {
                    let packet = Packet {
                        kind: if ptype == 0x01 {
                            DdpHeaderKind::Short
                        } else {
                            DdpHeaderKind::Extended
                        },
                        node: src,
                        datagram: llap[3..].to_vec(),
                    };
                    if node_tx.try_send(LinkEvent::Datagram(packet)).is_err() {
                        debug!("LocalTalk: AppleTalk node queue full, dropped a packet");
                    }
                }
            }
        }
        Vec::new()
    }

    /// Send an LLAP packet over UDP multicast
    fn send_udp(&mut self, llap: &[u8]) {
        let Some(socket) = &self.socket else {
            return;
        };

        // Build UDP packet: 4-byte sender ID (big-endian) + LLAP data
        let mut packet = Vec::with_capacity(4 + llap.len());
        packet.extend_from_slice(&self.sender_id.to_be_bytes());
        packet.extend_from_slice(llap);

        let dest = SocketAddr::V4(SocketAddrV4::new(LTOUDP_MULTICAST, LTOUDP_PORT));

        match socket.send_to(&packet, dest) {
            Ok(_) => {
                self.tx_packets += 1;
            }
            Err(e) => {
                warn!("LocalTalk: UDP send error: {}", e);
            }
        }
    }

    /// Pass the emulated time on to the AppleTalk node, one tick per second.
    /// A paused emulator sends none, so the node's timers stop with it.
    pub fn advance_time(&mut self, seconds: u64) {
        #[cfg(feature = "afp_server")]
        if let Some(node_tx) = &self.node_tx {
            let last = self.seconds.replace(seconds).unwrap_or(seconds);
            for _ in last..seconds {
                if node_tx.try_send(LinkEvent::Tick).is_err() {
                    break;
                }
            }
        }
        #[cfg(not(feature = "afp_server"))]
        let _ = seconds;
    }

    /// Poll for incoming UDP packets and state changes.
    /// Returns true if something arrived, so the status changed. A frame
    /// still waiting for the guest's receiver does not count: with
    /// AppleTalk off in the guest it waits for good, and this runs on
    /// every tick batch.
    pub fn poll(&mut self) -> bool {
        let mut received_any = false;

        // Collect pending UDP packets. Not in the middle of a frame the guest
        // is sending: this runs for every byte it writes, and the line is
        // the guest's until it finishes.
        let udp_packets = if let Some(socket) =
            self.socket.as_ref().filter(|_| self.tx_buffer.is_empty())
        {
            let mut packets = Vec::new();
            let mut buf = [0u8; 4 + MAX_LLAP_SIZE + 64]; // Extra space for safety

            loop {
                match socket.recv_from(&mut buf) {
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

                        packets.push(buf[4..len].to_vec());
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
            packets
        } else {
            Vec::new()
        };

        for llap in udp_packets {
            self.handle_rx_packet(&llap);
            received_any = true;
        }

        // Inject packets from the AppleTalk node into the SCC
        #[cfg(feature = "afp_server")]
        while let Some(packet) = self.node_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            let mut llap = Vec::with_capacity(3 + packet.datagram.len());
            llap.push(packet.node);
            llap.push(self.server_node);
            llap.push(match packet.kind {
                DdpHeaderKind::Short => 0x01,
                DdpHeaderKind::Extended => 0x02,
            });
            llap.extend_from_slice(&packet.datagram);
            Self::dump_llap("-> guest (node)", &llap);
            self.queue_rx(llap);
            received_any = true;
        }

        received_any
    }

    /// Queue a frame for the guest. A guest with AppleTalk off never reads
    /// any, so on a busy LocalTalk over UDP network the queue would keep
    /// every frame on the network; the oldest go once it is full. A lost
    /// frame is one the line could have lost too, and the protocols above
    /// retransmit.
    fn queue_rx(&mut self, llap: Vec<u8>) {
        const MAX_QUEUED: usize = 128;
        if self.rx_queue.len() == MAX_QUEUED {
            self.rx_queue.pop_front();
        }
        self.rx_queue.push_back(llap);
    }

    /// Handle a received LLAP packet from the network
    fn handle_rx_packet(&mut self, llap: &[u8]) {
        if llap.len() < 3 {
            return;
        }
        #[cfg(feature = "afp_server")]
        Self::dump_llap("-> guest (udp)", llap);

        let dest = llap[0];

        // Z8530 SDLC address search mode = hardware destination filter
        // When ON: only accept our address + broadcast
        // When OFF: accept all packets (Mac firmware handles its own filtering)
        if self.address_search_mode
            && self.node_address != 0
            && dest != self.node_address
            && dest != 0xFF
        {
            return; // Not for us, drop
        }

        let ptype = llap[2];
        let src = llap[1];

        // If we have a settled address and someone ENQs for it, respond with ACK
        // ("that address is taken"). Still queue the ENQ for the SCC as well.
        if ptype == 0x81
            && dest == self.node_address
            && self.node_address != 0
            && !self.address_search_mode
        {
            let ack = vec![src, self.node_address, 0x82];
            self.send_udp(&ack);
        }

        // Queue the packet for injection into SCC
        self.queue_rx(llap.to_vec());
        self.rx_packets += 1;
    }

    /// Set the node address (from SCC WR6 / SDLC station address)
    pub fn set_node_address(&mut self, addr: u8) {
        if addr != 0 {
            self.node_address = addr;
        }
    }

    /// Set the SDLC address search mode (from SCC WR3 bit 2)
    pub fn set_address_search_mode(&mut self, mode: bool) {
        self.address_search_mode = mode;
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
        self.rx_queue.pop_front()
    }

    /// Deliver a frame the guest did not read again, before the queued
    /// ones. Only data frames: a late CTS or ACK would answer nothing.
    pub fn return_frame(&mut self, llap: Vec<u8>) {
        if matches!(llap.get(2), Some(0x01 | 0x02)) {
            self.rx_queue.push_front(llap);
        }
    }

    /// Get the number of packets waiting in the RX queue
    pub fn rx_queue_len(&self) -> usize {
        self.rx_queue.len()
    }

    /// Write data from the SCC TX path
    /// Accumulates bytes and extracts LLAP packets.
    /// Returns packets to inject into the SCC RX buffer immediately.
    pub fn write_from_scc(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        if data.is_empty() {
            return Vec::new();
        }

        self.tx_buffer.extend_from_slice(data);
        self.try_extract_packets()
    }

    /// Try to extract complete LLAP packets from the TX buffer.
    /// Returns packets to inject into the SCC RX buffer immediately.
    fn try_extract_packets(&mut self) -> Vec<Vec<u8>> {
        let mut replies = Vec::new();

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
            self.tx_extracted = true;
            let packet: Vec<u8> = self.tx_buffer.drain(..packet_len).collect();
            replies.extend(self.handle_tx_packet(&packet));
        }

        // If buffer gets too large without extracting packets, something is wrong
        if self.tx_buffer.len() > MAX_LLAP_SIZE * 2 {
            warn!(
                "LocalTalk: TX buffer overflow ({} bytes), clearing",
                self.tx_buffer.len()
            );
            self.tx_buffer.clear();
        }

        replies
    }

    /// The guest ended a frame. The bytes extracted so far are framed by
    /// their DDP length, so anything left is either a frame whose length
    /// field does not cover it, which goes out as it is, or what followed a
    /// complete packet, which is dropped. Either way the next frame starts
    /// clean. Returns packets to inject into the SCC RX buffer immediately.
    pub fn end_tx_frame(&mut self) -> Vec<Vec<u8>> {
        let extracted = std::mem::take(&mut self.tx_extracted);
        let rest = std::mem::take(&mut self.tx_buffer);
        if extracted || rest.len() < 3 {
            return Vec::new();
        }
        self.handle_tx_packet(&rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a bridge for testing (may fail if UDP port is in use)
    /// A bridge on a local socket of its own, so the tests neither need a
    /// network nor meet another instance's traffic on the LToUDP port
    fn test_bridge() -> LocalTalkBridge {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        LocalTalkBridge::with_socket(socket).unwrap()
    }

    #[test]
    fn test_frame_end_resynchronizes_the_byte_stream() {
        let mut bridge = test_bridge();

        // A data frame whose DDP length promises more than it holds
        bridge.write_from_scc(&[10, 42, 0x01, 0x00, 0x40, 1, 2, 3]);
        assert_eq!(bridge.tx_buffer.len(), 8);
        bridge.end_tx_frame();
        assert!(bridge.tx_buffer.is_empty());
        // The next frame parses from its start
        bridge.write_from_scc(&[10, 42, 0x84]);
        assert_eq!(bridge.pending_cts.len(), 1);
        assert!(bridge.tx_buffer.is_empty());
        // Bytes after a complete packet are not a frame
        bridge.end_tx_frame();
        bridge.write_from_scc(&[10, 42, 0x84, 0xAA, 0xBB, 0xCC]);
        assert_eq!(bridge.pending_cts.len(), 2);
        bridge.end_tx_frame();
        assert!(bridge.tx_buffer.is_empty());
        assert_eq!(bridge.pending_cts.len(), 2);
    }

    #[test]
    fn test_the_receive_queue_keeps_the_newest_frames() {
        let mut bridge = test_bridge();
        for i in 0..200u8 {
            bridge.handle_rx_packet(&[0x20, i, 0x01]);
        }
        assert_eq!(bridge.rx_queue_len(), 128);
        assert_eq!(bridge.read_to_scc(), Some(vec![0x20, 72, 0x01]));
    }

    #[test]
    fn test_return_frame() {
        let mut bridge = test_bridge();

        bridge.handle_rx_packet(&[42, 10, 0x01, 1]);
        // A data frame the guest did not read goes before the queue
        bridge.return_frame(vec![42, 10, 0x01, 0]);
        assert_eq!(bridge.read_to_scc(), Some(vec![42, 10, 0x01, 0]));
        // A control frame does not come back
        bridge.return_frame(vec![42, 10, 0x85]);
        assert_eq!(bridge.read_to_scc(), Some(vec![42, 10, 0x01, 1]));
        assert_eq!(bridge.read_to_scc(), None);
    }

    #[test]
    fn test_llap_type_conversion() {
        assert_eq!(LlapType::try_from(0x81), Ok(LlapType::LapEnq));
        assert_eq!(LlapType::try_from(0x84), Ok(LlapType::LapRts));
        assert!(LlapType::try_from(0x99).is_err());
    }

    #[test]
    fn test_rx_filter_address_search_mode() {
        let mut bridge = test_bridge();

        bridge.set_node_address(42);
        bridge.set_address_search_mode(true);

        // Packet for our address should be accepted
        bridge.handle_rx_packet(&[42, 10, 0x01]);
        assert_eq!(bridge.rx_queue.len(), 1);

        // Broadcast should be accepted
        bridge.handle_rx_packet(&[0xFF, 10, 0x01]);
        assert_eq!(bridge.rx_queue.len(), 2);

        // Packet for different address should be dropped
        bridge.handle_rx_packet(&[99, 10, 0x01]);
        assert_eq!(bridge.rx_queue.len(), 2);
    }

    #[test]
    fn test_rx_no_filter_when_search_mode_off() {
        let mut bridge = test_bridge();

        bridge.set_node_address(42);
        bridge.set_address_search_mode(false);

        // All packets should be accepted when address search mode is off
        bridge.handle_rx_packet(&[99, 10, 0x01]);
        assert_eq!(bridge.rx_queue.len(), 1);

        bridge.handle_rx_packet(&[42, 10, 0x01]);
        assert_eq!(bridge.rx_queue.len(), 2);
    }

    #[test]
    fn test_rx_no_filter_when_no_address() {
        let mut bridge = test_bridge();

        // node_address = 0 (not yet assigned), search mode on
        bridge.set_address_search_mode(true);

        // All packets should be accepted when we have no address yet
        bridge.handle_rx_packet(&[99, 10, 0x01]);
        assert_eq!(bridge.rx_queue.len(), 1);
    }

    #[test]
    fn test_rx_too_small_packet() {
        let mut bridge = test_bridge();

        // Packets smaller than 3 bytes should be dropped
        bridge.handle_rx_packet(&[42, 10]);
        assert_eq!(bridge.rx_queue.len(), 0);

        bridge.handle_rx_packet(&[]);
        assert_eq!(bridge.rx_queue.len(), 0);
    }

    #[test]
    fn test_set_node_address_ignores_zero() {
        let mut bridge = test_bridge();

        bridge.set_node_address(42);
        assert_eq!(bridge.node_address, 42);

        // Setting to 0 should be ignored
        bridge.set_node_address(0);
        assert_eq!(bridge.node_address, 42);
    }

    #[test]
    fn test_tx_rts_generates_cts() {
        let mut bridge = test_bridge();

        // RTS from node 42 to node 10 should generate CTS
        bridge.handle_tx_packet(&[10, 42, 0x84]);
        assert_eq!(bridge.pending_cts.len(), 1);
        let (dest, src) = bridge.pending_cts[0];
        assert_eq!(dest, 42); // CTS goes back to sender
        assert_eq!(src, 10);
    }

    #[cfg(all(feature = "afp_server", feature = "afp_server_test"))]
    mod private {
        use super::*;
        use snow_appletalk::ddp::{build_short, parse_short};
        use snow_appletalk::test::{TEST_SOCKET, TEST_TYPE};

        fn private_bridge() -> LocalTalkBridge {
            LocalTalkBridge::new_private()
        }

        #[test]
        fn test_enq_ack() {
            let mut bridge = private_bridge();

            // The Mac tries the AppleTalk node's address: the real frame has
            // it as both destination and source
            let server = bridge.server_node;
            assert_eq!(
                bridge.handle_tx_packet(&[server, server, 0x81]),
                vec![vec![server, server, 0x82]]
            );
            // The node keeps it
            assert_eq!(bridge.server_node, server);
        }

        #[test]
        fn test_a_mac_holding_the_address_moves_the_node() {
            let mut bridge = private_bridge();
            let server = bridge.server_node;
            bridge.handle_tx_packet(&[0xFF, server, 0x01, 0, 5, 2, 2, 2]);
            assert_ne!(bridge.server_node, server);
        }

        #[test]
        fn test_poll_is_quiet_while_a_frame_waits() {
            let mut bridge = private_bridge();
            bridge
                .rx_queue
                .push_back(vec![0x20, bridge.server_node, 0x01]);
            assert!(!bridge.poll());
            assert_eq!(bridge.rx_queue_len(), 1);
        }

        #[test]
        fn test_rts_gets_immediate_cts() {
            let mut bridge = private_bridge();

            // A unicast RTS to the AppleTalk node is answered immediately
            // (the driver's CTS poll window is too short to wait for a
            // later poll of pending_cts)
            let replies = bridge.handle_tx_packet(&[bridge.server_node, 0x20, 0x84]);
            assert_eq!(replies, vec![vec![0x20, bridge.server_node, 0x85]]);
            assert!(bridge.pending_cts.is_empty());

            // A broadcast RTS gets no reply (a synthesized CTS makes the
            // Mac's LLAP driver treat it as a collision)
            assert!(bridge.handle_tx_packet(&[0xFF, 0x20, 0x84]).is_empty());
        }

        #[test]
        fn test_emulated_seconds_reach_the_node() {
            let mut bridge = private_bridge();
            let (node_tx, node_rx) = crossbeam_channel::bounded(16);
            bridge.node_tx = Some(node_tx);
            // The first call sets the start, each later second is a tick,
            // and time going back (a restored state) sends nothing
            bridge.advance_time(100);
            bridge.advance_time(103);
            bridge.advance_time(50);
            bridge.advance_time(51);
            let ticks = node_rx
                .try_iter()
                .filter(|e| matches!(e, LinkEvent::Tick))
                .count();
            assert_eq!(ticks, 4);
        }

        /// What the emulator does between the SCC and the bridge on each
        /// pass (emulator/mod.rs), and a millisecond of emulated time
        fn pump(scc: &mut crate::mac::scc::Scc, bridge: &mut LocalTalkBridge) {
            use crate::mac::scc::SccCh::B;
            if scc.has_tx_data(B) {
                for reply in bridge.write_from_scc(&scc.take_tx(B)) {
                    scc.push_rx_frame(B, reply);
                }
            }
            if scc.take_tx_frame_ended(B) {
                for reply in bridge.end_tx_frame() {
                    scc.push_rx_frame(B, reply);
                }
            }
            bridge.poll();
            if let Some(frame) = scc.take_unread_frame(B) {
                bridge.return_frame(frame);
            }
            if scc.is_rx_ready_for_data(B)
                && let Some(frame) = bridge.read_to_scc()
            {
                scc.push_rx(B, &frame);
            }
            scc.tick(15_667, 15_667_000);
        }

        /// A LocalTalk driver's sequence, scripted against the real SCC, the
        /// bridge and the node, with no CPU: what the guest runs pins the
        /// timing the bridge relies on (CTS at once, a frame held until the
        /// receiver is on and the line has been quiet for the interdialog
        /// gap)
        #[test]
        fn test_a_scripted_localtalk_driver() {
            use crate::bus::BusMember;
            use crate::mac::scc::{RTXC_FREQUENCY, Scc};
            let mut scc = Scc::new(RTXC_FREQUENCY);
            let mut bridge = private_bridge();
            let server = bridge.server_node;
            let wr = |scc: &mut Scc, reg: u8, val: u8| {
                scc.write(0, reg);
                scc.write(0, val);
            };
            // Channel B in SDLC mode (WR4 = $20), station address $20 (WR6),
            // the flag (WR7), receiver on in hunt with address search (WR3),
            // transmitter on (WR5)
            wr(&mut scc, 4, 0x20);
            wr(&mut scc, 6, 0x20);
            wr(&mut scc, 7, 0x7E);
            wr(&mut scc, 3, 0xDD);
            wr(&mut scc, 5, 0x6A);
            // Send a frame: its bytes, then Reset Tx Underrun/EOM (WR0 $C0)
            let send = |scc: &mut Scc, frame: &[u8]| {
                for &b in frame {
                    scc.write(2, b);
                }
                scc.write(0, 0xC0);
            };
            // Read a frame: the bytes while RR0 says a character waits
            let receive = |scc: &mut Scc, bridge: &mut LocalTalkBridge| {
                let mut frame = Vec::new();
                for _ in 0..5000 {
                    pump(scc, bridge);
                    while scc.read(0).unwrap() & 0x01 != 0 {
                        frame.push(scc.read(2).unwrap());
                    }
                    if !frame.is_empty() && scc.read(0).unwrap() & 0x01 == 0 {
                        return frame;
                    }
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
                panic!("no frame came");
            };
            // RTS to the node: the CTS is there by the next pass
            send(&mut scc, &[server, 0x20, 0x84]);
            pump(&mut scc, &mut bridge);
            assert!(scc.read(0).unwrap() & 0x01 != 0, "no CTS at once");
            assert_eq!(receive(&mut scc, &mut bridge)[..3], [0x20, server, 0x85]);
            // The data frame: an AEP echo request to the node's socket 4
            let mut frame = vec![server, 0x20, 0x01];
            frame.extend(build_short(4, 0x30, 4, b"\x01ping"));
            send(&mut scc, &frame);
            let reply = receive(&mut scc, &mut bridge);
            assert_eq!(reply[..3], [0x20, server, 0x01]);
            // The last two bytes are the frame check sequence, which the
            // SCC passes on in SDLC mode and the driver drops
            let (header, data) = parse_short(&reply[3..reply.len() - 2]).unwrap();
            assert_eq!((header.dst_socket, header.src_socket), (0x30, 4));
            assert_eq!(data, &b"\x02ping"[..]);
        }

        #[test]
        fn test_echo() {
            let mut bridge = private_bridge();

            // The Mac broadcasts a test datagram from node 0x20
            let mut llap = vec![0xFF, 0x20, 0x01];
            llap.extend_from_slice(&build_short(TEST_SOCKET, 0x30, TEST_TYPE, b"SNT1hello"));
            bridge.handle_tx_packet(&llap);

            // Wait for the AppleTalk node to answer
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                bridge.poll();
                if let Some(reply) = bridge.read_to_scc() {
                    assert_eq!(reply[..3], [0x20, bridge.server_node, 0x01]);
                    let (header, data) = parse_short(&reply[3..]).unwrap();
                    assert_eq!(header.dst_socket, 0x30);
                    assert_eq!(header.src_socket, TEST_SOCKET);
                    assert_eq!(header.ddp_type, TEST_TYPE);
                    assert_eq!(data, &b"SNT1hello"[..]);
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "No reply from the AppleTalk node"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}
