//! NAT implementation for Snow
//!
//! This module provides Network Address Translation (NAT) functionality
//! for the emulated Ethernet interface. It receives layer 2 packets from
//! the emulator via a crossbeam channel, processes them through a NAT
//! implementation using smoltcp, and sends responses back.
//!
//! The remote connectivity is implemented fully through userland (OS) sockets.
//!
//! Currently only supports TCP and UDP.
//!
//! The MacOS TCP/IP stack seems to struggle with ARP requests during already
//! active TCP sessions which can stall connections and leave smoltcp in a
//! 'neighbor discovery pending' state. To avoid this, we send unsolicited ARP
//! replies to smoltcp regularly to keep the neighbor cache entry from being evicted.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TrySendError};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::udp;
use smoltcp::wire::{
    ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol, IpAddress,
    IpCidr, IpEndpoint, Ipv4Address,
};

/// A layer 2 Ethernet packet
pub type Packet = Vec<u8>;

const MTU: usize = 1514;
const SMOLTCP_BUFFER_SIZE: usize = 65536;

/// Default timeout for UDP connection tracking
const NAT_TIMEOUT_UDP: Duration = Duration::from_secs(300);

/// Default timeout for TCP connection tracking, while the connection is alive
const NAT_TIMEOUT_TCP_OPEN: Duration = Duration::from_secs(900);

/// Default timeout for TCP connection tracking, after the connection is closed.
/// We keep the connection around for a bit for backlog to clear up and the FIN to arrive.
const NAT_TIMEOUT_TCP_CLOSED: Duration = Duration::from_secs(45);

/// Interval of gratuitous ARP on behalf of the emulator to smoltcp
const GRATUITOUS_ARP_INTERVAL: Duration = Duration::from_secs(10);

/// Virtual network device that bridges crossbeam channels with smoltcp
struct VirtualDevice {
    rx: Receiver<Packet>,
    tx: Sender<Packet>,
    stats: Arc<NatEngineStats>,
    /// Queue for routed UDP/TCP packets that need NAT handling (intercepted before smoltcp)
    intercepted_packets: Vec<Packet>,
    /// Queue for packets to feed back to smoltcp after processing (e.g., TCP SYN after creating listening socket)
    smoltcp_queue: Vec<Packet>,
    /// Gateway IP address to detect routed packets
    gateway_ip: Ipv4Address,
    /// Learned local node (for gratuitous ARP)
    local_node: Option<(EthernetAddress, Ipv4Address)>,
    /// Next time gratuitous ARP should be sent
    next_gratuitous_arp: Instant,
}

impl VirtualDevice {
    fn new(
        tx: Sender<Packet>,
        rx: Receiver<Packet>,
        stats: Arc<NatEngineStats>,
        gateway_ip: Ipv4Address,
    ) -> Self {
        Self {
            rx,
            tx,
            stats,
            intercepted_packets: Vec::new(),
            smoltcp_queue: Vec::new(),
            gateway_ip,
            local_node: None,
            next_gratuitous_arp: Instant::now(),
        }
    }

    /// Drain routed packets that were intercepted and need NAT handling
    fn drain_intercepted_packets(&mut self) -> Vec<Packet> {
        std::mem::take(&mut self.intercepted_packets)
    }

    /// Check if a packet needs NAT (routed UDP or TCP SYN packet where destination IP != gateway IP)
    fn needs_nat(&self, packet: &[u8]) -> bool {
        use smoltcp::wire::{EthernetFrame, IpProtocol, Ipv4Packet, TcpPacket};

        // Parse Ethernet frame
        let eth_frame = match EthernetFrame::new_checked(packet) {
            Ok(frame) => frame,
            Err(_) => return false,
        };

        // Check if it's IPv4
        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return false;
        }

        // Parse IPv4 packet
        let ipv4_packet = match Ipv4Packet::new_checked(eth_frame.payload()) {
            Ok(packet) => packet,
            Err(_) => return false,
        };

