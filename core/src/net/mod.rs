//! Host side of the emulated Ethernet adapters
//!
//! An [`EthernetPort`] connects an adapter model to the host network
//! (NAT, TAP, raw bridge) and carries the packet capture.

mod pcap;
#[cfg(feature = "ethernet_raw")]
mod raw;
#[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
mod tap;

use crate::debuggable::DebuggableProperties;
use crate::devices::{
    EthernetConfig, EthernetFunction, EthernetLinkType, EthernetStatus, NatConfig,
};

use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
#[cfg(feature = "ethernet_nat")]
use snow_nat::NatEngineStats;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use pcap::PcapCaptureState;
#[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
use tap::BridgeStats;

type BasicPacket = Vec<u8>;

/// Counters of the receive filter
#[derive(Default)]
struct RxFilterStats {
    unicast: usize,
    multicast: usize,
    broadcast: usize,
    filtered: usize,
}

/// Maximum amount of packets to buffer in the RX/TX queues
#[cfg(any(
    test,
    feature = "ethernet_nat",
    feature = "ethernet_raw",
    all(feature = "ethernet_tap", target_os = "linux")
))]
const PACKET_QUEUE_SIZE: usize = 512;

/// Locally administered MAC address of the NAT gateway visible from the emulated Mac.
#[cfg(feature = "ethernet_nat")]
const NAT_MAC_ADDRESS: [u8; 6] = [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55];

/// Connection of an emulated Ethernet adapter to the host network
#[derive(Serialize, Deserialize)]
pub struct EthernetPort {
    /// MAC address
    mac: [u8; 6],

    /// Transmit queue (Mac -> network)
    #[serde(skip)]
    tx: Option<crossbeam_channel::Sender<BasicPacket>>,

    /// Receive queue (network -> Mac)
    #[serde(skip)]
    rx: Option<crossbeam_channel::Receiver<BasicPacket>>,

    /// Whether the adapter's receiver is on. The host side drops the frames for the guest while
    /// it is off. It is in a save state, so that a restored adapter needn't set it again.
    #[serde(
        serialize_with = "serialize_flag",
        deserialize_with = "deserialize_flag"
    )]
    rx_enabled: Arc<AtomicBool>,

    /// The address its NAT gateway gave it, 0 for none: the gateway writes it. It is in a save
    /// state, so that a restored adapter gets the same address back.
    #[serde(
        serialize_with = "serialize_lease",
        deserialize_with = "deserialize_lease"
    )]
    lease: Arc<AtomicU32>,

    /// Link type
    #[serde(skip)]
    link: EthernetLinkType,

    /// The NAT gateway that a NAT link attaches to
    #[serde(skip)]
    nat: SharedNat,

    /// NAT engine statistics
    #[cfg(feature = "ethernet_nat")]
    #[serde(skip)]
    nat_stats: Option<Arc<NatEngineStats>>,

    /// Tap bridge statistics
    #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
    #[serde(skip)]
    tap_stats: Option<Arc<BridgeStats>>,

    /// TAP stop signal
    #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
    #[serde(skip)]
    tap_stop: Arc<AtomicBool>,

    /// The receive and transmit threads of the TAP bridge. They keep the TAP device open until
    /// they exit.
    #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
    #[serde(skip)]
    tap_threads: Vec<std::thread::JoinHandle<()>>,

    /// Subscribed layer 2 multicast groups
    multicast_groups: Vec<[u8; 6]>,

    /// Pass all frames, for adapters that filter by themselves
    promiscuous: bool,

    /// Receive filter statistics
    #[serde(skip)]
    rx_stats: RxFilterStats,

    /// Pcap capture state
    #[serde(skip)]
    pcap_capture: Option<PcapCaptureState>,

    /// Pcap packet sender channel
    #[serde(skip)]
    pcap_tx: Option<crossbeam_channel::Sender<(std::time::SystemTime, Vec<u8>)>>,
}

/// The NAT gateway of a machine
///
/// Its Ethernet adapters on a NAT link all attach to it, so they are on one network; it starts
/// with the first of them. A port has one of its own until the machine gives it the machine's
/// (`EthernetFunction::set_shared_nat()`).
#[derive(Clone, Default)]
pub struct SharedNat {
    #[cfg(feature = "ethernet_nat")]
    state: Arc<std::sync::Mutex<NatState>>,
}

