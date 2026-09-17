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

#[cfg(feature = "https_stripping")]
mod https_stripping;

#[cfg(feature = "mactcp_helpers")]
mod mactcp_helpers;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};
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
    /// Set once the emulator has dropped its end of the channels
    disconnected: bool,
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
            disconnected: false,
        }
    }

    /// Drain routed packets that were intercepted and need NAT handling
    fn drain_intercepted_packets(&mut self) -> Vec<Packet> {
        std::mem::take(&mut self.intercepted_packets)
    }

    /// Check for IPv4 packets that the NAT gateway should not handle.
    /// Filters UDP packets that would cause OS bind/connect errors:
    ///   - src IP 0.0.0.0 (mostly BOOTP requests for MacTCP)
    ///   - broadcasts (255.255.255.255)
    ///   - UDP src port < 1024 (privileged ports as the emulator is likely running as non-root)
    ///   - UDP dst port == 520 (RIP)
    fn needs_filtering(&self, packet: &[u8]) -> bool {
        use smoltcp::wire::{IpProtocol, Ipv4Packet, UdpPacket};

        let eth_frame = match EthernetFrame::new_checked(packet) {
            Ok(frame) => frame,
            Err(_) => return false,
        };

        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return false;
        }

        let ipv4_packet = match Ipv4Packet::new_checked(eth_frame.payload()) {
            Ok(packet) => packet,
            Err(_) => return false,
        };

        if ipv4_packet.next_header() != IpProtocol::Udp {
            return false;
        }

        let udp_packet = match UdpPacket::new_checked(ipv4_packet.payload()) {
            Ok(packet) => packet,
            Err(_) => return false,
        };

        if ipv4_packet.dst_addr() == Ipv4Address::BROADCAST {
            return true;
        }

        let filtered = ipv4_packet.src_addr() == Ipv4Address::new(0, 0, 0, 0)
            || udp_packet.src_port() < 1024
            || udp_packet.dst_port() == 520;

        if filtered {
            log::debug!(
                "NAT: Filtered connection from {}:{} to {}:{}",
                ipv4_packet.src_addr(),
                udp_packet.src_port(),
                ipv4_packet.dst_addr(),
                udp_packet.dst_port()
            );
        }

        filtered
    }

    /// Check if a packet needs NAT (routed UDP or TCP SYN packet where destination IP != gateway IP)
    fn needs_nat(&self, packet: &[u8]) -> bool {
        use smoltcp::wire::{EthernetFrame, IpProtocol, Ipv4Packet, TcpPacket};

        // Parse Ethernet frame
        let eth_frame = match EthernetFrame::new_checked(packet) {
            Ok(frame) => frame,
            Err(_) => return false,
        };

        // Intercept RARP broadcasts so handle_rarp() can reply locally
        #[cfg(feature = "mactcp_helpers")]
        if eth_frame.ethertype() == EthernetProtocol::Unknown(0x8035) {
            return true;
        }

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
        let packet = match self.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(packet) => packet,
            Err(RecvTimeoutError::Timeout) => return None,
            Err(RecvTimeoutError::Disconnected) => {
                self.disconnected = true;
                return None;
            }
        };
        self.stats.rx_packets.fetch_add(1, Ordering::Relaxed);
        self.stats
            .rx_bytes
            .fetch_add(packet.len(), Ordering::Relaxed);

        // Drop packets that would cause OS errors (BOOTP, privileged src ports)
        if self.needs_filtering(&packet) {
            self.stats.filtered_packets.fetch_add(1, Ordering::Relaxed);
            self.stats
                .filtered_bytes
                .fetch_add(packet.len(), Ordering::Relaxed);
            return None;
        }

        // Intercept ICMP packets MacTCP Helpers can handle instead of smoltcp
        #[cfg(feature = "mactcp_helpers")]
        if mactcp_helpers::needs_icmp_answer(&packet) {
            self.intercepted_packets.push(packet);
            return None;
        }

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
            && operation == ArpOperation::Request
            && target_protocol_addr == self.gateway_ip
            && self
                .local_node
                .is_none_or(|(mac, ip)| mac != source_hardware_addr || ip != source_protocol_addr)
        {
            self.local_node = Some((source_hardware_addr, source_protocol_addr));
            self.next_gratuitous_arp = Instant::now() + GRATUITOUS_ARP_INTERVAL;
            log::debug!(
                "Learned local MAC address {} - IP address: {}",
                source_hardware_addr,
                source_protocol_addr
            );
        }

        // Drop ARP requests not targeting the NAT gateway
        // This prevents smoltcp from replying to gratuitous ARPs or claiming ownership
        // of other IP addresses. This happens because any_ip is enabled below
        if let Ok(ArpRepr::EthernetIpv4 {
            operation,
            target_protocol_addr,
            ..
        }) = EthernetFrame::new_checked(&packet)
            .and_then(|p| ArpPacket::new_checked(p.payload()))
            .and_then(|p| ArpRepr::parse(&p))
            && operation == ArpOperation::Request
            && target_protocol_addr != self.gateway_ip
        {
            return None;
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
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buffer)
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

        // Throttle ICMP replies to avoid a race condition within MacTCP
        #[cfg(feature = "mactcp_helpers")]
        mactcp_helpers::delay_icmp_reply(&buffer);

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

