//! NAT implementation for Snow
//!
//! This module provides Network Address Translation (NAT) functionality
//! for the emulated Ethernet interface. It receives layer 2 packets from
//! the emulator via a crossbeam channel, processes them through a NAT
//! implementation using smoltcp, and sends responses back.
//!
//! The gateway's network can hold several Ethernet adapters of the emulated system
//! (`NatNetwork::attach()`). It switches frames between them and the gateway by their
//! destination address.
//!
//! The remote connectivity is implemented fully through userland (OS) sockets.
//!
//! Currently only supports TCP and UDP. The gateway also answers BOOTP and DHCP requests
//! (dhcp.rs), and passes DNS queries sent to it on to the host's DNS server. Broadcasts and
//! multicast stay on the emulated network, and the host's own addresses (127.0.0.0/8,
//! 0.0.0.0/8) can't be reached through it.
//!
//! The MacOS TCP/IP stack seems to struggle with ARP requests during already
//! active TCP sessions which can stall connections and leave smoltcp in a
//! 'neighbor discovery pending' state. To avoid this, we send unsolicited ARP
//! replies to smoltcp regularly to keep the neighbor cache entry from being evicted.

mod dhcp;
#[cfg(feature = "https_stripping")]
mod https_stripping;

#[cfg(feature = "mactcp_helpers")]
mod mactcp_helpers;

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::udp;
use smoltcp::wire::{
    ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol, IpAddress,
    IpCidr, IpEndpoint, Ipv4Address,
};

/// A layer 2 Ethernet packet
pub type Packet = Vec<u8>;

/// UDP port of DNS
const DNS_PORT: u16 = 53;

/// The host's DNS server: the first one in /etc/resolv.conf
#[cfg(not(windows))]
pub fn host_dns_server() -> Option<std::net::IpAddr> {
    let conf = std::fs::read_to_string("/etc/resolv.conf").ok()?;
    conf.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next() == Some("nameserver"))
            .then(|| words.next()?.parse().ok())
            .flatten()
    })
}

/// The host's DNS server: the first one of the network adapter that is up, has a gateway and,
/// of those, the lowest metric, the one that the host's traffic goes out through
#[cfg(windows)]
pub fn host_dns_server() -> Option<std::net::IpAddr> {
    use windows::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_FRIENDLY_NAME,
        GAA_FLAG_SKIP_MULTICAST, GAA_FLAG_SKIP_UNICAST, GetAdaptersAddresses,
        IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::AF_UNSPEC;

    let flags = GAA_FLAG_SKIP_UNICAST
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_FRIENDLY_NAME
        | GAA_FLAG_INCLUDE_GATEWAYS;
    // Of u64s, for the alignment of the adapters' structures; 16 KB as Microsoft advises, and
    // more if that is too little
    let mut buffer: Vec<u64> = vec![0; 2048];
    loop {
        let mut size = u32::try_from(buffer.len() * 8).ok()?;
        // SAFETY: the buffer holds `size` bytes, aligned for IP_ADAPTER_ADDRESSES_LH
        let result = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC.0),
                flags,
                None,
                Some(buffer.as_mut_ptr().cast()),
                &raw mut size,
            )
        };
        if result == ERROR_BUFFER_OVERFLOW.0 {
            buffer.resize(size as usize / 8 + 1, 0);
        } else if result == 0 {
            break;
        } else {
            return None;
        }
    }

    let mut best: Option<(u32, std::net::IpAddr)> = None;
    let mut adapter = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    // SAFETY: GetAdaptersAddresses filled the buffer with a list of adapters, each with its
    // list of DNS servers, which live as long as the buffer
    while let Some(a) = unsafe { adapter.as_ref() } {
        if a.OperStatus == IfOperStatusUp
            && !a.FirstGatewayAddress.is_null()
            && best.is_none_or(|(metric, _)| a.Ipv4Metric < metric)
        {
            let mut server = a.FirstDnsServerAddress;
            // SAFETY: as above
            while let Some(s) = unsafe { server.as_ref() } {
                // SAFETY: as above
                if let Some(dns) = unsafe { windows_address(s.Address.lpSockaddr) } {
                    best = Some((a.Ipv4Metric, dns));
                    break;
                }
                server = s.Next;
            }
        }
        adapter = a.Next;
    }
    best.map(|(_, dns)| dns)
}

/// The address of a socket address of Windows, if it is one that a socket can reach without
/// more: not IPv6 link-local, which needs the interface too, nor IPv6 site-local, deprecated
/// (RFC 3879), which Windows lists as fec0:0:0:ffff::1 to 3 when the adapter has no IPv6 DNS
/// server
///
/// # Safety
/// `address` is null or points to a SOCKADDR_IN or SOCKADDR_IN6 as its family says
#[cfg(windows)]
unsafe fn windows_address(
    address: *const windows::Win32::Networking::WinSock::SOCKADDR,
) -> Option<std::net::IpAddr> {
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6, SOCKADDR_IN, SOCKADDR_IN6};

    // SAFETY: by the caller
    let family = unsafe { address.as_ref()? }.sa_family;
    if family == AF_INET {
        // SAFETY: by the caller; S_addr is in network order, as are its bytes in memory
        let ip = unsafe { (*address.cast::<SOCKADDR_IN>()).sin_addr.S_un.S_addr };
        Some(std::net::Ipv4Addr::from(ip.to_ne_bytes()).into())
    } else if family == AF_INET6 {
        // SAFETY: by the caller
        let ip =
            std::net::Ipv6Addr::from(unsafe { (*address.cast::<SOCKADDR_IN6>()).sin6_addr.u.Byte });
        let prefix = ip.segments()[0] & 0xffc0;
        (prefix != 0xfe80 && prefix != 0xfec0).then_some(ip.into())
    } else {
        None
    }
}

/// How long the host's DNS server, once looked up, is used before the next DNS query looks it up
/// again: it changes with the host's network, as on a switch to another Wi-Fi network or to a
/// VPN, but a lookup on every query would cost too much
const HOST_DNS_LIFETIME: Duration = Duration::from_secs(10);

/// Where DNS queries to the gateway go: the server set, or else the host's, looked up again
/// at most every HOST_DNS_LIFETIME
struct DnsServer {
    /// The server set, if any
    set: Option<std::net::IpAddr>,
    /// The host's, as last looked up
    host: Option<std::net::IpAddr>,
    /// When the host's was looked up
    looked_up: Instant,
}

impl DnsServer {
    fn new(set: Option<std::net::IpAddr>, now: Instant) -> Self {
        let host = if set.is_none() {
            host_dns_server()
        } else {
            None
        };
        Self {
            set,
            host,
            looked_up: now,
        }
    }

    /// The server to use at `now`, with `lookup` to look up the host's
    fn get(
        &mut self,
        now: Instant,
        lookup: impl FnOnce() -> Option<std::net::IpAddr>,
    ) -> Option<std::net::IpAddr> {
        if self.set.is_some() {
            return self.set;
        }
        if now.duration_since(self.looked_up) >= HOST_DNS_LIFETIME {
            let host = lookup();
            if host != self.host {
                log::info!("NAT: the host's DNS server is now {:?}", host);
            }
            self.host = host;
            self.looked_up = now;
        }
        self.host
    }
}

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