/// The network of a gateway, and the gateway once it runs
#[cfg(feature = "ethernet_nat")]
#[derive(Default)]
struct NatState {
    config: NatConfig,
    network: Option<snow_nat::NatNetwork>,
}

impl SharedNat {
    /// A gateway on this network
    pub fn new(config: NatConfig) -> Self {
        let nat = Self::default();
        nat.configure(config);
        nat
    }

    /// Gives the gateway another network. The gateway that runs goes on until its adapters
    /// attach again (`EthernetFunction::set_shared_nat()`), to a gateway on this network.
    pub fn configure(&self, config: NatConfig) {
        #[cfg(feature = "ethernet_nat")]
        {
            let mut state = self.state.lock().unwrap();
            state.config = config;
            state.network = None;
        }
        #[cfg(not(feature = "ethernet_nat"))]
        let _ = config;
    }
}

#[cfg(feature = "ethernet_nat")]
impl SharedNat {
    /// Attaches an adapter, as `NatNetwork::attach()` does, starting the gateway if it isn't
    /// yet. Also returns the gateway's statistics.
    fn attach(
        &self,
        mac: [u8; 6],
        rx_enabled: Arc<AtomicBool>,
        lease: Arc<AtomicU32>,
        https_stripping: bool,
    ) -> (
        crossbeam_channel::Sender<BasicPacket>,
        crossbeam_channel::Receiver<BasicPacket>,
        Arc<NatEngineStats>,
    ) {
        #[cfg(not(feature = "ethernet_nat_https_stripping"))]
        let _ = https_stripping;
        let mut state = self.state.lock().unwrap();
        let config = state.config;
        let nat = state.network.get_or_insert_with(|| {
            snow_nat::NatNetwork::start(
                NAT_MAC_ADDRESS,
                config.gateway,
                config.prefix_len,
                config.dns,
                PACKET_QUEUE_SIZE,
            )
        });
        let (tx, rx) = nat.attach(
            mac,
            rx_enabled,
            lease,
            #[cfg(feature = "ethernet_nat_https_stripping")]
            https_stripping,
        );
        let stats = nat.stats();
        drop(state);
        (tx, rx, stats)
    }
}

impl EthernetPort {
    /// A port with the Ethernet address `mac`, and its link down
    pub fn new(mac: [u8; 6]) -> Self {
        Self {
            mac,
            tx: None,
            rx: None,
            rx_enabled: Arc::new(AtomicBool::new(true)),
            lease: Arc::default(),
            link: EthernetLinkType::Down,
            nat: SharedNat::default(),
            #[cfg(feature = "ethernet_nat")]
            nat_stats: None,
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            tap_stop: Arc::new(AtomicBool::new(false)),
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            tap_threads: Vec::new(),
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            tap_stats: None,
            multicast_groups: Default::default(),
            promiscuous: false,
            rx_stats: Default::default(),
            pcap_capture: None,
            pcap_tx: None,
        }
    }

    /// MAC address
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Sends a frame to the network
    pub fn send(&mut self, packet: &[u8]) {
        if self.rx.is_none() && self.tx.is_none() && self.link != EthernetLinkType::Down {
            // Initialize link
            let link = self.link.clone();
            if let Err(e) = self.set_link(link) {
                log::error!("Failed to set ethernet link: {}", e);
            }
        }

        // Capture packet before sending
        self.capture_packet(packet);

        if let Some(ref tx) = self.tx {
            match tx.try_send(packet.to_owned()) {
                Ok(_) => {}
                Err(e) => log::error!("Failed to send packet {:?}", e),
            }
        }
    }

    /// Whether the link is up (receive queue present)
    pub fn connected(&self) -> bool {
        self.rx.is_some()
    }

    /// Whether a received frame is waiting
    pub fn rx_pending(&self) -> bool {
        self.rx.as_ref().is_some_and(|rx| !rx.is_empty())
    }