        // Check if destination is NOT the gateway (i.e., it's being routed)
        let dst_ip = ipv4_packet.dst_addr();
        if dst_ip == self.gateway_ip {
            return false;
        }

        match ipv4_packet.next_header() {
            IpProtocol::Udp => {
                // Intercept all UDP packets for NAT
                true
            }
            IpProtocol::Tcp => {
                // Intercept TCP SYN packets to track connection
                let tcp_packet = match TcpPacket::new_checked(ipv4_packet.payload()) {
                    Ok(packet) => packet,
                    Err(_) => return false,
                };
                tcp_packet.syn() && !tcp_packet.ack()
            }
            _ => false,
        }
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

    fn receive(
        &mut self,
        _timestamp: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // First, check if we have packets queued for smoltcp (e.g., TCP SYN after creating listening socket)
        if let Some(packet) = self.smoltcp_queue.pop() {
            return Some((
                VirtualRxToken { buffer: packet },
                VirtualTxToken {
                    tx: self.tx.clone(),
                    stats: self.stats.clone(),
                },
            ));
        }

        // Check if it is time to send gratuitous ARP
        if let Some((local_mac, local_ip)) = self.local_node {
            let now = Instant::now();
            if self.next_gratuitous_arp < now {
                self.next_gratuitous_arp = now + GRATUITOUS_ARP_INTERVAL;

                let mut buffer = vec![0; 42];
                let mut eth_frame = EthernetFrame::new_unchecked(&mut buffer);
                eth_frame.set_src_addr(local_mac);
                eth_frame.set_dst_addr(EthernetAddress::BROADCAST);
                eth_frame.set_ethertype(EthernetProtocol::Arp);
                let mut arp_packet = ArpPacket::new_unchecked(eth_frame.payload_mut());

                let arp_repr = ArpRepr::EthernetIpv4 {
                    // Not a gratuitous ARP as per RFC 5227 but more of an
                    // 'unsollicited ARP reply'. smoltcp responds better to this than a
                    // by-the-book gratuitous ARP, which seems to be ignored.
                    operation: ArpOperation::Reply,
                    source_hardware_addr: local_mac,
                    source_protocol_addr: local_ip,
                    target_hardware_addr: EthernetAddress::BROADCAST,
                    target_protocol_addr: self.gateway_ip,
                };

                arp_repr.emit(&mut arp_packet);

                return Some((
                    VirtualRxToken { buffer },
                    VirtualTxToken {
                        tx: self.tx.clone(),
                        stats: self.stats.clone(),
                    },
                ));
            }
        }

        // Receive new packet from Ethernet adapter channel
        let packet = self.rx.recv_timeout(Duration::from_millis(100)).ok()?;
        self.stats.rx_packets.fetch_add(1, Ordering::Relaxed);
        self.stats
            .rx_bytes
            .fetch_add(packet.len(), Ordering::Relaxed);

        // Check if this packet needs NAT (routed TCP/UDP)
        if self.needs_nat(&packet) {
            self.intercepted_packets.push(packet);
            // Don't pass to smoltcp, return None to process next packet
            // This packet will be returned to the smoltcp_queue later
            return None;
        }

        // Try to learn the emulator's IP address from ARP traffic so we can keep the neighbor cache entry alive
        if let Ok(ArpRepr::EthernetIpv4 {
            operation,
            source_hardware_addr,
            source_protocol_addr,
            target_protocol_addr,
            ..
        }) = EthernetFrame::new_checked(&packet)
            .and_then(|p| ArpPacket::new_checked(p.payload()))
            .and_then(|p| ArpRepr::parse(&p))
        {
            if operation == ArpOperation::Request
                && target_protocol_addr == self.gateway_ip
                && self.local_node.is_none_or(|(mac, ip)| {
                    mac != source_hardware_addr || ip != source_protocol_addr
                })
            {
                self.local_node = Some((source_hardware_addr, source_protocol_addr));
                self.next_gratuitous_arp = Instant::now() + GRATUITOUS_ARP_INTERVAL;
                log::debug!(
                    "Learned local MAC address {} - IP address: {}",
                    source_hardware_addr,
                    source_protocol_addr
                );
            }
        }

        // Pass non-routed packets (like ARP) normally
        Some((
            VirtualRxToken { buffer: packet },
            VirtualTxToken {
                tx: self.tx.clone(),
                stats: self.stats.clone(),
            },
        ))
    }