/// UDP NAT entry
struct UdpEntry {
    /// Userland UDP socket for external communication
    os_socket: UdpSocket,
    /// Remote (internet) endpoint
    remote_endpoint: IpEndpoint,
    /// Emulator's source endpoint
    local_endpoint: IpEndpoint,
    /// Time at which this entry expires
    expires_at: Instant,
}

/// TCP NAT entry
struct TcpEntry {
    /// Userland TCP socket for external communication
    os_socket: TcpStream,
    /// Remote (internet) endpoint
    remote_endpoint: IpEndpoint,
    /// Emulator's source endpoint
    local_endpoint: IpEndpoint,
    /// Time at which this entry expires
    expires_at: Instant,
    /// Whether the emulator's FIN has been passed on to the OS socket
    local_fin_forwarded: bool,
}

/// HTTPS stripping NAT entry
#[cfg(feature = "https_stripping")]
struct TcpHttpsStrippingEntry {
    /// HTTPS stripping wrapper around TLS connection (None until Host header is parsed)
    os_socket: Box<Option<https_stripping::HttpsStrippingStream>>,
    /// Remote (internet) endpoint (IP address and port 80, not 443)
    remote_endpoint: IpEndpoint,
    /// Emulator's source endpoint
    local_endpoint: IpEndpoint,
    /// Time at which this entry expires
    expires_at: Instant,
    /// Buffer for collecting initial HTTP request to extract Host header
    initial_buffer: Vec<u8>,
    /// Whether the emulator's FIN has been seen
    local_fin_forwarded: bool,
}

/// NAT connection tracking entry
enum NatEntry {
    Udp(UdpEntry),
    Tcp(TcpEntry),
    #[cfg(feature = "https_stripping")]
    TcpHttpsStripping(TcpHttpsStrippingEntry),
}