    /// Takes the next received frame that passes the receive filter, if any
    pub fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            let packet = self.rx.as_ref()?.try_recv().ok()?;
            if self.accept(&packet) {
                // Capture received packet
                self.capture_packet(&packet);
                return Some(packet);
            }
        }
    }

    /// Receive filter: frames for our address, broadcasts and subscribed
    /// multicast groups, never our own frames echoed back
    fn accept(&mut self, packet: &[u8]) -> bool {
        if packet.len() < 14 {
            self.rx_stats.filtered += 1;
            return false;
        }
        let dest: [u8; 6] = packet[0..6].try_into().unwrap();
        let src: [u8; 6] = packet[6..12].try_into().unwrap();
        if src == self.mac {
            self.rx_stats.filtered += 1;
            false
        } else if dest == self.mac || self.promiscuous {
            self.rx_stats.unicast += 1;
            true
        } else if dest == [0xFF; 6] {
            self.rx_stats.broadcast += 1;
            true
        } else if self.multicast_groups.contains(&dest) {
            self.rx_stats.multicast += 1;
            true
        } else {
            self.rx_stats.filtered += 1;
            false
        }
    }

    /// Turns the adapter's receiver on or off. While it is off, the host
    /// side drops the frames for the guest as they arrive, and the ones
    /// already queued are dropped.
    pub fn set_rx_enabled(&mut self, enabled: bool) {
        self.rx_enabled.store(enabled, Ordering::Relaxed);
        if !enabled {
            self.flush_rx();
        }
    }

    pub fn rx_enabled(&self) -> bool {
        self.rx_enabled.load(Ordering::Relaxed)
    }

    /// Drops all received frames
    pub fn flush_rx(&mut self) {
        if let Some(rx) = &self.rx {
            while rx.try_recv().is_ok() {}
        }
    }

    /// Passes all received frames to the adapter, which filters them
    pub fn set_promiscuous(&mut self, promiscuous: bool) {
        self.promiscuous = promiscuous;
    }

    /// Subscribes to a layer 2 multicast group
    pub fn add_multicast(&mut self, mac: [u8; 6]) {
        if !self.multicast_groups.contains(&mac) {
            self.multicast_groups.push(mac);
        }
    }

    pub fn link(&self) -> EthernetLinkType {
        self.link.clone()
    }

    /// The Config of the adapter of this port, as it is now: the address it was made with, or
    /// the random one it got
    pub fn config(&self) -> EthernetConfig {
        EthernetConfig {
            mac: Some(self.mac),
            link: self.link(),
        }
    }

    pub fn set_link(&mut self, link: EthernetLinkType) -> Result<()> {
        // Terminate active links
        // Dropping the senders will cause the channel to close and the receiver threads to terminate
        self.rx = None;
        self.tx = None;
        #[cfg(feature = "ethernet_nat")]
        {
            self.nat_stats = None;
        }
        #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
        self.stop_tap_bridge();
        // Stays down if the new link fails to start, so that send() does not retry it on every
        // frame
        self.link = EthernetLinkType::Down;

        let started = match &link {
            EthernetLinkType::Down => {
                log::info!("Ethernet link down");
                Ok(())
            }
            #[cfg(feature = "ethernet_raw")]
            EthernetLinkType::Bridge(i) => {
                log::info!("Ethernet link bridge to interface {}", i);
                self.start_bridge(*i)
            }
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            EthernetLinkType::TapBridge(name) => {
                log::info!("Ethernet link TAP bridge: {}", name);
                self.start_tap_bridge(name)
            }
            #[cfg(feature = "ethernet_nat")]
            EthernetLinkType::NAT => {
                self.start_nat(false);
                Ok(())
            }
            #[cfg(feature = "ethernet_nat_https_stripping")]
            EthernetLinkType::NATHttpsStripping => {
                self.start_nat(true);
                Ok(())
            }
            // A link this build has no support for
            #[allow(unreachable_patterns)]
            _ => Err(anyhow::anyhow!("This build has no support for this link")),
        };
        if let Err(e) = started {
            // A link that fails to start may have set its channels already, and frames sent to
            // them would go nowhere
            self.rx = None;
            self.tx = None;
            return Err(e);
        }
        self.link = link;
        Ok(())
    }

    /// Attaches the port to its NAT gateway
    #[cfg(feature = "ethernet_nat")]
    fn start_nat(&mut self, https_stripping: bool) {
        let (tx, rx, stats) = self.nat.attach(
            self.mac,
            self.rx_enabled.clone(),
            self.lease.clone(),
            https_stripping,
        );
        self.tx = Some(tx);
        self.rx = Some(rx);
        self.nat_stats = Some(stats);
    }

    /// Takes the machine's NAT gateway: a NAT link in use moves to it
    pub fn set_shared_nat(&mut self, nat: SharedNat) {
        self.nat = nat;
        #[cfg(feature = "ethernet_nat")]
        if self.nat_stats.is_some() {
            let link = self.link.clone();
            if let Err(e) = self.set_link(link) {
                log::error!("Failed to set ethernet link: {}", e);
            }
        }
    }

    pub fn debug_properties(&self) -> DebuggableProperties {
        #[cfg(any(
            feature = "ethernet_nat",
            all(feature = "ethernet_tap", target_os = "linux")
        ))]
        use crate::dbgprop_str;
        use crate::debuggable::*;
        use crate::{dbgprop_group, dbgprop_string, dbgprop_udec};

        let mut result = vec![];

        if let Some(tx) = &self.tx {
            result.push(dbgprop_udec!("TX queue length", tx.len()));
        }
        if let Some(rx) = &self.rx {
            result.push(dbgprop_udec!("RX queue length", rx.len()));
        }
        result.push(dbgprop_group!(
            "RX filter",
            vec![
                dbgprop_udec!("Accepted: unicast", self.rx_stats.unicast),
                dbgprop_udec!("Accepted: multicast", self.rx_stats.multicast),
                dbgprop_udec!("Accepted: broadcast", self.rx_stats.broadcast),
                dbgprop_udec!("Dropped", self.rx_stats.filtered),
            ]
        ));
        result.push(dbgprop_group!(
            "Multicast groups",
            Vec::from_iter(self.multicast_groups.iter().map(|mac| dbgprop_string!(
                "Member of",
                format!(
                    "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                )
            )))
        ));
        #[cfg(feature = "ethernet_nat")]
        if let Some(stats) = &self.nat_stats {
            result.push(dbgprop_group!(
                "NAT engine",
                vec![
                    dbgprop_udec!(
                        "Emu -> remote packets",
                        stats.rx_packets.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Emu -> remote bytes",
                        stats.rx_bytes.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Remote -> emu packets",
                        stats.tx_packets.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Remote -> emu bytes",
                        stats.tx_bytes.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Filtered packets",
                        stats.filtered_packets.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Filtered bytes",
                        stats.filtered_bytes.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Active TCP connections",
                        stats.nat_active_tcp.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Active UDP connections",
                        stats.nat_active_udp.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Total TCP connections",
                        stats.nat_total_tcp.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Total UDP connections",
                        stats.nat_total_udp.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "Expired connections",
                        stats.nat_expired.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!("TCP SYNs seen", stats.nat_tcp_syn.load(Ordering::Relaxed)),
                    dbgprop_udec!(
                        "TCP FINs from emulator seen",
                        stats.nat_tcp_fin_local.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "TCP FINs from remote seen",
                        stats.nat_tcp_fin_remote.load(Ordering::Relaxed)
                    ),
                ]
            ));
        } else {
            result.push(dbgprop_str!("NAT engine", "Inactive"));
        }

        #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
        if let Some(stats) = &self.tap_stats {
            result.push(dbgprop_group!(
                "Tap bridge",
                vec![
                    dbgprop_udec!("RX total (packets)", stats.rx_total.load(Ordering::Relaxed)),
                    dbgprop_udec!(
                        "RX total (bytes)",
                        stats.rx_total_bytes.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "RX dropped: too large",
                        stats.rx_dropped_too_large.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "RX dropped: invalid frame",
                        stats.rx_dropped_invalid.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "RX dropped: queue full",
                        stats.rx_dropped_full.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!("TX total (packets)", stats.tx_total.load(Ordering::Relaxed)),
                    dbgprop_udec!(
                        "TX total (bytes)",
                        stats.tx_total_bytes.load(Ordering::Relaxed)
                    ),
                ]
            ));
        } else {
            result.push(dbgprop_str!("Tap bridge", "Inactive"));
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn connect_test_rx(&mut self) -> crossbeam_channel::Sender<BasicPacket> {
        let (tx, rx) = crossbeam_channel::bounded(PACKET_QUEUE_SIZE);
        self.rx = Some(rx);
        tx
    }

    #[cfg(test)]
    pub(crate) fn rx_len(&self) -> usize {
        self.rx.as_ref().map_or(0, |rx| rx.len())
    }
}

fn serialize_flag<S: Serializer>(flag: &Arc<AtomicBool>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bool(flag.load(Ordering::Relaxed))
}

fn deserialize_flag<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Arc<AtomicBool>, D::Error> {
    Ok(Arc::new(AtomicBool::new(bool::deserialize(deserializer)?)))
}

fn serialize_lease<S: Serializer>(
    lease: &Arc<AtomicU32>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_u32(lease.load(Ordering::Relaxed))
}

fn deserialize_lease<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Arc<AtomicU32>, D::Error> {
    Ok(Arc::new(AtomicU32::new(u32::deserialize(deserializer)?)))
}

/// The Ethernet function of every adapter model is its port: the frontend works on the port
impl EthernetFunction for EthernetPort {
    fn status(&self) -> EthernetStatus {
        EthernetStatus {
            mac: self.mac,
            link: self.link(),
            capture: self.capture_status(),
        }
    }

    fn set_link(&mut self, link: EthernetLinkType) -> Result<()> {
        Self::set_link(self, link)
    }

    fn set_shared_nat(&mut self, nat: SharedNat) {
        Self::set_shared_nat(self, nat);
    }

    fn start_capture(&mut self, filename: &Path) -> Result<()> {
        Self::start_capture(self, filename)
    }

    fn stop_capture(&mut self) -> Option<(PathBuf, usize)> {
        Self::stop_capture(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two ports on a NAT link with one gateway are on one network: a frame from one to the
    /// other gets there
    #[cfg(feature = "ethernet_nat")]
    #[test]
    fn ports_with_one_nat_gateway_reach_each_other() {
        let nat = SharedNat::default();
        let mut a = EthernetPort::new([0x08, 0x00, 0x07, 1, 2, 3]);
        let mut b = EthernetPort::new([0x08, 0x00, 0x07, 4, 5, 6]);
        a.set_shared_nat(nat.clone());
        b.set_shared_nat(nat);
        a.set_link(EthernetLinkType::NAT).unwrap();
        b.set_link(EthernetLinkType::NAT).unwrap();

        let mut frame = vec![0; 60];
        frame[0..6].copy_from_slice(&b.mac());
        frame[6..12].copy_from_slice(&a.mac());
        frame[12..14].copy_from_slice(&[0x88, 0xB5]);
        a.send(&frame);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let received = loop {
            if let Some(received) = b.recv() {
                break received;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the frame didn't arrive"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert_eq!(received, frame);
    }

    /// A DHCP DISCOVER from `mac`, as a client without an address sends it
    #[cfg(feature = "ethernet_nat")]
    fn dhcp_discover(mac: [u8; 6]) -> Vec<u8> {
        let mut frame = vec![0; 14 + 20 + 8 + 300];
        frame[0..6].copy_from_slice(&[0xFF; 6]);
        frame[6..12].copy_from_slice(&mac);
        frame[12..14].copy_from_slice(&[0x08, 0x00]);
        let ip = &mut frame[14..34];
        ip[0] = 0x45;
        ip[2..4].copy_from_slice(&(20u16 + 8 + 300).to_be_bytes());
        ip[8] = 64;
        ip[9] = 17;
        ip[16..20].copy_from_slice(&[255; 4]);
        let udp = &mut frame[34..42];
        udp[0..2].copy_from_slice(&68u16.to_be_bytes());
        udp[2..4].copy_from_slice(&67u16.to_be_bytes());
        udp[4..6].copy_from_slice(&308u16.to_be_bytes());
        let bootp = &mut frame[42..];
        bootp[0..4].copy_from_slice(&[1, 1, 6, 0]);
        bootp[4..8].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        bootp[28..34].copy_from_slice(&mac);
        // RFC 2132: the magic cookie, a DHCP message type of DISCOVER, the end
        bootp[236..243].copy_from_slice(&[99, 130, 83, 99, 53, 1, 1]);
        bootp[243] = 255;
        frame
    }

    /// The address a port gets from its gateway's DHCP server
    #[cfg(feature = "ethernet_nat")]
    fn dhcp_address(port: &mut EthernetPort) -> [u8; 4] {
        port.send(&dhcp_discover(port.mac()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            // The others' requests come too, as broadcasts: a reply is a BOOTREPLY (op 2)
            if let Some(reply) = port.recv()
                && reply[14 + 20 + 8] == 2
            {
                // yiaddr, in the BOOTP packet after the Ethernet, IP and UDP headers
                return reply[14 + 20 + 8 + 16..14 + 20 + 8 + 20]
                    .try_into()
                    .unwrap();
            }
            assert!(std::time::Instant::now() < deadline, "no DHCP reply");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Ports restored from a save state get the addresses they had back from a new gateway,
    /// whichever asks first
    #[cfg(feature = "ethernet_nat")]
    #[test]
    fn ports_keep_their_addresses_across_a_save() {
        let nat = SharedNat::default();
        let mut a = EthernetPort::new([0x08, 0x00, 0x07, 1, 2, 3]);
        let mut b = EthernetPort::new([0x08, 0x00, 0x07, 4, 5, 6]);
        a.set_shared_nat(nat.clone());
        b.set_shared_nat(nat);
        a.set_link(EthernetLinkType::NAT).unwrap();
        b.set_link(EthernetLinkType::NAT).unwrap();
        assert_eq!(dhcp_address(&mut a), [10, 0, 0, 2]);
        assert_eq!(dhcp_address(&mut b), [10, 0, 0, 3]);

        let saved_a = serde_json::to_string(&a).unwrap();
        let saved_b = serde_json::to_string(&b).unwrap();
        let mut a: EthernetPort = serde_json::from_str(&saved_a).unwrap();
        let mut b: EthernetPort = serde_json::from_str(&saved_b).unwrap();
        let nat = SharedNat::default();
        b.set_shared_nat(nat.clone());
        a.set_shared_nat(nat);
        b.set_link(EthernetLinkType::NAT).unwrap();
        a.set_link(EthernetLinkType::NAT).unwrap();
        assert_eq!(dhcp_address(&mut b), [10, 0, 0, 3]);
        assert_eq!(dhcp_address(&mut a), [10, 0, 0, 2]);
    }

    /// A new network for the gateway: the ports on a NAT link move to it when they take the
    /// gateway again
    #[cfg(feature = "ethernet_nat")]
    #[test]
    fn ports_move_to_a_new_nat_network() {
        let nat = SharedNat::new(NatConfig::default());
        let mut a = EthernetPort::new([0x08, 0x00, 0x07, 1, 2, 3]);
        let mut b = EthernetPort::new([0x08, 0x00, 0x07, 4, 5, 6]);
        a.set_shared_nat(nat.clone());
        b.set_shared_nat(nat.clone());
        a.set_link(EthernetLinkType::NAT).unwrap();
        b.set_link(EthernetLinkType::NAT).unwrap();
        assert_eq!(dhcp_address(&mut a), [10, 0, 0, 2]);
        assert_eq!(dhcp_address(&mut b), [10, 0, 0, 3]);

        nat.configure(NatConfig {
            gateway: [192, 168, 77, 1],
            prefix_len: 24,
            dns: None,
        });
        a.set_shared_nat(nat.clone());
        b.set_shared_nat(nat);
        assert_eq!(dhcp_address(&mut b), [192, 168, 77, 2]);
        assert_eq!(dhcp_address(&mut a), [192, 168, 77, 3]);
    }

    #[test]
    fn a_nat_network_must_have_room_for_adapters() {
        assert!(NatConfig::default().check().is_ok());
        let config = |gateway, prefix_len| NatConfig {
            gateway,
            prefix_len,
            dns: None,
        };
        assert!(config([192, 168, 1, 254], 24).check().is_ok());
        assert!(config([192, 168, 1, 0], 24).check().is_err());
        assert!(config([192, 168, 1, 255], 24).check().is_err());
        assert!(config([10, 0, 0, 1], 7).check().is_err());
        assert!(config([10, 0, 0, 1], 31).check().is_err());
        assert_eq!(config([10, 0, 0, 1], 24).netmask(), [255, 255, 255, 0]);
    }

    #[test]
    fn the_receiver_stays_off_across_a_save() {
        let mut port = EthernetPort::new([0, 0x80, 0x19, 1, 2, 3]);
        assert!(port.rx_enabled());
        port.set_rx_enabled(false);
        let saved = serde_json::to_string(&port).unwrap();
        let port: EthernetPort = serde_json::from_str(&saved).unwrap();
        assert!(!port.rx_enabled());
    }
}