    fn transmit(&mut self, _timestamp: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
        Some(VirtualTxToken {
            tx: self.tx.clone(),
            stats: self.stats.clone(),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
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
    stats: Arc<NatEngineStats>,
}

impl TxToken for VirtualTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);

        // Send the packet back to the emulator
        let send_len = buffer.len();
        match self.tx.try_send(buffer) {
            Ok(()) => {
                self.stats.tx_packets.fetch_add(1, Ordering::Relaxed);
                self.stats.tx_bytes.fetch_add(send_len, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.stats.tx_dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                // Ignore errors, if the channel closes the next time we try to
                // read from rx the thread will terminate.
            }
        }

        result
    }
}

/// NAT connection tracking entry
enum NatEntry {
    Udp {
        /// Userland UDP socket for external communication
        os_socket: UdpSocket,
        /// Remote (internet) endpoint
        remote_endpoint: IpEndpoint,
        /// Emulator's source endpoint
        local_endpoint: IpEndpoint,
        /// Time at which this entry expires
        expires_at: Instant,
    },
    Tcp {
        /// Userland TCP socket for external communication
        os_socket: TcpStream,
        /// Remote (internet) endpoint
        remote_endpoint: IpEndpoint,
        /// Emulator's source endpoint
        local_endpoint: IpEndpoint,
        /// Time at which this entry expires
        expires_at: Instant,
    },
}

impl NatEntry {
    pub fn expires_at(&self) -> Instant {
        match self {
            Self::Udp { expires_at, .. } => *expires_at,
            Self::Tcp { expires_at, .. } => *expires_at,
        }
    }

    pub fn is_expired(&self, now: &Instant) -> bool {
        &self.expires_at() <= now
    }
}

pub type NatEngineStatCounter = std::sync::atomic::AtomicUsize;

/// NAT engine statistics
/// Only individually synchronized when reading
#[derive(Default)]
pub struct NatEngineStats {
    pub rx_packets: NatEngineStatCounter,
    pub rx_bytes: NatEngineStatCounter,
    pub tx_packets: NatEngineStatCounter,
    pub tx_bytes: NatEngineStatCounter,
    pub tx_dropped: NatEngineStatCounter,
    pub nat_active_tcp: NatEngineStatCounter,
    pub nat_total_tcp: NatEngineStatCounter,
    pub nat_active_udp: NatEngineStatCounter,
    pub nat_total_udp: NatEngineStatCounter,
    pub nat_tcp_syn: NatEngineStatCounter,
    pub nat_tcp_fin_local: NatEngineStatCounter,
    pub nat_tcp_fin_remote: NatEngineStatCounter,
    pub nat_expired: NatEngineStatCounter,
}

/// NAT engine instance for handling network address translation
pub struct NatEngine {
    /// Virtual network device instance
    device: VirtualDevice,

    /// smoltcp network interface
    iface: Interface,

    /// smoltcp socket set
    sockets: SocketSet<'static>,

    /// Gateway MAC address
    gateway_mac: EthernetAddress,

    /// Gateway IP address
    gateway_ip: IpAddress,

    /// NAT table: maps smoltcp socket handles to NAT entries
    nat_table: HashMap<SocketHandle, NatEntry>,

    /// Buffer for receiving data from OS sockets
    recv_buffer: Vec<u8>,