/// How long to wait for a packet from the emulator when nothing else is happening
/// and there are no NAT entries whose OS sockets need polling.
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long to wait for a packet from the emulator when nothing is happening but
/// there are NAT entries. The OS sockets are non-blocking and cannot be waited on
/// together with the channel, so they are polled at this interval.
const ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// An Ethernet adapter of the emulated system, on the gateway's network
struct GuestPort {
    /// Its Ethernet address
    mac: EthernetAddress,
    /// The channel of the frames for it
    tx: Sender<Packet>,
    /// Whether its receiver is on: the frames for it are dropped while it is off
    rx_enabled: Arc<AtomicBool>,
    /// The address the gateway gave it, 0 for none. The adapter keeps it, in save states too,
    /// and brings it back when it attaches again.
    lease: Arc<AtomicU32>,
    /// Its connections to port 80 go out as HTTPS (https_stripping.rs)
    #[cfg(feature = "https_stripping")]
    https_stripping: bool,
}

/// The adapters of the emulated system on the gateway's network, as on a switch: a frame goes
/// to the adapter of its destination address, a broadcast or multicast frame to all of them.
/// An adapter that has closed its channel is taken off.
#[derive(Clone, Default)]
struct GuestTx {
    ports: Arc<Mutex<Vec<GuestPort>>>,
}

impl GuestTx {
    /// Adds an adapter, in place of one with the same address
    fn attach(&self, port: GuestPort) {
        let mut ports = self.ports.lock().unwrap();
        ports.retain(|p| p.mac != port.mac);
        ports.push(port);
    }

    /// The address that the adapter at `mac` holds, if it holds one, and those that the other
    /// adapters hold
    fn leases(&self, mac: EthernetAddress) -> (Option<Ipv4Address>, Vec<Ipv4Address>) {
        let mut own = None;
        let mut others = vec![];
        for port in self.ports.lock().unwrap().iter() {
            let lease = port.lease.load(Ordering::Relaxed);
            if lease == 0 {
                continue;
            }
            if port.mac == mac {
                own = Some(Ipv4Address::from(lease));
            } else {
                others.push(Ipv4Address::from(lease));
            }
        }
        (own, others)
    }

    /// Gives the adapter at `mac` the address `ip`
    fn set_lease(&self, mac: EthernetAddress, ip: Ipv4Address) {
        for port in self.ports.lock().unwrap().iter() {
            if port.mac == mac {
                port.lease.store(u32::from(ip), Ordering::Relaxed);
            }
        }
    }