impl NatEntry {
    pub fn expires_at(&self) -> Instant {
        match self {
            Self::Udp(entry) => entry.expires_at,
            Self::Tcp(entry) => entry.expires_at,
            #[cfg(feature = "https_stripping")]
            Self::TcpHttpsStripping(entry) => entry.expires_at,
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
    pub filtered_packets: NatEngineStatCounter,
    pub filtered_bytes: NatEngineStatCounter,
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

    /// Whether to enable HTTPS stripping
    #[cfg(feature = "https_stripping")]
    https_stripping: bool,

    /// Gateway subnet prefix length (for ICMP Address Mask replies)
    #[cfg(feature = "mactcp_helpers")]
    gateway_subnet: u8,
}

impl NatEngine {
    /// Creates a new NAT engine instance with the given TX/RX channels
    ///
    /// * `tx` - Sender channel for packets going to the emulator
    /// * `rx` - Receiver channel for packets coming from the emulator
    /// * `gateway_mac` - MAC address of the NAT gateway
    /// * `gateway_ip` - IP address of the NAT gateway
    /// * `gateway_subnet` - NAT gateway subnet mask (CIDR)
    /// * `https_stripping` - Whether to enable HTTPS stripping
    pub fn new(
        tx: Sender<Packet>,
        rx: Receiver<Packet>,
        gateway_mac: [u8; 6],
        gateway_ip: [u8; 4],
        gateway_subnet: u8,
        #[cfg(feature = "https_stripping")] https_stripping: bool,
    ) -> Self {
        let stats = Arc::new(NatEngineStats::default());
        let gateway_mac_addr = EthernetAddress(gateway_mac);
        let gateway_ip_addr =
            IpAddress::v4(gateway_ip[0], gateway_ip[1], gateway_ip[2], gateway_ip[3]);

        let gateway_ipv4 = Ipv4Address::from(gateway_ip);
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
            #[cfg(feature = "https_stripping")]
            https_stripping,
            #[cfg(feature = "mactcp_helpers")]
            gateway_subnet,
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

        // Clean up expired NAT entries. This has to run between poll() and forwarding: a
        // socket aborted while forwarding only sends its RST on the next poll(), and reaping
        // it before then would leave the emulator believing the connection is still open.
        self.cleanup_expired_entries()?;

        // Detect new UDP flows and create NAT entries
        self.detect_new_flows()?;

        // Do forwarding
        self.forward_udp_os_to_smoltcp()?;
        self.forward_tcp_smoltcp_to_os()?;
        self.forward_tcp_os_to_smoltcp()?;
        #[cfg(feature = "https_stripping")]
        {
            self.forward_tls_to_smoltcp()?;
            self.forward_smoltcp_to_tls()?;
        }

        // Update active connections statistics
        self.stats.nat_active_tcp.store(
            self.nat_table
                .iter()
                .filter(|(_, e)| {
                    matches!(e, NatEntry::Tcp(_)) || {
                        #[cfg(feature = "https_stripping")]
                        {
                            matches!(e, NatEntry::TcpHttpsStripping(_))
                        }
                        #[cfg(not(feature = "https_stripping"))]
                        {
                            false
                        }
                    }
                })
                .count(),
            Ordering::Relaxed,
        );
        self.stats.nat_active_udp.store(
            self.nat_table
                .iter()
                .filter(|(_, e)| matches!(e, NatEntry::Udp(_)))
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

            #[cfg(feature = "mactcp_helpers")]
            if eth_frame.ethertype() == EthernetProtocol::Unknown(0x8035) {
                if let Err(e) = self.handle_rarp(&eth_frame) {
                    log::error!("Failed to handle RARP: {}", e);
                }
                continue;
            }

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
                #[cfg(feature = "mactcp_helpers")]
                IpProtocol::Icmp => {
                    let IpAddress::Ipv4(gw_ipv4) = self.gateway_ip;
                    match mactcp_helpers::handle_icmp_address_mask_request(
                        &eth_frame,
                        gw_ipv4,
                        self.gateway_mac,
                        self.gateway_subnet,
                    ) {
                        Ok(Some(buf)) => {
                            self.device.tx.try_send(buf).ok();
                        }
                        Ok(None) => {}
                        Err(e) => log::error!("Failed to handle ICMP address mask: {}", e),
                    }
                }
                _ => {
                    log::warn!("Unsupported protocol: {:?}", ipv4_packet.next_header());
                }
            }
        }

        Ok(())
    }

    /// Handle a RARP request by replying with the assigned IP for the client MAC
    #[cfg(feature = "mactcp_helpers")]
    fn handle_rarp(&self, eth_frame: &smoltcp::wire::EthernetFrame<&[u8]>) -> Result<()> {
        let IpAddress::Ipv4(gw_ipv4) = self.gateway_ip;

        let Some(buf) = mactcp_helpers::handle_rarp_request(eth_frame, gw_ipv4, self.gateway_mac)?
        else {
            return Ok(());
        };

        match self.device.tx.try_send(buf) {
            Ok(()) => {
                self.stats.tx_packets.fetch_add(1, Ordering::Relaxed);
                self.stats.tx_bytes.fetch_add(42, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.stats.tx_dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
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
            if let NatEntry::Udp(entry) = entry {
                let IpAddress::Ipv4(mac_ipv4) = entry.local_endpoint.addr;
                let mac_match = mac_ipv4 == src_ip && entry.local_endpoint.port == src_port;

                let IpAddress::Ipv4(remote_ipv4) = entry.remote_endpoint.addr;
                let remote_match = remote_ipv4 == dst_ip && entry.remote_endpoint.port == dst_port;

                mac_match && remote_match
            } else {
                false
            }
        });

        if let Some((handle, _entry)) = existing_entry {
            // Existing entry - forward to OS socket directly
            let handle = *handle;
            if let Some(NatEntry::Udp(entry)) = self.nat_table.get_mut(&handle) {
                entry.expires_at = Instant::now() + NAT_TIMEOUT_UDP;
                entry.os_socket.send(payload)?;
            }
        } else {
            // Create new NAT entry

            let os_socket = UdpSocket::bind("0.0.0.0:0")?;
            os_socket.set_nonblocking(true)?;

            let remote_addr = SocketAddr::new(std::net::IpAddr::V4(dst_ip), dst_port);
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

            // Bind to remote endpoint so responses appear to come from the actual remote.
            // TCP does a similar stuff. This works because any_ip is enabled.
            let bind_endpoint = IpEndpoint::new(IpAddress::Ipv4(dst_ip), dst_port);
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
            let entry = NatEntry::Udp(UdpEntry {
                os_socket,
                remote_endpoint: IpEndpoint::new(IpAddress::Ipv4(dst_ip), dst_port),
                local_endpoint: IpEndpoint::new(IpAddress::Ipv4(src_ip), src_port),
                expires_at: Instant::now() + NAT_TIMEOUT_UDP,
            });

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
            match entry {
                NatEntry::Tcp(entry) => {
                    let (IpAddress::Ipv4(mac_ip), IpAddress::Ipv4(remote_ip)) =
                        (entry.local_endpoint.addr, entry.remote_endpoint.addr);
                    mac_ip == src_ip
                        && entry.local_endpoint.port == src_port
                        && remote_ip == dst_ip
                        && entry.remote_endpoint.port == dst_port
                }
                #[cfg(feature = "https_stripping")]
                NatEntry::TcpHttpsStripping(entry) => {
                    // For HTTPS stripping, we check against the original port 80 connection
                    // Note: remote_endpoint stores port 80 (original), not 443
                    let (IpAddress::Ipv4(mac_ip), IpAddress::Ipv4(remote_ip)) =
                        (entry.local_endpoint.addr, entry.remote_endpoint.addr);
                    mac_ip == src_ip
                        && entry.local_endpoint.port == src_port
                        && remote_ip == dst_ip
                        && entry.remote_endpoint.port == dst_port
                }
                _ => false,
            }
        });

        if existing.is_some() {
            // Already in connection tracking table
            return Ok(());
        }

        // Check if this is a port 80 connection and HTTPS stripping is enabled
        #[cfg(feature = "https_stripping")]
        let use_https_stripping = self.https_stripping && dst_port == 80;
        #[cfg(not(feature = "https_stripping"))]
        let use_https_stripping = false;

        if use_https_stripping {
            #[cfg(feature = "https_stripping")]
            {
                log::debug!(
                    "NAT: HTTPS stripping enabled for {}:{} -> {}:80",
                    src_ip,
                    src_port,
                    dst_ip
                );

                // Create smoltcp TCP socket for the emulator side
                let rx_buffer = tcp::SocketBuffer::new(vec![0; SMOLTCP_BUFFER_SIZE]);
                let tx_buffer = tcp::SocketBuffer::new(vec![0; SMOLTCP_BUFFER_SIZE]);
                let mut socket = tcp::Socket::new(rx_buffer, tx_buffer);

                // Listen on port 80 on emulator side
                let dst_endpoint = IpEndpoint::new(IpAddress::Ipv4(dst_ip), 80);
                socket.listen(dst_endpoint)?;

                let handle = self.sockets.add(socket);

                // Create entry without OS socket (will be established when we get Host header)
                // Note: We store port 80 in remote_endpoint for duplicate detection,
                // actual TLS connection will be to port 443
                let entry = NatEntry::TcpHttpsStripping(TcpHttpsStrippingEntry {
                    os_socket: Box::new(None),
                    remote_endpoint: IpEndpoint::new(IpAddress::Ipv4(dst_ip), dst_port),
                    local_endpoint: IpEndpoint::new(IpAddress::Ipv4(src_ip), src_port),
                    expires_at: Instant::now() + NAT_TIMEOUT_TCP_OPEN,
                    initial_buffer: Vec::new(),
                    local_fin_forwarded: false,
                });

                self.nat_table.insert(handle, entry);
                self.stats.nat_total_tcp.fetch_add(1, Ordering::Relaxed);

                // Feed the SYN packet back to smoltcp so it can complete the handshake
                self.device.smoltcp_queue.push(raw_packet.to_vec());
            }
            #[cfg(not(feature = "https_stripping"))]
            unreachable!()
        } else {
            // Normal TCP connection
            let remote_addr = SocketAddr::new(std::net::IpAddr::V4(dst_ip), dst_port);

            let os_socket = match TcpStream::connect_timeout(&remote_addr, Duration::from_secs(5)) {
                Ok(s) => s,
                #[cfg(feature = "mactcp_helpers")]
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    let ack_num = (tcp_packet.seq_number().0 as u32).wrapping_add(1);
                    let buf = mactcp_helpers::build_tcp_rst(
                        _eth_frame.src_addr(),
                        self.gateway_mac,
                        dst_ip,
                        src_ip,
                        dst_port,
                        src_port,
                        ack_num,
                    );
                    self.device.tx.try_send(buf).ok();
                    log::debug!(
                        "NAT: Sent TCP RST to {}:{} for {}:{}",
                        src_ip,
                        src_port,
                        dst_ip,
                        dst_port
                    );
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };
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

            let entry = NatEntry::Tcp(TcpEntry {
                os_socket,
                remote_endpoint: IpEndpoint::new(IpAddress::Ipv4(dst_ip), dst_port),
                local_endpoint: IpEndpoint::new(IpAddress::Ipv4(src_ip), src_port),
                expires_at: Instant::now() + NAT_TIMEOUT_TCP_OPEN,
                local_fin_forwarded: false,
            });

            self.nat_table.insert(handle, entry);
            self.stats.nat_total_tcp.fetch_add(1, Ordering::Relaxed);

            // Feed the SYN packet back to smoltcp so it can complete the handshake
            self.device.smoltcp_queue.push(raw_packet.to_vec());
        }

        Ok(())
    }

    /// Forward data from smoltcp TCP sockets (emulator side) to OS sockets (Internet side)
    fn forward_tcp_smoltcp_to_os(&mut self) -> Result<()> {
        use smoltcp::socket::tcp;

        let handles: Vec<_> = self
            .nat_table
            .iter()
            .filter_map(|(handle, entry)| {
                if matches!(entry, NatEntry::Tcp(_)) {
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect();

        for handle in handles {
            let entry = match self.nat_table.get_mut(&handle) {
                Some(NatEntry::Tcp(entry)) => entry,
                _ => continue,
            };

            let socket = self.sockets.get_mut::<tcp::Socket>(handle);

            // Forward data from emulator (smoltcp) to Internet (OS socket)
            if socket.can_recv() {
                let mut write_failed = false;

                match socket.recv(|buffer| {
                    if !buffer.is_empty() {
                        // Write to OS socket and only consume as much as we could push
                        // out from the smoltcp receive buffer
                        match entry.os_socket.write(buffer) {
                            Ok(written) => (written, written),
                            Err(e) if Self::is_retryable(&e) => (0, 0),
                            Err(e) => {
                                log::warn!("Error writing to OS socket, closing connection: {}", e);
                                write_failed = true;
                                (0, 0)
                            }
                        }
                    } else {
                        (0, 0)
                    }
                }) {
                    Ok(_) => {
                        entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_OPEN;
                    }
                    Err(e) => {
                        log::error!("TCP error receiving from emulator: {:?}", e);
                    }
                };

                // The remote is gone, so the unsent data can never drain and the FIN below
                // would never be reached. Drop it, and close the emulator's side once what
                // is already queued for it has gone out.
                if write_failed {
                    Self::discard_received(socket);
                    socket.close();
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                }
            } else if !entry.local_fin_forwarded && Self::local_fin_received(socket) {
                // Emulator closed its side and everything it sent has been passed on:
                // half-close the OS socket so the remote sees the FIN too. The entry keeps
                // the open timeout: the remote may take its time with a response still to
                // come, and its own FIN moves the entry to the closed timeout.
                entry.local_fin_forwarded = true;
                self.stats.nat_tcp_fin_local.fetch_add(1, Ordering::Relaxed);
                if let Err(e) = entry.os_socket.shutdown(std::net::Shutdown::Write) {
                    log::debug!("Error shutting down OS socket for writing: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Forward data from smoltcp TCP sockets (emulator side) to TLS sockets
    #[cfg(feature = "https_stripping")]
    fn forward_smoltcp_to_tls(&mut self) -> Result<()> {
        use smoltcp::socket::tcp;

        // Handle HTTPS stripping connections
        let https_handles: Vec<_> = self
            .nat_table
            .iter()
            .filter_map(|(handle, entry)| {
                if matches!(entry, NatEntry::TcpHttpsStripping(_)) {
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect();

        // Track handles that failed TLS establishment and should be aborted
        let mut failed_handles = Vec::new();

        for handle in https_handles {
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);

            // Forward data from emulator (smoltcp) to Internet (TLS socket)
            if socket.can_recv() {
                let entry = match self.nat_table.get_mut(&handle) {
                    Some(NatEntry::TcpHttpsStripping(entry)) => entry,
                    _ => continue,
                };

                let mut tls_failed = false;
                let mut write_failed = false;

                match socket.recv(|buffer| {
                    if buffer.is_empty() {
                        return (0, 0);
                    }

                    // If TLS connection not yet established, buffer data and try to extract Host
                    if entry.os_socket.is_none() {
                        // Append to initial buffer
                        entry.initial_buffer.extend_from_slice(buffer);

                        // Try to parse Host header
                        match https_stripping::extract_http_host(&entry.initial_buffer) {
                            Ok(hostname) => {
                                log::debug!("HTTPS stripping: Extracted Host: {}", hostname);

                                // Establish TLS connection using the hostname for SNI
                                let IpAddress::Ipv4(ip) = entry.remote_endpoint.addr;
                                let dst_ip_addr = std::net::IpAddr::V4(ip);

                                match https_stripping::HttpsStrippingStream::connect(
                                    &hostname,
                                    dst_ip_addr,
                                    443,
                                ) {
                                    Ok(mut tls_stream) => {
                                        log::debug!(
                                            "Established TLS connection to {} ({}:443)",
                                            hostname,
                                            dst_ip_addr
                                        );

                                        match tls_stream.write_all(&entry.initial_buffer) {
                                            Ok(_) => {
                                                // Now switch to non-blocking mode for ongoing forwarding
                                                if let Err(e) = tls_stream.set_nonblocking(true) {
                                                    log::error!(
                                                        "Failed to set TLS stream non-blocking: {}",
                                                        e
                                                    );
                                                    tls_failed = true;
                                                    return (buffer.len(), buffer.len());
                                                }

                                                *entry.os_socket = Some(tls_stream);
                                                entry.expires_at =
                                                    Instant::now() + NAT_TIMEOUT_TCP_OPEN;
                                                entry.initial_buffer.clear();
                                                return (buffer.len(), buffer.len());
                                            }
                                            Err(e) => {
                                                log::error!("Failed to write buffered data: {}", e);
                                                tls_failed = true;
                                                return (buffer.len(), buffer.len());
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        log::error!(
                                            "Failed to establish TLS connection to {}:443: {}",
                                            hostname,
                                            e
                                        );
                                        tls_failed = true;
                                        return (buffer.len(), buffer.len());
                                    }
                                }
                            }
                            Err(_e) => {
                                // Not enough data yet or parse error
                                return (buffer.len(), buffer.len());
                            }
                        }
                    }

                    // TLS connection is established, forward data
                    if let Some(ref mut tls_socket) = *entry.os_socket {
                        match tls_socket.write(buffer) {
                            Ok(written) => {
                                entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_OPEN;
                                (written, written)
                            }
                            Err(e) if Self::is_retryable(&e) => (0, 0),
                            Err(e) => {
                                log::warn!(
                                    "Error writing to TLS socket, closing connection: {}",
                                    e
                                );
                                write_failed = true;
                                (0, 0)
                            }
                        }
                    } else {
                        (0, 0)
                    }
                }) {
                    Ok(_) => {}
                    Err(e) => {
                        log::error!(
                            "TCP error receiving from emulator (HTTPS stripping): {:?}",
                            e
                        );
                    }
                }

                // As for plain TCP: drop what can't be passed on, and close the emulator's
                // side once what is already queued for it has gone out
                if write_failed {
                    Self::discard_received(socket);
                    socket.close();
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                }

                // If TLS failed, mark for abort
                if tls_failed {
                    failed_handles.push(handle);
                }
            } else if let Some(NatEntry::TcpHttpsStripping(entry)) = self.nat_table.get_mut(&handle)
                && !entry.local_fin_forwarded
                && Self::local_fin_received(socket)
            {
                // Emulator closed its side. Unlike plain TCP this is not passed on: a TLS
                // half-close (close_notify) makes many servers tear down the whole
                // connection, which would cut off a response still in flight. The remote
                // closing after its response ends the connection as usual, so the entry
                // keeps the open timeout until then.
                entry.local_fin_forwarded = true;
                self.stats.nat_tcp_fin_local.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Abort connections that failed TLS establishment
        for handle in failed_handles {
            log::error!("Aborting connection due to TLS failure");
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);
            // Removed once the RST is out, when the socket has reached CLOSED
            Self::discard_received(socket);
            socket.abort();
        }

        Ok(())
    }

    /// Forward data from OS UDP sockets back to the emulator (via smoltcp)
    fn forward_udp_os_to_smoltcp(&mut self) -> Result<()> {
        let handles: Vec<_> = self
            .nat_table
            .iter()
            .filter_map(|(handle, entry)| {
                if matches!(entry, NatEntry::Udp(_)) {
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect();

        for handle in handles {
            let local_endpoint = match self.nat_table.get(&handle) {
                Some(NatEntry::Udp(entry)) => entry.local_endpoint,
                _ => continue,
            };

            // Get mutable access to entry
            let entry = match self.nat_table.get_mut(&handle) {
                Some(NatEntry::Udp(entry)) => entry,
                _ => continue,
            };

            // Try to receive from OS socket (response from internet)
            match entry.os_socket.recv_from(&mut self.recv_buffer) {
                Ok((len, _from_addr)) => {
                    // Keep entry alive
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_UDP;

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

    /// Whether an error from a non-blocking Internet-side socket only means to try again later
    fn is_retryable(e: &std::io::Error) -> bool {
        matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        )
    }

    /// Throw away whatever the emulator sent that has not been passed on yet
    fn discard_received(socket: &mut smoltcp::socket::tcp::Socket) {
        while socket.can_recv() && socket.recv(|buf| (buf.len(), ())).is_ok() {}
    }

    /// Whether the emulator has closed its transmit half of the connection (sent FIN).
    ///
    /// Once the receive buffer has been drained, `can_recv()` is false in these states so
    /// `recv()` would never report `RecvError::Finished`; the state has to be checked directly.
    fn local_fin_received(socket: &smoltcp::socket::tcp::Socket) -> bool {
        use smoltcp::socket::tcp::State;

        matches!(
            socket.state(),
            State::CloseWait | State::LastAck | State::Closing | State::TimeWait
        )
    }

    /// Pump data from an Internet-side reader into a smoltcp TCP socket's transmit buffer.
    ///
    /// Data is read directly into the free space of the smoltcp transmit buffer, so the
    /// OS socket only hands over as much as smoltcp can actually queue. Whatever does not
    /// fit stays in the OS socket's receive buffer, which applies TCP backpressure to the
    /// remote instead of silently discarding the data (which would corrupt the stream
    /// towards the emulator once the emulator falls behind).
    ///
    /// Returns `Ok(None)` if the remote closed the connection (EOF), otherwise
    /// `Ok(Some(bytes_queued))`. A `WouldBlock` or `Interrupted` from the reader is
    /// reported as `Ok(Some(0))`.
    fn pump_reader_to_socket<R: Read>(
        reader: &mut R,
        socket: &mut smoltcp::socket::tcp::Socket,
    ) -> std::io::Result<Option<usize>> {
        let mut total = 0;

        // The ring buffer hands out its largest contiguous free slice, which may be
        // smaller than the total free space near the wrap-around point, so keep going
        // while the reader is still filling what we offer.
        while socket.can_send() {
            let mut offered = 0;
            let outcome = socket.send(|buf| {
                offered = buf.len();
                if buf.is_empty() {
                    return (0, Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)));
                }
                match reader.read(buf) {
                    Ok(n) => (n, Ok(n)),
                    Err(e) => (0, Err(e)),
                }
            });

            match outcome {
                Ok(Ok(0)) => return Ok(None),
                Ok(Ok(n)) => {
                    total += n;
                    if n < offered {
                        break;
                    }
                }
                Ok(Err(e)) if Self::is_retryable(&e) => break,
                Ok(Err(e)) => return Err(e),
                Err(e) => {
                    log::warn!("Error sending to smoltcp socket: {}", e);
                    break;
                }
            }
        }

        Ok(Some(total))
    }

    /// Forward TCP data from OS sockets (Internet side) to smoltcp sockets (emulator side)
    fn forward_tcp_os_to_smoltcp(&mut self) -> Result<()> {
        use smoltcp::socket::tcp;

        // Handle regular TCP connections
        let handles: Vec<_> = self
            .nat_table
            .iter()
            .filter_map(|(handle, entry)| {
                if matches!(entry, NatEntry::Tcp(_)) {
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect();

        for handle in handles {
            let entry = match self.nat_table.get_mut(&handle) {
                Some(NatEntry::Tcp(entry)) => entry,
                _ => continue,
            };

            let socket = self.sockets.get_mut::<tcp::Socket>(handle);

            // Forward data from Internet (OS socket) to emulator (smoltcp)
            // smoltcp will handle segmentation and MTU
            match Self::pump_reader_to_socket(&mut entry.os_socket, socket) {
                Ok(None) => {
                    // Connection closed by remote
                    self.stats
                        .nat_tcp_fin_remote
                        .fetch_add(1, Ordering::Relaxed);
                    socket.close();
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                }
                Ok(Some(0)) => {
                    // No data available or transmit buffer full
                }
                Ok(Some(_)) => {
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_OPEN;
                }
                Err(e) => {
                    // The error won't clear on its own, so close the emulator's side instead
                    // of retrying the read on every pass until the entry times out. What is
                    // already queued for the emulator still goes out ahead of the FIN: a
                    // server that closes with part of the request unread resets the
                    // connection right after sending its whole response.
                    log::warn!("Error receiving from OS socket, closing connection: {}", e);
                    socket.close();
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                }
            }
        }

        Ok(())
    }

    /// Forward TCP data from TLS sockets (Internet side) to smoltcp sockets (emulator side)
    #[cfg(feature = "https_stripping")]
    fn forward_tls_to_smoltcp(&mut self) -> Result<()> {
        use smoltcp::socket::tcp;

        let https_handles: Vec<_> = self
            .nat_table
            .iter()
            .filter_map(|(handle, entry)| {
                if matches!(entry, NatEntry::TcpHttpsStripping(_)) {
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect();

        for handle in https_handles {
            let entry = match self.nat_table.get_mut(&handle) {
                Some(NatEntry::TcpHttpsStripping(entry)) => entry,
                _ => continue,
            };

            // Skip if TLS connection not yet established
            let Some(ref mut tls_socket) = *entry.os_socket else {
                continue;
            };

            let socket = self.sockets.get_mut::<tcp::Socket>(handle);

            // The https:// -> http:// rewriting happens in the Read impl
            let result = match Self::pump_reader_to_socket(tls_socket, socket) {
                // Many servers close TCP without sending close_notify, after which rustls
                // fails every read with UnexpectedEof. Treat it as the remote closing.
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    log::debug!("HTTPS stripping: TLS socket closed without close_notify");
                    Ok(None)
                }
                result => result,
            };
            match result {
                Ok(None) => {
                    // Connection closed by remote
                    log::debug!("HTTPS stripping: TLS socket closed by remote");
                    self.stats
                        .nat_tcp_fin_remote
                        .fetch_add(1, Ordering::Relaxed);
                    socket.close();
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                }
                Ok(Some(0)) => {
                    // No data available or transmit buffer full
                }
                Ok(Some(_)) => {
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_OPEN;
                }
                Err(e) => {
                    // As for plain TCP: close the emulator's side after what is already
                    // queued for it rather than retrying the read on every pass
                    log::error!("Error receiving from TLS socket, closing connection: {}", e);
                    socket.close();
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
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

            let entry = NatEntry::Udp(UdpEntry {
                os_socket,
                remote_endpoint: IpEndpoint::new(IpAddress::v4(0, 0, 0, 0), 0),
                local_endpoint,
                expires_at: Instant::now() + NAT_TIMEOUT_UDP,
            });

            self.nat_table.insert(handle, entry);
        }

        Ok(())
    }

    /// Clean up expired NAT entries that have been idle too long, and TCP entries whose
    /// connection with the emulator has fully closed (or was reset)
    fn cleanup_expired_entries(&mut self) -> Result<()> {
        use smoltcp::socket::tcp;

        let now = Instant::now();

        let is_tcp = |entry: &NatEntry| {
            matches!(entry, NatEntry::Tcp(_)) || {
                #[cfg(feature = "https_stripping")]
                {
                    matches!(entry, NatEntry::TcpHttpsStripping(_))
                }
                #[cfg(not(feature = "https_stripping"))]
                {
                    false
                }
            }
        };

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

        // Find TCP entries whose smoltcp socket has reached CLOSED. A socket can get there
        // (LAST-ACK -> CLOSED) with data from the emulator still waiting for the OS socket
        // to take it; those stay until it has been passed on.
        let closed: Vec<_> = self
            .nat_table
            .iter()
            .filter(|(handle, entry)| {
                // Only look the socket up for TCP entries: get() panics on a type mismatch
                is_tcp(entry) && !expired.contains(handle) && {
                    let socket = self.sockets.get::<tcp::Socket>(**handle);
                    socket.state() == tcp::State::Closed && !socket.can_recv()
                }
            })
            .map(|(handle, _)| *handle)
            .collect();

        // An expired TCP entry whose socket is still open is reset first, so the emulator
        // is not left waiting on a connection that is gone. It is removed on the next pass,
        // once poll() has sent the RST.
        let expired: Vec<_> = expired
            .into_iter()
            .filter(|handle| {
                if is_tcp(&self.nat_table[handle]) {
                    let socket = self.sockets.get_mut::<tcp::Socket>(*handle);
                    if socket.state() != tcp::State::Closed {
                        Self::discard_received(socket);
                        socket.abort();
                        return false;
                    }
                }
                true
            })
            .collect();

        // Remove expired entries
        self.stats
            .nat_expired
            .fetch_add(expired.len(), Ordering::Relaxed);
        for handle in expired.into_iter().chain(closed) {
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
            if self.device.disconnected {
                log::info!("NAT emulator link closed");
                break;
            }

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