    /// Statistics
    stats: Arc<NatEngineStats>,
}

impl NatEngine {
    /// Creates a new NAT engine instance with the given TX/RX channels
    ///
    /// * `tx` - Sender channel for packets going to the emulator
    /// * `rx` - Receiver channel for packets coming from the emulator
    /// * `gateway_mac` - MAC address of the NAT gateway
    /// * `gateway_ip` - IP address of the NAT gateway
    /// * `gateway_subnet` - NAT gateway subnet mask (CIDR)
    pub fn new(
        tx: Sender<Packet>,
        rx: Receiver<Packet>,
        gateway_mac: [u8; 6],
        gateway_ip: [u8; 4],
        gateway_subnet: u8,
    ) -> Self {
        let stats = Arc::new(NatEngineStats::default());
        let gateway_mac_addr = EthernetAddress(gateway_mac);
        let gateway_ip_addr =
            IpAddress::v4(gateway_ip[0], gateway_ip[1], gateway_ip[2], gateway_ip[3]);

        let gateway_ipv4 = Ipv4Address::from_bytes(&gateway_ip);
        let mut device = VirtualDevice::new(tx, rx, stats.clone(), gateway_ipv4);

        let config = Config::new(gateway_mac_addr.into());
        let mut iface = Interface::new(config, &mut device, smoltcp::time::Instant::now());

        iface.update_ip_addrs(|ip_addrs| {
            // Add gateway IP for ARP and local communication
            ip_addrs
                .push(IpCidr::new(gateway_ip_addr, gateway_subnet))
                .unwrap();
            // Add wildcard IP with /0 netmask to act as gateway
            ip_addrs
                .push(IpCidr::new(IpAddress::v4(0, 0, 0, 1), 0))
                .unwrap();
        });

        // Add default route through the wildcard IP
        iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(0, 0, 0, 1))
            .unwrap();

        // Enable any_ip mode to accept TCP connections destined for any IP address
        iface.set_any_ip(true);

        let sockets = SocketSet::new(vec![]);

        Self {
            device,
            iface,
            sockets,
            gateway_mac: gateway_mac_addr,
            gateway_ip: gateway_ip_addr,
            nat_table: HashMap::new(),
            recv_buffer: vec![0u8; SMOLTCP_BUFFER_SIZE],
            stats,
        }
    }

    /// Obtains a reference to the statistics of this engine instance
    pub fn stats(&self) -> Arc<NatEngineStats> {
        self.stats.clone()
    }

    /// Processes incoming packets from the emulator
    ///
    /// This method should be called in a loop to continuously process packets.
    /// It will block waiting for packets on the rx channel.
    pub fn process(&mut self) -> Result<()> {
        // Try to intercept and handle routed TCP/UDP packets BEFORE smoltcp processes
        self.process_intercepted_packets()?;

        // Run smoltcp
        let timestamp = smoltcp::time::Instant::now();
        self.iface
            .poll(timestamp, &mut self.device, &mut self.sockets);

        // Detect new UDP flows and create NAT entries
        self.detect_new_flows()?;

        // Do forwarding
        self.forward_udp_os_to_smoltcp()?;
        self.forward_tcp_smoltcp_to_os()?;
        self.forward_tcp_os_to_smoltcp()?;

        // Clean up expired NAT entries
        self.cleanup_expired_entries()?;

        // Update active connections statistics
        self.stats.nat_active_tcp.store(
            self.nat_table
                .iter()
                .filter(|(_, e)| matches!(e, NatEntry::Tcp { .. }))
                .count(),
            Ordering::Relaxed,
        );
        self.stats.nat_active_udp.store(
            self.nat_table
                .iter()
                .filter(|(_, e)| matches!(e, NatEntry::Udp { .. }))
                .count(),
            Ordering::Relaxed,
        );

        Ok(())
    }