    /// Sends a frame to the adapters it is for, but the one at `except`. Says whether one took
    /// it.
    fn deliver(&self, packet: &[u8], except: Option<EthernetAddress>) -> bool {
        let Ok(eth_frame) = EthernetFrame::new_checked(packet) else {
            return false;
        };
        let dst = eth_frame.dst_addr();
        let mut taken = false;
        self.ports.lock().unwrap().retain(|port| {
            if Some(port.mac) == except
                || (dst.is_unicast() && dst != port.mac)
                || !port.rx_enabled.load(Ordering::Relaxed)
            {
                return true;
            }
            match port.tx.try_send(packet.to_vec()) {
                Ok(()) => {
                    taken = true;
                    true
                }
                Err(TrySendError::Full(_)) => true,
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
        taken
    }

    /// Sends a frame from the gateway. A frame that no adapter takes is dropped, as if the
    /// queue were full.
    fn try_send(&self, packet: Packet) -> std::result::Result<(), TrySendError<Packet>> {
        if self.deliver(&packet, None) {
            Ok(())
        } else {
            Err(TrySendError::Full(packet))
        }
    }

    /// Passes a frame from an adapter to the others. A frame for another adapter goes to it
    /// alone, and that is said; a broadcast or multicast frame goes to the others, and on to
    /// the gateway too.
    fn forward(&self, packet: &[u8]) -> bool {
        let Ok(eth_frame) = EthernetFrame::new_checked(packet) else {
            return false;
        };
        let dst = eth_frame.dst_addr();
        let to_guest = dst.is_unicast()
            && self
                .ports
                .lock()
                .unwrap()
                .iter()
                .any(|port| port.mac == dst);
        if to_guest || !dst.is_unicast() {
            self.deliver(packet, Some(eth_frame.src_addr()));
        }
        to_guest
    }

    /// Whether the adapter at `mac` strips HTTPS
    #[cfg(feature = "https_stripping")]
    fn strips_https(&self, mac: EthernetAddress) -> bool {
        self.ports
            .lock()
            .unwrap()
            .iter()
            .any(|port| port.mac == mac && port.https_stripping)
    }
}

/// Virtual network device that bridges crossbeam channels with smoltcp
struct VirtualDevice {
    rx: Receiver<Packet>,
    tx: GuestTx,
    stats: Arc<NatEngineStats>,
    /// Queue for routed UDP/TCP packets that need NAT handling (intercepted before smoltcp)
    intercepted_packets: Vec<Packet>,
    /// Queue for packets to feed back to smoltcp after processing (e.g., TCP SYN after creating listening socket)
    smoltcp_queue: Vec<Packet>,
    /// Gateway IP address to detect routed packets
    gateway_ip: Ipv4Address,
    /// The broadcast address of the gateway's subnet
    subnet_broadcast: Ipv4Address,
    /// Learned local nodes (for gratuitous ARP): one address per adapter
    local_nodes: Vec<(EthernetAddress, Ipv4Address)>,
    /// The address that each Ethernet address on the emulated network uses, the last one its
    /// ARP packets showed, whether DHCP gave it or it was set by hand: DHCP, BOOTP and RARP hand
    /// none of them out to another adapter
    arp_addresses: Vec<(EthernetAddress, Ipv4Address)>,
    /// Next time gratuitous ARP should be sent
    next_gratuitous_arp: Instant,
    /// Packets received from the emulator while waiting in `wait_for_packet()`,
    /// handed to smoltcp on the next `receive()`
    pending: VecDeque<Packet>,
    /// Set once the emulator has dropped its end of the channels
    disconnected: bool,
}

impl VirtualDevice {
    fn new(
        tx: GuestTx,
        rx: Receiver<Packet>,
        stats: Arc<NatEngineStats>,
        gateway_ip: Ipv4Address,
        subnet_broadcast: Ipv4Address,
    ) -> Self {
        Self {
            rx,
            tx,
            stats,
            intercepted_packets: Vec::new(),
            smoltcp_queue: Vec::new(),
            gateway_ip,
            subnet_broadcast,
            local_nodes: Vec::new(),
            arp_addresses: Vec::new(),
            next_gratuitous_arp: Instant::now(),
            pending: VecDeque::new(),
            disconnected: false,
        }
    }

    /// Block until a packet arrives from the emulator or `timeout` elapses.
    /// A received packet is queued for the next `receive()` call.
    fn wait_for_packet(&mut self, timeout: Duration) {
        match self.rx.recv_timeout(timeout) {
            Ok(packet) => self.pending.push_back(packet),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => self.disconnected = true,
        }
    }

    /// Drain routed packets that were intercepted and need NAT handling
    fn drain_intercepted_packets(&mut self) -> Vec<Packet> {
        std::mem::take(&mut self.intercepted_packets)
    }

    /// Whether a packet is for the host's own addresses, which a gateway must not pass on:
    /// loopback (127.0.0.0/8) and "this network" (0.0.0.0/8) (RFC 1122, 3.2.1.3). A host
    /// socket would reach the services of the host itself.
    fn to_host_only_address(packet: &[u8]) -> bool {
        use smoltcp::wire::Ipv4Packet;

        let Ok(eth_frame) = EthernetFrame::new_checked(packet) else {
            return false;
        };
        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return false;
        }
        let Ok(ipv4_packet) = Ipv4Packet::new_checked(eth_frame.payload()) else {
            return false;
        };
        matches!(ipv4_packet.dst_addr().octets()[0], 0 | 127)
    }

    /// The reset that answers a TCP segment from the guest, as from a host with nothing
    /// listening (RFC 793, 3.4 "Reset Generation"); None for anything else, and for a reset
    fn tcp_reset(packet: &[u8]) -> Option<Packet> {
        use smoltcp::wire::{IpProtocol, Ipv4Packet, TcpPacket, TcpSeqNumber};

        let eth_frame = EthernetFrame::new_checked(packet).ok()?;
        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return None;
        }
        let ipv4_packet = Ipv4Packet::new_checked(eth_frame.payload()).ok()?;
        if ipv4_packet.next_header() != IpProtocol::Tcp {
            return None;
        }
        let tcp_packet = TcpPacket::new_checked(ipv4_packet.payload()).ok()?;
        if tcp_packet.rst() {
            return None;
        }

        let mut buffer = vec![0; 14 + 20 + 20];
        let mut eth = EthernetFrame::new_unchecked(&mut buffer[..]);
        eth.set_dst_addr(eth_frame.src_addr());
        eth.set_src_addr(eth_frame.dst_addr());
        eth.set_ethertype(EthernetProtocol::Ipv4);
        let mut ip = Ipv4Packet::new_unchecked(eth.payload_mut());
        ip.set_version(4);
        ip.set_header_len(20);
        ip.set_total_len(40);
        ip.set_hop_limit(64);
        ip.set_next_header(IpProtocol::Tcp);
        ip.set_src_addr(ipv4_packet.dst_addr());
        ip.set_dst_addr(ipv4_packet.src_addr());
        ip.fill_checksum();
        let mut tcp = TcpPacket::new_unchecked(ip.payload_mut());
        tcp.set_src_port(tcp_packet.dst_port());
        tcp.set_dst_port(tcp_packet.src_port());
        tcp.set_header_len(20);
        tcp.clear_flags();
        tcp.set_rst(true);
        if tcp_packet.ack() {
            // The reset takes its sequence number from the segment's acknowledgment
            tcp.set_seq_number(tcp_packet.ack_number());
        } else {
            // Sequence number zero, and the segment acknowledged
            tcp.set_seq_number(TcpSeqNumber(0));
            tcp.set_ack(true);
            tcp.set_ack_number(tcp_packet.seq_number() + tcp_packet.segment_len());
        }
        tcp.fill_checksum(
            &IpAddress::Ipv4(ipv4_packet.dst_addr()),
            &IpAddress::Ipv4(ipv4_packet.src_addr()),
        );
        Some(buffer)
    }

    /// Check for IPv4 packets that the NAT gateway should not handle.
    /// Filters UDP packets that would cause OS bind/connect errors:
    ///   - src IP 0.0.0.0 (mostly BOOTP requests for MacTCP)
    ///   - broadcasts (255.255.255.255, and to the gateway's subnet)
    ///   - multicast and reserved addresses (224.0.0.0 and up)
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

        // Broadcasts, to all networks or to the gateway's subnet, stay on the emulated network,
        // and so do multicast groups and the reserved addresses: classes D and E, 224.0.0.0 and
        // up (RFC 1112, 4). A host socket would send a group's datagrams out on the host's
        // network, and its own programs would get them too, but never let the answers back in.
        let dst = ipv4_packet.dst_addr();
        if dst == Ipv4Address::BROADCAST || dst == self.subnet_broadcast || dst.octets()[0] >= 224 {
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
        use smoltcp::wire::{EthernetFrame, IpProtocol, Ipv4Packet, TcpPacket, UdpPacket};

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

        // Check if destination is NOT the gateway (i.e., it's being routed). DNS queries to
        // the gateway go to the host's DNS server, as routed UDP.
        let dst_ip = ipv4_packet.dst_addr();
        if dst_ip == self.gateway_ip {
            return ipv4_packet.next_header() == IpProtocol::Udp
                && UdpPacket::new_checked(ipv4_packet.payload())
                    .is_ok_and(|udp| udp.dst_port() == DNS_PORT);
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
        // Check if it is time to send gratuitous ARP: one for each local node, queued for smoltcp
        let now = Instant::now();
        if !self.local_nodes.is_empty() && self.next_gratuitous_arp < now {
            self.next_gratuitous_arp = now + GRATUITOUS_ARP_INTERVAL;
            for &(local_mac, local_ip) in &self.local_nodes {
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
                self.smoltcp_queue.push(buffer);
            }
        }

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

        // Receive new packet from Ethernet adapter channel. This must not block:
        // smoltcp calls receive() until it returns None before it runs egress, so
        // blocking here would delay every transmission to the emulator. Waiting for
        // packets is done in NatEngine::process() when there is nothing else to do.
        let packet = match self.pending.pop_front() {
            Some(packet) => packet,
            None => match self.rx.try_recv() {
                Ok(packet) => packet,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    return None;
                }
            },
        };
        self.stats.rx_packets.fetch_add(1, Ordering::Relaxed);
        self.stats
            .rx_bytes
            .fetch_add(packet.len(), Ordering::Relaxed);

        // A frame for another adapter of the emulated system goes to it alone; broadcast and
        // multicast go to the others as well
        if self.tx.forward(&packet) {
            return None;
        }

        // The host's own addresses are no way out: TCP gets a reset, the rest is dropped
        if Self::to_host_only_address(&packet) {
            if let Some(reset) = Self::tcp_reset(&packet) {
                let len = reset.len();
                if self.tx.try_send(reset).is_ok() {
                    self.stats.tx_packets.fetch_add(1, Ordering::Relaxed);
                    self.stats.tx_bytes.fetch_add(len, Ordering::Relaxed);
                }
            }
            self.stats.filtered_packets.fetch_add(1, Ordering::Relaxed);
            self.stats
                .filtered_bytes
                .fetch_add(packet.len(), Ordering::Relaxed);
            return None;
        }

        // BOOTP and DHCP requests are for the gateway's own server
        if dhcp::is_request(&packet) {
            self.intercepted_packets.push(packet);
            return None;
        }

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

        // The address an adapter uses, as its ARP packets show it. A probe for an address
        // (RFC 5227), from 0.0.0.0, doesn't use one yet.
        if let Ok(eth) = EthernetFrame::new_checked(&packet)
            && eth.ethertype() == EthernetProtocol::Arp
            && let Ok(ArpRepr::EthernetIpv4 {
                source_hardware_addr,
                source_protocol_addr,
                ..
            }) = ArpPacket::new_checked(eth.payload()).and_then(|p| ArpRepr::parse(&p))
            && !source_protocol_addr.is_unspecified()
            && !self
                .arp_addresses
                .contains(&(source_hardware_addr, source_protocol_addr))
        {
            self.arp_addresses
                .retain(|&(mac, _)| mac != source_hardware_addr);
            self.arp_addresses
                .push((source_hardware_addr, source_protocol_addr));
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
            && !self
                .local_nodes
                .contains(&(source_hardware_addr, source_protocol_addr))
        {
            self.local_nodes
                .retain(|&(mac, _)| mac != source_hardware_addr);
            self.local_nodes
                .push((source_hardware_addr, source_protocol_addr));
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
    tx: GuestTx,
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

    /// Gateway subnet prefix length (for ICMP Address Mask replies)
    #[cfg(feature = "mactcp_helpers")]
    gateway_subnet: u8,

    /// What the DHCP server hands out, but the address
    lease: dhcp::Lease,

    /// The addresses handed out
    addresses: Addresses,

    /// Where DNS queries to the gateway go
    dns: DnsServer,
}

/// The addresses that the gateway hands out (DHCP, BOOTP, RARP): one for each Ethernet address,
/// always the same. An adapter that comes back with the address it had, from a save state, gets
/// it again if it is still free; the others get the lowest free address after the gateway's.
struct Addresses {
    /// The first one, after the gateway's
    first: u32,
    /// The last one, before the network's broadcast address
    last: u32,
    /// Those given
    given: Vec<(EthernetAddress, Ipv4Address)>,
}

impl Addresses {
    fn new(gateway_ip: Ipv4Address, netmask: u32) -> Self {
        let gateway = u32::from(gateway_ip);
        Self {
            first: gateway.wrapping_add(1),
            last: (gateway | !netmask).wrapping_sub(1),
            given: vec![],
        }
    }

    /// The address of the adapter at `mac`: the one it was given, else `held`, the one it holds
    /// from before, if that is on the network and free, else the lowest free one. `reserved`
    /// are those that the other adapters hold. None once the network has none left.
    fn of(
        &mut self,
        mac: EthernetAddress,
        held: Option<Ipv4Address>,
        reserved: &[Ipv4Address],
    ) -> Option<Ipv4Address> {
        if let Some(&(_, ip)) = self.given.iter().find(|&&(given, _)| given == mac) {
            return Some(ip);
        }
        let free = |address: u32| {
            let ip = Ipv4Address::from(address);
            !self.given.iter().any(|&(_, given)| given == ip) && !reserved.contains(&ip)
        };
        let address = held
            .map(u32::from)
            .filter(|&address| (self.first..=self.last).contains(&address) && free(address))
            .or_else(|| (self.first..=self.last).find(|&address| free(address)))?;
        let ip = Ipv4Address::from(address);
        self.given.push((mac, ip));
        Some(ip)
    }
}

/// The address of the adapter at `mac`, as `Addresses::of()` gives it: with the address it holds,
/// and keeping out those that the other adapters hold (from `guests`) or use (as `arp_addresses`
/// saw them). The adapter then holds it.
fn address_of(
    addresses: &mut Addresses,
    guests: &GuestTx,
    arp_addresses: &[(EthernetAddress, Ipv4Address)],
    mac: EthernetAddress,
) -> Option<Ipv4Address> {
    let (held, mut reserved) = guests.leases(mac);
    reserved.extend(
        arp_addresses
            .iter()
            .filter(|&&(other, _)| other != mac)
            .map(|&(_, ip)| ip),
    );
    let ip = addresses.of(mac, held, &reserved)?;
    guests.set_lease(mac, ip);
    Some(ip)
}

/// A NAT gateway and its network, which the emulated system's Ethernet adapters attach to.
/// The gateway runs on a thread of its own as long as the network or one of its adapters is
/// there.
pub struct NatNetwork {
    /// The channel of the frames from the adapters, which all share it
    to_gateway: Sender<Packet>,
    /// The adapters
    guests: GuestTx,
    stats: Arc<NatEngineStats>,
    /// The length of an adapter's queue
    queue_size: usize,
}

impl NatNetwork {
    /// Starts a gateway at `gateway_ip`, on a network of `gateway_subnet` bits, with the
    /// Ethernet address `gateway_mac`. DNS queries to it go to `dns`, or with None to the host's
    /// DNS server. Each queue of frames holds `queue_size` frames.
    pub fn start(
        gateway_mac: [u8; 6],
        gateway_ip: [u8; 4],
        gateway_subnet: u8,
        dns: Option<std::net::IpAddr>,
        queue_size: usize,
    ) -> Self {
        let (to_gateway, from_guests) = crossbeam_channel::bounded(queue_size);
        let guests = GuestTx::default();
        let mut engine = NatEngine::new(
            from_guests,
            guests.clone(),
            gateway_mac,
            gateway_ip,
            gateway_subnet,
            dns,
        );
        let stats = engine.stats();
        std::thread::spawn(move || engine.run());
        Self {
            to_gateway,
            guests,
            stats,
            queue_size,
        }
    }

    /// Attaches the adapter with the Ethernet address `mac`, in place of one with the same
    /// address. Returns its channels: of the frames it sends, and of the frames for it, which
    /// are dropped while `rx_enabled` is false. Dropping the second detaches it.
    ///
    /// `lease` holds the address the gateway gives the adapter, 0 for none: it asks for the
    /// address it holds, and gets it again if that is on the network and free.
    pub fn attach(
        &self,
        mac: [u8; 6],
        rx_enabled: Arc<AtomicBool>,
        lease: Arc<AtomicU32>,
        #[cfg(feature = "https_stripping")] https_stripping: bool,
    ) -> (Sender<Packet>, Receiver<Packet>) {
        let (tx, rx) = crossbeam_channel::bounded(self.queue_size);
        self.guests.attach(GuestPort {
            mac: EthernetAddress(mac),
            tx,
            rx_enabled,
            lease,
            #[cfg(feature = "https_stripping")]
            https_stripping,
        });
        (self.to_gateway.clone(), rx)
    }

    /// The statistics of the gateway
    pub fn stats(&self) -> Arc<NatEngineStats> {
        self.stats.clone()
    }
}

impl NatEngine {
    /// Creates a new NAT engine instance
    ///
    /// * `rx` - Receiver channel for packets coming from the emulator's adapters
    /// * `tx` - The adapters, where packets for the emulator go
    /// * `gateway_mac` - MAC address of the NAT gateway
    /// * `gateway_ip` - IP address of the NAT gateway
    /// * `gateway_subnet` - NAT gateway subnet mask (CIDR)
    /// * `dns` - The DNS server that DNS queries to the gateway go to; None: the host's
    fn new(
        rx: Receiver<Packet>,
        tx: GuestTx,
        gateway_mac: [u8; 6],
        gateway_ip: [u8; 4],
        gateway_subnet: u8,
        dns: Option<std::net::IpAddr>,
    ) -> Self {
        let stats = Arc::new(NatEngineStats::default());
        let gateway_mac_addr = EthernetAddress(gateway_mac);
        let gateway_ip_addr =
            IpAddress::v4(gateway_ip[0], gateway_ip[1], gateway_ip[2], gateway_ip[3]);

        let gateway_ipv4 = Ipv4Address::from(gateway_ip);
        let netmask = u32::MAX
            .checked_shl(32 - u32::from(gateway_subnet))
            .unwrap_or(0);
        let subnet_broadcast = Ipv4Address::from(u32::from_be_bytes(gateway_ip) | !netmask);
        let mut device = VirtualDevice::new(tx, rx, stats.clone(), gateway_ipv4, subnet_broadcast);

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

        // Each adapter gets its own address, from the one after the gateway's
        let addresses = Addresses::new(gateway_ipv4, netmask);
        let lease = dhcp::Lease {
            client_ip: Ipv4Address::UNSPECIFIED,
            netmask: Ipv4Address::from(netmask),
            gateway_ip: gateway_ipv4,
            gateway_mac: gateway_mac_addr,
        };
        let dns = DnsServer::new(dns, Instant::now());
        match dns.set.or(dns.host) {
            Some(dns) => log::info!("NAT: DNS queries to the gateway go to {}", dns),
            None => log::warn!(
                "NAT: no DNS server set, and the host has none: DNS queries to the gateway fail"
            ),
        }

        Self {
            device,
            iface,
            sockets,
            gateway_mac: gateway_mac_addr,
            gateway_ip: gateway_ip_addr,
            nat_table: HashMap::new(),
            recv_buffer: vec![0u8; SMOLTCP_BUFFER_SIZE],
            stats,
            #[cfg(feature = "mactcp_helpers")]
            gateway_subnet,
            lease,
            addresses,
            dns,
        }
    }

    /// Obtains a reference to the statistics of this engine instance
    pub fn stats(&self) -> Arc<NatEngineStats> {
        self.stats.clone()
    }

    /// Processes incoming packets from the emulator
    ///
    /// This method should be called in a loop to continuously process packets.
    /// If nothing happened during a pass, it blocks briefly waiting for packets on
    /// the rx channel.
    pub fn process(&mut self) -> Result<()> {
        let rx_packets_before = self.stats.rx_packets.load(Ordering::Relaxed);
        let tx_packets_before = self.stats.tx_packets.load(Ordering::Relaxed);

        // Try to intercept and handle routed TCP/UDP packets BEFORE smoltcp processes
        let mut busy = self.process_intercepted_packets()?;

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
        busy |= self.forward_udp_os_to_smoltcp()?;
        busy |= self.forward_tcp_smoltcp_to_os()?;
        busy |= self.forward_tcp_os_to_smoltcp()?;
        #[cfg(feature = "https_stripping")]
        {
            busy |= self.forward_tls_to_smoltcp()?;
            busy |= self.forward_smoltcp_to_tls()?;
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

        busy |= self.stats.rx_packets.load(Ordering::Relaxed) != rx_packets_before
            || self.stats.tx_packets.load(Ordering::Relaxed) != tx_packets_before;

        if !busy {
            // Nothing moved in this pass: wait for the emulator instead of spinning.
            // With NAT entries present the OS sockets still need to be polled, so keep
            // the wait short.
            let timeout = if self.nat_table.is_empty() {
                IDLE_POLL_INTERVAL
            } else {
                ACTIVE_POLL_INTERVAL
            };
            self.device.wait_for_packet(timeout);
        }

        Ok(())
    }

    /// Handle intercepted packets waiting for processing
    ///
    /// Returns whether any packets were processed.
    fn process_intercepted_packets(&mut self) -> Result<bool> {
        use smoltcp::wire::{EthernetFrame, IpProtocol, Ipv4Packet, TcpPacket, UdpPacket};

        let packets = self.device.drain_intercepted_packets();
        let processed = !packets.is_empty();

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

                    if udp_packet.dst_port() == dhcp::SERVER_PORT {
                        self.handle_dhcp(&packet);
                        continue;
                    }
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

        Ok(processed)
    }

    /// Handle a RARP request by replying with the assigned IP for the client MAC
    #[cfg(feature = "mactcp_helpers")]
    fn handle_rarp(&mut self, eth_frame: &smoltcp::wire::EthernetFrame<&[u8]>) -> Result<()> {
        let IpAddress::Ipv4(gw_ipv4) = self.gateway_ip;

        let addresses = &mut self.addresses;
        let device = &self.device;
        let Some(buf) =
            mactcp_helpers::handle_rarp_request(eth_frame, gw_ipv4, self.gateway_mac, |mac| {
                address_of(addresses, &device.tx, &device.arp_addresses, mac)
            })?
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

    /// Answer a BOOTP or DHCP request, with the address of the adapter that asks
    fn handle_dhcp(&mut self, packet: &[u8]) {
        let Some(mac) = dhcp::client_hardware_address(packet) else {
            return;
        };
        let Some(client_ip) = address_of(
            &mut self.addresses,
            &self.device.tx,
            &self.device.arp_addresses,
            mac,
        ) else {
            log::warn!("DHCP: no address left for {}", mac);
            return;
        };
        let lease = dhcp::Lease {
            client_ip,
            ..self.lease
        };
        let Some(buf) = dhcp::reply(packet, &lease) else {
            return;
        };
        log::info!("DHCP: {} for {}", client_ip, mac);
        let len = buf.len();
        match self.device.tx.try_send(buf) {
            Ok(()) => {
                self.stats.tx_packets.fetch_add(1, Ordering::Relaxed);
                self.stats.tx_bytes.fetch_add(len, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.stats.tx_dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
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

            // A DNS query to the gateway goes to the host's DNS server; the answer comes back
            // from the gateway, since the smoltcp socket is bound to it
            let remote_addr = if dst_ip == self.device.gateway_ip && dst_port == DNS_PORT {
                let Some(dns) = self.dns.get(Instant::now(), host_dns_server) else {
                    return Ok(());
                };
                SocketAddr::new(dns, DNS_PORT)
            } else {
                SocketAddr::new(std::net::IpAddr::V4(dst_ip), dst_port)
            };
            let os_socket = UdpSocket::bind(if remote_addr.is_ipv6() {
                "[::]:0"
            } else {
                "0.0.0.0:0"
            })?;
            os_socket.set_nonblocking(true)?;
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
        let use_https_stripping =
            dst_port == 80 && self.device.tx.strips_https(_eth_frame.src_addr());
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
    ///
    /// Returns whether any data was forwarded.
    fn forward_tcp_smoltcp_to_os(&mut self) -> Result<bool> {
        use smoltcp::socket::tcp;

        let mut forwarded = false;

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
                    Ok(0) => {}
                    Ok(_) => {
                        forwarded = true;
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
                    forwarded = true;
                    Self::discard_received(socket);
                    socket.close();
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                }
            } else if !entry.local_fin_forwarded && Self::local_fin_received(socket) {
                // Emulator closed its side and everything it sent has been passed on:
                // half-close the OS socket so the remote sees the FIN too. The entry keeps
                // the open timeout: the remote may take its time with a response still to
                // come, and its own FIN moves the entry to the closed timeout.
                forwarded = true;
                entry.local_fin_forwarded = true;
                self.stats.nat_tcp_fin_local.fetch_add(1, Ordering::Relaxed);
                if let Err(e) = entry.os_socket.shutdown(std::net::Shutdown::Write) {
                    log::debug!("Error shutting down OS socket for writing: {}", e);
                }
            }
        }

        Ok(forwarded)
    }

    /// Forward data from smoltcp TCP sockets (emulator side) to TLS sockets
    ///
    /// Returns whether any data was forwarded.
    #[cfg(feature = "https_stripping")]
    fn forward_smoltcp_to_tls(&mut self) -> Result<bool> {
        use smoltcp::socket::tcp;

        let mut forwarded = false;

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
                    Ok(0) => {}
                    Ok(_) => forwarded = true,
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
                    forwarded = true;
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
                forwarded = true;
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

        Ok(forwarded)
    }

    /// Forward data from OS UDP sockets back to the emulator (via smoltcp)
    ///
    /// Returns whether any data was forwarded.
    fn forward_udp_os_to_smoltcp(&mut self) -> Result<bool> {
        let mut forwarded = false;
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
                    forwarded = true;

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

        Ok(forwarded)
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
    ///
    /// Returns whether any data was forwarded.
    fn forward_tcp_os_to_smoltcp(&mut self) -> Result<bool> {
        use smoltcp::socket::tcp;

        let mut forwarded = false;

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
                    forwarded = true;
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
                    forwarded = true;
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_OPEN;
                }
                Err(e) => {
                    // The error won't clear on its own, so close the emulator's side instead
                    // of retrying the read on every pass until the entry times out. What is
                    // already queued for the emulator still goes out ahead of the FIN: a
                    // server that closes with part of the request unread resets the
                    // connection right after sending its whole response.
                    log::warn!("Error receiving from OS socket, closing connection: {}", e);
                    forwarded = true;
                    socket.close();
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                }
            }
        }

        Ok(forwarded)
    }

    /// Forward TCP data from TLS sockets (Internet side) to smoltcp sockets (emulator side)
    ///
    /// Returns whether any data was forwarded.
    #[cfg(feature = "https_stripping")]
    fn forward_tls_to_smoltcp(&mut self) -> Result<bool> {
        use smoltcp::socket::tcp;

        let mut forwarded = false;

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
                    forwarded = true;
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
                    forwarded = true;
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_OPEN;
                }
                Err(e) => {
                    // As for plain TCP: close the emulator's side after what is already
                    // queued for it rather than retrying the read on every pass
                    log::error!("Error receiving from TLS socket, closing connection: {}", e);
                    forwarded = true;
                    socket.close();
                    entry.expires_at = Instant::now() + NAT_TIMEOUT_TCP_CLOSED;
                }
            }
        }

        Ok(forwarded)
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

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::wire::{IpProtocol, Ipv4Packet};

    const GATEWAY_MAC: [u8; 6] = [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55];
    const GUEST_MAC: [u8; 6] = [0x08, 0x00, 0x07, 0x12, 0x34, 0x56];
    const GUEST_IP: [u8; 4] = [10, 0, 0, 2];

    /// An adapter at `mac` on `guests`, and its end of the channel of the frames for it
    fn attach(guests: &GuestTx, mac: [u8; 6]) -> Receiver<Packet> {
        let (tx, rx) = crossbeam_channel::unbounded();
        guests.attach(GuestPort {
            mac: EthernetAddress(mac),
            tx,
            rx_enabled: Arc::new(AtomicBool::new(true)),
            lease: Arc::default(),
            #[cfg(feature = "https_stripping")]
            https_stripping: false,
        });
        rx
    }

    /// The device of a gateway at 10.0.0.1/8 with the guest's adapter at GUEST_MAC, and the
    /// guest's ends of its channels
    fn device() -> (VirtualDevice, Sender<Packet>, Receiver<Packet>) {
        let (to_nat, from_guest) = crossbeam_channel::unbounded();
        let tx = GuestTx::default();
        let from_nat = attach(&tx, GUEST_MAC);
        let device = VirtualDevice::new(
            tx,
            from_guest,
            Arc::new(NatEngineStats::default()),
            Ipv4Address::new(10, 0, 0, 1),
            Ipv4Address::new(10, 255, 255, 255),
        );
        (device, to_nat, from_nat)
    }

    /// A frame from the guest to the gateway: IPv4 to `dst`, around this transport header
    /// and payload
    fn ipv4_frame(dst: [u8; 4], protocol: IpProtocol, transport: &[u8]) -> Packet {
        let mut f = vec![0; 14 + 20 + transport.len()];
        let mut eth = EthernetFrame::new_unchecked(&mut f[..]);
        eth.set_dst_addr(EthernetAddress(GATEWAY_MAC));
        eth.set_src_addr(EthernetAddress(GUEST_MAC));
        eth.set_ethertype(EthernetProtocol::Ipv4);
        let mut ip = Ipv4Packet::new_unchecked(eth.payload_mut());
        ip.set_version(4);
        ip.set_header_len(20);
        ip.set_total_len((20 + transport.len()) as u16);
        ip.set_hop_limit(64);
        ip.set_next_header(protocol);
        ip.set_src_addr(Ipv4Address::from(GUEST_IP));
        ip.set_dst_addr(Ipv4Address::from(dst));
        ip.fill_checksum();
        ip.payload_mut().copy_from_slice(transport);
        f
    }

    /// A UDP datagram from the guest's port 40000 to `dst`:`port`, without a checksum
    fn udp_frame(dst: [u8; 4], port: u16) -> Packet {
        let mut udp = vec![0; 8 + 4];
        udp[0..2].copy_from_slice(&40000u16.to_be_bytes());
        udp[2..4].copy_from_slice(&port.to_be_bytes());
        udp[4..6].copy_from_slice(&12u16.to_be_bytes());
        udp[8..].copy_from_slice(b"test");
        ipv4_frame(dst, IpProtocol::Udp, &udp)
    }

    #[test]
    fn broadcasts_stay_on_the_emulated_network() {
        let (device, _to_nat, _from_nat) = device();
        assert!(device.needs_filtering(&udp_frame([255, 255, 255, 255], 7779)));
        assert!(device.needs_filtering(&udp_frame([10, 255, 255, 255], 7779)));
        assert!(!device.needs_filtering(&udp_frame([10, 1, 1, 13], 7779)));
    }

    #[test]
    fn multicast_stays_on_the_emulated_network() {
        let (device, _to_nat, _from_nat) = device();
        assert!(device.needs_filtering(&udp_frame([224, 0, 0, 251], 5353)));
        assert!(device.needs_filtering(&udp_frame([239, 255, 255, 253], 427)));
        assert!(device.needs_filtering(&udp_frame([240, 0, 0, 1], 7779)));
        assert!(!device.needs_filtering(&udp_frame([223, 255, 255, 254], 7779)));
    }

    /// A TCP SYN from the guest's port 40000 to `dst`:`port`, sequence number 1000
    fn tcp_syn_frame(dst: [u8; 4], port: u16) -> Packet {
        use smoltcp::wire::{TcpPacket, TcpSeqNumber};

        let mut tcp = vec![0; 20];
        let mut packet = TcpPacket::new_unchecked(&mut tcp[..]);
        packet.set_src_port(40000);
        packet.set_dst_port(port);
        packet.set_seq_number(TcpSeqNumber(1000));
        packet.set_header_len(20);
        packet.clear_flags();
        packet.set_syn(true);
        packet.set_window_len(8192);
        packet.fill_checksum(
            &IpAddress::Ipv4(Ipv4Address::from(GUEST_IP)),
            &IpAddress::Ipv4(Ipv4Address::from(dst)),
        );
        ipv4_frame(dst, IpProtocol::Tcp, &tcp)
    }

    /// The device's answer to a frame from the guest: whether it went no further, and what came
    /// back to the guest
    fn send(frame: Packet) -> (bool, Vec<Packet>) {
        let (mut device, to_nat, from_nat) = device();
        to_nat.send(frame).unwrap();
        assert!(device.receive(smoltcp::time::Instant::now()).is_none());
        let stopped = device.intercepted_packets.is_empty();
        (stopped, from_nat.try_iter().collect())
    }

    #[test]
    fn the_hosts_own_addresses_are_no_way_out() {
        for dst in [[127, 0, 0, 1], [127, 1, 2, 3], [0, 0, 0, 0]] {
            assert_eq!(send(udp_frame(dst, 7778)), (true, vec![]));
        }
        // An address of the host on its own network is a way out like any other
        assert_eq!(send(udp_frame([10, 1, 1, 13], 7780)), (false, vec![]));
        assert_eq!(send(tcp_syn_frame([10, 1, 1, 13], 7781)), (false, vec![]));
    }

    #[test]
    fn a_connection_to_the_hosts_loopback_is_reset() {
        use smoltcp::wire::{TcpPacket, TcpSeqNumber};

        let (stopped, answers) = send(tcp_syn_frame([127, 0, 0, 1], 7777));
        assert!(stopped);
        assert_eq!(answers.len(), 1);
        let eth = EthernetFrame::new_checked(&answers[0][..]).unwrap();
        assert_eq!(eth.dst_addr(), EthernetAddress(GUEST_MAC));
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert!(ip.verify_checksum());
        assert_eq!(ip.src_addr(), Ipv4Address::new(127, 0, 0, 1));
        assert_eq!(ip.dst_addr(), Ipv4Address::from(GUEST_IP));
        let tcp = TcpPacket::new_checked(ip.payload()).unwrap();
        assert!(tcp.verify_checksum(
            &IpAddress::Ipv4(ip.src_addr()),
            &IpAddress::Ipv4(ip.dst_addr())
        ));
        assert_eq!((tcp.src_port(), tcp.dst_port()), (7777, 40000));
        assert!(tcp.rst() && tcp.ack() && !tcp.syn());
        assert_eq!(tcp.seq_number(), TcpSeqNumber(0));
        // The SYN counts for one
        assert_eq!(tcp.ack_number(), TcpSeqNumber(1001));
    }

    const OTHER_MAC: [u8; 6] = [0x08, 0x00, 0x07, 0xAB, 0xCD, 0xEF];

    /// A frame from `src` to `dst`, of no particular protocol
    fn raw_frame(dst: [u8; 6], src: [u8; 6]) -> Packet {
        let mut f = vec![0; 60];
        let mut eth = EthernetFrame::new_unchecked(&mut f[..]);
        eth.set_dst_addr(EthernetAddress(dst));
        eth.set_src_addr(EthernetAddress(src));
        eth.set_ethertype(EthernetProtocol::Unknown(0x88B5));
        f
    }

    #[test]
    fn a_frame_between_two_adapters_goes_to_the_other_alone() {
        let guests = GuestTx::default();
        let to_guest = attach(&guests, GUEST_MAC);
        let to_other = attach(&guests, OTHER_MAC);

        let frame = raw_frame(OTHER_MAC, GUEST_MAC);
        assert!(guests.forward(&frame));
        assert_eq!(to_other.try_iter().collect::<Vec<_>>(), vec![frame]);
        assert!(to_guest.is_empty());
    }

    #[test]
    fn a_broadcast_goes_to_the_other_adapters_and_on_to_the_gateway() {
        let guests = GuestTx::default();
        let to_guest = attach(&guests, GUEST_MAC);
        let to_other = attach(&guests, OTHER_MAC);

        let frame = raw_frame([0xFF; 6], GUEST_MAC);
        assert!(!guests.forward(&frame));
        assert_eq!(to_other.try_iter().collect::<Vec<_>>(), vec![frame]);
        assert!(to_guest.is_empty());

        // A frame for an address that isn't an adapter's is for the gateway
        assert!(!guests.forward(&raw_frame(GATEWAY_MAC, GUEST_MAC)));
        assert!(to_guest.is_empty() && to_other.is_empty());
    }

    #[test]
    fn a_frame_from_the_gateway_goes_to_its_adapter() {
        let guests = GuestTx::default();
        let to_guest = attach(&guests, GUEST_MAC);
        let to_other = attach(&guests, OTHER_MAC);

        let frame = raw_frame(OTHER_MAC, GATEWAY_MAC);
        assert!(guests.try_send(frame.clone()).is_ok());
        assert_eq!(to_other.try_iter().collect::<Vec<_>>(), vec![frame]);
        assert!(to_guest.is_empty());
        // No adapter there: dropped
        assert!(guests.try_send(raw_frame([0x02; 6], GATEWAY_MAC)).is_err());
        // An adapter that has gone is taken off
        drop(to_other);
        assert!(guests.try_send(raw_frame(OTHER_MAC, GATEWAY_MAC)).is_err());
        assert_eq!(guests.ports.lock().unwrap().len(), 1);
    }

    #[cfg(feature = "https_stripping")]
    #[test]
    fn https_stripping_is_for_each_adapter() {
        let guests = GuestTx::default();
        let _to_guest = attach(&guests, GUEST_MAC);
        let (tx, _to_other) = crossbeam_channel::unbounded();
        guests.attach(GuestPort {
            mac: EthernetAddress(OTHER_MAC),
            tx,
            rx_enabled: Arc::new(AtomicBool::new(true)),
            lease: Arc::default(),
            https_stripping: true,
        });
        assert!(!guests.strips_https(EthernetAddress(GUEST_MAC)));
        assert!(guests.strips_https(EthernetAddress(OTHER_MAC)));
    }

    #[test]
    fn each_adapter_has_an_address_of_its_own() {
        let mut addresses = Addresses::new(Ipv4Address::new(10, 0, 0, 1), 0xFF00_0000);
        let a = EthernetAddress(GUEST_MAC);
        let b = EthernetAddress(OTHER_MAC);
        assert_eq!(
            addresses.of(a, None, &[]),
            Some(Ipv4Address::new(10, 0, 0, 2))
        );
        assert_eq!(
            addresses.of(b, None, &[]),
            Some(Ipv4Address::new(10, 0, 0, 3))
        );
        // Always the same one
        assert_eq!(
            addresses.of(a, None, &[]),
            Some(Ipv4Address::new(10, 0, 0, 2))
        );
    }

    /// An adapter back with the address it held gets it again, even when another asks first;
    /// one held on another network is forgotten
    #[test]
    fn an_adapter_gets_the_address_it_held_back() {
        let mut addresses = Addresses::new(Ipv4Address::new(10, 0, 0, 1), 0xFF00_0000);
        let a = EthernetAddress(GUEST_MAC);
        let b = EthernetAddress(OTHER_MAC);
        let held = Ipv4Address::new(10, 0, 0, 3);
        // b asks first, while a holds .3: b gets .2
        assert_eq!(
            addresses.of(b, None, &[held]),
            Some(Ipv4Address::new(10, 0, 0, 2))
        );
        assert_eq!(addresses.of(a, Some(held), &[]), Some(held));

        let mut addresses = Addresses::new(Ipv4Address::new(192, 168, 1, 1), 0xFFFF_FF00);
        assert_eq!(
            addresses.of(a, Some(held), &[]),
            Some(Ipv4Address::new(192, 168, 1, 2))
        );
    }

    /// The gateway gives an adapter its address through the adapter's lease
    #[test]
    fn the_adapter_holds_its_address() {
        let guests = GuestTx::default();
        let _rx = attach(&guests, GUEST_MAC);
        let mut addresses = Addresses::new(Ipv4Address::new(10, 0, 0, 1), 0xFF00_0000);
        let a = EthernetAddress(GUEST_MAC);
        assert_eq!(guests.leases(a), (None, vec![]));
        let ip = address_of(&mut addresses, &guests, &[], a);
        assert_eq!(ip, Some(Ipv4Address::new(10, 0, 0, 2)));
        assert_eq!(guests.leases(a), (ip, vec![]));
    }

    #[test]
    fn a_full_network_has_no_address_left() {
        // 10.0.0.0/30: the gateway at .1, one address left, .2; .3 is the broadcast address
        let mut addresses = Addresses::new(Ipv4Address::new(10, 0, 0, 1), 0xFFFF_FFFC);
        assert_eq!(
            addresses.of(EthernetAddress(GUEST_MAC), None, &[]),
            Some(Ipv4Address::new(10, 0, 0, 2))
        );
        assert_eq!(addresses.of(EthernetAddress(OTHER_MAC), None, &[]), None);
        assert_eq!(addresses.of(EthernetAddress(OTHER_MAC), None, &[]), None);
        assert_eq!(addresses.given.len(), 1);
    }

    /// The host's DNS server is looked up again for a query once HOST_DNS_LIFETIME has passed,
    /// not sooner; a server set is used without a lookup
    #[test]
    fn the_hosts_dns_server_is_looked_up_again_after_a_while() {
        let start = Instant::now();
        let old = Some(std::net::IpAddr::from([192, 168, 1, 1]));
        let new = Some(std::net::IpAddr::from([10, 1, 1, 1]));
        let mut dns = DnsServer {
            set: None,
            host: old,
            looked_up: start,
        };
        let soon = start + HOST_DNS_LIFETIME / 2;
        assert_eq!(dns.get(soon, || panic!("looked up too soon")), old);
        let later = start + HOST_DNS_LIFETIME;
        assert_eq!(dns.get(later, || new), new);
        assert_eq!(dns.get(later, || panic!("looked up again at once")), new);

        let set = Some(std::net::IpAddr::from([9, 9, 9, 9]));
        let mut dns = DnsServer {
            set,
            host: None,
            looked_up: start,
        };
        assert_eq!(
            dns.get(later, || panic!("looked up with a server set")),
            set
        );
    }

    /// An address that another adapter uses, as the gateway sees it in that adapter's ARP
    /// packets, isn't handed out; one being probed for (from 0.0.0.0) is
    #[test]
    fn an_address_seen_in_arp_isnt_handed_out() {
        let arp = |src: [u8; 4]| {
            let mut buffer = vec![0; 42];
            let mut eth = EthernetFrame::new_unchecked(&mut buffer);
            eth.set_src_addr(EthernetAddress(OTHER_MAC));
            eth.set_dst_addr(EthernetAddress::BROADCAST);
            eth.set_ethertype(EthernetProtocol::Arp);
            ArpRepr::EthernetIpv4 {
                operation: ArpOperation::Request,
                source_hardware_addr: EthernetAddress(OTHER_MAC),
                source_protocol_addr: Ipv4Address::from(src),
                target_hardware_addr: EthernetAddress([0; 6]),
                target_protocol_addr: Ipv4Address::new(10, 0, 0, 1),
            }
            .emit(&mut ArpPacket::new_unchecked(eth.payload_mut()));
            buffer
        };
        let (mut device, to_nat, _from_nat) = device();
        to_nat.send(arp([0, 0, 0, 0])).unwrap();
        let _ = device.receive(smoltcp::time::Instant::now());
        assert!(device.arp_addresses.is_empty());
        // The other adapter set by hand to 10.0.0.2
        to_nat.send(arp([10, 0, 0, 2])).unwrap();
        let _ = device.receive(smoltcp::time::Instant::now());

        let mut addresses = Addresses::new(Ipv4Address::new(10, 0, 0, 1), 0xFF00_0000);
        let guest = EthernetAddress(GUEST_MAC);
        assert_eq!(
            address_of(&mut addresses, &device.tx, &device.arp_addresses, guest),
            Some(Ipv4Address::new(10, 0, 0, 3))
        );
        // The adapter that uses it gets it
        assert_eq!(
            address_of(
                &mut addresses,
                &device.tx,
                &device.arp_addresses,
                EthernetAddress(OTHER_MAC)
            ),
            Some(Ipv4Address::new(10, 0, 0, 2))
        );
    }
}