    /// Handle intercepted packets waiting for processing
    fn process_intercepted_packets(&mut self) -> Result<()> {
        use smoltcp::wire::{EthernetFrame, IpProtocol, Ipv4Packet, TcpPacket, UdpPacket};

        let packets = self.device.drain_intercepted_packets();

        for packet in packets {
            // Parse the packet layers
            let eth_frame = match EthernetFrame::new_checked(&packet[..]) {
                Ok(frame) => frame,
                Err(e) => {
                    log::warn!("Failed to parse Ethernet frame: {}", e);
                    continue;
                }
            };

            let ipv4_packet = match Ipv4Packet::new_checked(eth_frame.payload()) {
                Ok(packet) => packet,
                Err(e) => {
                    log::warn!("Failed to parse IPv4 packet: {}", e);
                    continue;
                }
            };

            // Dispatch based on protocol
            match ipv4_packet.next_header() {
                IpProtocol::Udp => {
                    let udp_packet = match UdpPacket::new_checked(ipv4_packet.payload()) {
                        Ok(packet) => packet,
                        Err(e) => {
                            log::warn!("Failed to parse UDP packet: {}", e);
                            continue;
                        }
                    };

                    if let Err(e) = self.handle_outbound_udp(&ipv4_packet, &udp_packet) {
                        log::error!("Failed to handle outbound UDP: {}", e);
                    }
                }
                IpProtocol::Tcp => {
                    let tcp_packet = match TcpPacket::new_checked(ipv4_packet.payload()) {
                        Ok(packet) => packet,
                        Err(e) => {
                            log::warn!("Failed to parse TCP packet: {}", e);
                            continue;
                        }
                    };

                    if let Err(e) =
                        self.handle_outbound_tcp(&packet[..], &eth_frame, &ipv4_packet, &tcp_packet)
                    {
                        log::error!("Failed to handle outbound TCP: {}", e);
                    }
                }
                _ => {
                    log::warn!("Unsupported protocol: {:?}", ipv4_packet.next_header());
                }
            }
        }

        Ok(())
    }

    /// Handle an outbound UDP packet that needs NAT
    fn handle_outbound_udp(
        &mut self,
        ipv4_packet: &smoltcp::wire::Ipv4Packet<&[u8]>,
        udp_packet: &smoltcp::wire::UdpPacket<&[u8]>,
    ) -> Result<()> {
        let src_ip = ipv4_packet.src_addr();
        let dst_ip = ipv4_packet.dst_addr();
        let src_port = udp_packet.src_port();
        let dst_port = udp_packet.dst_port();
        let payload = udp_packet.payload();

        // Check if we already have a NAT entry for this flow
        let existing_entry = self.nat_table.iter().find(|(_, entry)| {
            if let NatEntry::Udp {
                local_endpoint,
                remote_endpoint,
                ..
            } = entry
            {
                let mac_match = if let IpAddress::Ipv4(mac_ipv4) = local_endpoint.addr {
                    mac_ipv4 == src_ip && local_endpoint.port == src_port
                } else {
                    false
                };

                let remote_match = if let IpAddress::Ipv4(remote_ipv4) = remote_endpoint.addr {
                    remote_ipv4 == dst_ip && remote_endpoint.port == dst_port
                } else {
                    false
                };

                mac_match && remote_match
            } else {
                false
            }
        });

        if let Some((handle, _entry)) = existing_entry {
            // Existing entry - forward to OS socket directly
            let handle = *handle;
            if let Some(NatEntry::Udp {
                os_socket,
                expires_at,
                ..
            }) = self.nat_table.get_mut(&handle)
            {
                *expires_at = Instant::now() + NAT_TIMEOUT_UDP;
                os_socket.send(payload)?;
            }
        } else {
            // Create new NAT entry

            let os_socket = UdpSocket::bind("0.0.0.0:0")?;
            os_socket.set_nonblocking(true)?;

            let remote_addr = SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::from(dst_ip.0)),
                dst_port,
            );
            os_socket.connect(remote_addr)?;
            os_socket.send(payload)?;

            // Create smoltcp UDP socket bound to a unique LOCAL port
            let rx_buffer = udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; 16],
                vec![0; SMOLTCP_BUFFER_SIZE],
            );
            let tx_buffer = udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; 16],
                vec![0; SMOLTCP_BUFFER_SIZE],
            );
            let mut socket = udp::Socket::new(rx_buffer, tx_buffer);

            // Bind to gateway IP with the same source port the emulator used
            let bind_endpoint = IpEndpoint::new(self.gateway_ip, src_port);
            socket.bind(bind_endpoint)?;

            let handle = self.sockets.add(socket);

            log::debug!(
                "Created UDP NAT entry: emulator {}:{} <-> smoltcp <-> OS {} <-> Internet {}:{}",
                src_ip,
                src_port,
                os_socket.local_addr()?,
                dst_ip,
                dst_port
            );

            // Store NAT entry
            let entry = NatEntry::Udp {
                os_socket,
                remote_endpoint: IpEndpoint::new(IpAddress::Ipv4(dst_ip), dst_port),
                local_endpoint: IpEndpoint::new(IpAddress::Ipv4(src_ip), src_port),
                expires_at: Instant::now() + NAT_TIMEOUT_UDP,
            };

            self.nat_table.insert(handle, entry);
            self.stats.nat_total_udp.fetch_add(1, Ordering::Relaxed);
        }

        Ok(())
    }

    /// Handle an outbound TCP SYN packet - create smoltcp socket and OS connection
    fn handle_outbound_tcp(
        &mut self,
        raw_packet: &[u8],
        _eth_frame: &smoltcp::wire::EthernetFrame<&[u8]>,
        ipv4_packet: &smoltcp::wire::Ipv4Packet<&[u8]>,
        tcp_packet: &smoltcp::wire::TcpPacket<&[u8]>,
    ) -> Result<()> {
        use smoltcp::socket::tcp;

        self.stats.nat_tcp_syn.fetch_add(1, Ordering::Relaxed);

        let src_ip = ipv4_packet.src_addr();
        let dst_ip = ipv4_packet.dst_addr();
        let src_port = tcp_packet.src_port();
        let dst_port = tcp_packet.dst_port();

        log::debug!(
            "NAT: New TCP connection from {}:{} to {}:{}",
            src_ip,
            src_port,
            dst_ip,
            dst_port
        );

        // Check if we already have an entry for this flow
        let existing = self.nat_table.iter().find(|(_, entry)| {
            if let NatEntry::Tcp {
                local_endpoint,
                remote_endpoint,
                ..
            } = entry
            {
                if let (IpAddress::Ipv4(mac_ip), IpAddress::Ipv4(remote_ip)) =
                    (local_endpoint.addr, remote_endpoint.addr)
                {
                    mac_ip == src_ip
                        && local_endpoint.port == src_port
                        && remote_ip == dst_ip
                        && remote_endpoint.port == dst_port
                } else {
                    false
                }
            } else {
                false
            }
        });

        if existing.is_some() {
            // Already in connection tracking table
            return Ok(());
        }

        // Connect to the destination via OS TCP socket
        let remote_addr = SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::from(dst_ip.0)),
            dst_port,
        );

        let os_socket = TcpStream::connect_timeout(&remote_addr, Duration::from_secs(5))?;
        os_socket.set_nonblocking(true)?;

        // Create smoltcp TCP socket for the emulator side
        let rx_buffer = tcp::SocketBuffer::new(vec![0; SMOLTCP_BUFFER_SIZE]);
        let tx_buffer = tcp::SocketBuffer::new(vec![0; SMOLTCP_BUFFER_SIZE]);
        let mut socket = tcp::Socket::new(rx_buffer, tx_buffer);

        // Listen on the DESTINATION endpoint from the packet (masquerading as the
        // remote endpoint).
        let dst_endpoint = IpEndpoint::new(IpAddress::Ipv4(dst_ip), dst_port);
        socket.listen(dst_endpoint)?;

        let handle = self.sockets.add(socket);

        log::debug!(
            "Created TCP NAT entry: emulator {}:{} <-> smoltcp <-> OS {} <-> Internet {}:{}",
            src_ip,
            src_port,
            os_socket.local_addr()?,
            dst_ip,
            dst_port
        );

        // Create TCP NAT entry
        let entry = NatEntry::Tcp {
            os_socket,
            remote_endpoint: IpEndpoint::new(IpAddress::Ipv4(dst_ip), dst_port),
            local_endpoint: IpEndpoint::new(IpAddress::Ipv4(src_ip), src_port),
            expires_at: Instant::now() + NAT_TIMEOUT_TCP_OPEN,
        };

        self.nat_table.insert(handle, entry);
        self.stats.nat_total_tcp.fetch_add(1, Ordering::Relaxed);

        // Feed the SYN packet back to smoltcp so it can complete the handshake
        self.device.smoltcp_queue.push(raw_packet.to_vec());

        Ok(())
    }

    /// Forward data from smoltcp TCP sockets (emulator side) to OS sockets (Internet side)
    fn forward_tcp_smoltcp_to_os(&mut self) -> Result<()> {
        use smoltcp::socket::tcp;

        let handles: Vec<_> = self
            .nat_table
            .iter()
            .filter_map(|(handle, entry)| {
                if matches!(entry, NatEntry::Tcp { .. }) {
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect();

        for handle in handles {
            let entry = match self.nat_table.get_mut(&handle) {
                Some(NatEntry::Tcp {
                    os_socket,
                    expires_at,
                    ..
                }) => (os_socket, expires_at),
                _ => continue,
            };

            let socket = self.sockets.get_mut::<tcp::Socket>(handle);

            // Forward data from emulator (smoltcp) to Internet (OS socket)
            if socket.can_recv() {
                match socket.recv(|buffer| {
                    if !buffer.is_empty() {
                        // Write to OS socket and only consume as much as we could push
                        // out from the smoltcp receive buffer
                        match entry.0.write(buffer) {
                            Ok(written) => (written, written),
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (0, 0),
                            Err(e) => {
                                log::warn!("Error writing to OS socket: {}", e);
                                (0, 0)
                            }
                        }
                    } else {
                        (0, 0)
                    }
                }) {
                    Ok(_) => {
                        *entry.1 = Instant::now() + NAT_TIMEOUT_TCP_OPEN;
                    }
                    Err(smoltcp::socket::tcp::RecvError::Finished) => {
                        *entry.1 = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                        self.stats.nat_tcp_fin_local.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        log::error!("TCP error receiving from emulator: {:?}", e);
                    }
                };
            }
        }

        Ok(())
    }

    /// Forward data from OS UDP sockets back to the emulator (via smoltcp)
    fn forward_udp_os_to_smoltcp(&mut self) -> Result<()> {
        let handles: Vec<_> = self
            .nat_table
            .iter()
            .filter_map(|(handle, entry)| {
                if matches!(entry, NatEntry::Udp { .. }) {
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect();

        for handle in handles {
            let local_endpoint = {
                let entry = match self.nat_table.get(&handle) {
                    Some(NatEntry::Udp { local_endpoint, .. }) => *local_endpoint,
                    _ => continue,
                };
                entry
            };

            // Get mutable access to entry
            let entry = match self.nat_table.get_mut(&handle) {
                Some(NatEntry::Udp {
                    os_socket,
                    expires_at,
                    ..
                }) => (os_socket, expires_at),
                _ => continue,
            };

            // Try to receive from OS socket (response from internet)
            match entry.0.recv_from(&mut self.recv_buffer) {
                Ok((len, _from_addr)) => {
                    // Keep entry alive
                    *entry.1 = Instant::now() + NAT_TIMEOUT_UDP;

                    // Send response via smoltcp UDP socket
                    let socket = self.sockets.get_mut::<udp::Socket>(handle);
                    if let Err(e) = socket.send_slice(&self.recv_buffer[..len], local_endpoint) {
                        log::warn!("Error sending via smoltcp UDP socket: {}", e);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No data available
                }
                Err(e) => {
                    log::warn!("Error receiving from OS socket: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Forward TCP data from OS sockets (Internet side) to smoltcp sockets (emulator side)
    fn forward_tcp_os_to_smoltcp(&mut self) -> Result<()> {
        use smoltcp::socket::tcp;

        let handles: Vec<_> = self
            .nat_table
            .iter()
            .filter_map(|(handle, entry)| {
                if matches!(entry, NatEntry::Tcp { .. }) {
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect();

        for handle in handles {
            let entry = match self.nat_table.get_mut(&handle) {
                Some(NatEntry::Tcp {
                    os_socket,
                    expires_at,
                    ..
                }) => (os_socket, expires_at),
                _ => continue,
            };

            let socket = self.sockets.get_mut::<tcp::Socket>(handle);

            // Forward data from Internet (OS socket) to emulator (smoltcp)
            if socket.can_send() {
                match entry.0.read(&mut self.recv_buffer) {
                    Ok(0) => {
                        // Connection closed by remote
                        self.stats
                            .nat_tcp_fin_remote
                            .fetch_add(1, Ordering::Relaxed);
                        socket.close();
                        *entry.1 = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                    }
                    Ok(len) => {
                        // Write data to smoltcp socket
                        // smoltcp will handle fragmentation and MTU
                        match socket.send_slice(&self.recv_buffer[..len]) {
                            Ok(_written) => {
                                *entry.1 = Instant::now() + NAT_TIMEOUT_TCP_OPEN;
                            }
                            Err(e) => {
                                log::warn!("Error sending to smoltcp socket: {}", e);
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // No data available
                    }
                    Err(e) => {
                        log::warn!("Error receiving from OS socket: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Detect new UDP flows and automatically create NAT entries
    fn detect_new_flows(&mut self) -> Result<()> {
        // Collect all socket handles that already have NAT entries
        let existing_handles: std::collections::HashSet<_> =
            self.nat_table.keys().copied().collect();

        // Collect all socket handles that are UDP sockets without NAT entries
        let mut new_flows = Vec::new();

        for (handle, _socket) in self.sockets.iter() {
            // Skip if already has NAT entry
            if existing_handles.contains(&handle) {
                continue;
            }

            let socket = self.sockets.get::<udp::Socket>(handle);

            // Check if socket has received data (indicating a new flow)
            if socket.can_recv() {
                new_flows.push((handle, socket.endpoint()));
            }
        }

        // Create NAT entries for new flows
        for (handle, listen_endpoint) in new_flows {
            // Create OS socket for this flow
            let os_socket = UdpSocket::bind("0.0.0.0:0")?;
            os_socket.set_nonblocking(true)?;

            log::debug!(
                "Detected new UDP flow on {}, created OS socket at {}",
                listen_endpoint,
                os_socket.local_addr()?
            );

            // Convert IpListenEndpoint to IpEndpoint (use gateway IP if addr is None)
            let endpoint_addr = listen_endpoint.addr.unwrap_or(self.gateway_ip);
            let local_endpoint = IpEndpoint::new(endpoint_addr, listen_endpoint.port);

            let entry = NatEntry::Udp {
                os_socket,
                remote_endpoint: IpEndpoint::new(IpAddress::v4(0, 0, 0, 0), 0),
                local_endpoint,
                expires_at: Instant::now() + NAT_TIMEOUT_UDP,
            };

            self.nat_table.insert(handle, entry);
        }

        Ok(())
    }

    /// Clean up expired NAT entries that have been idle too long
    fn cleanup_expired_entries(&mut self) -> Result<()> {
        let now = Instant::now();

        // Find expired entries
        let expired: Vec<_> = self
            .nat_table
            .iter()
            .filter_map(|(handle, entry)| {
                if entry.is_expired(&now) {
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect();

        // Remove expired entries
        self.stats
            .nat_expired
            .fetch_add(expired.len(), Ordering::Relaxed);
        for handle in expired {
            self.nat_table.remove(&handle);

            // Close and remove the smoltcp socket
            self.sockets.remove(handle);
            // OS socket will be closed when dropped
        }

        Ok(())
    }

    /// Runs the NAT processing loop
    /// This method will continuously process packets until an error occurs or the channel is closed.
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
