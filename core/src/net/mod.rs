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
use crate::emulator::comm::EthernetCaptureStatus;
use crate::emulator::devices::{Ethernet, EthernetStatus, replace_status};

use anyhow::Result;
use serde::{Deserialize, Serialize};
#[cfg(feature = "ethernet_nat")]
use snow_nat::NatEngineStats;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
const PACKET_QUEUE_SIZE: usize = 512;

/// Locally administered MAC address of the NAT gateway visible from the emulated Mac.
#[cfg(feature = "ethernet_nat")]
const NAT_MAC_ADDRESS: [u8; 6] = [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55];

/// Link mode for ethernet device
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum EthernetLinkType {
    /// No link
    Down,
    /// Userland NAT
    #[cfg(feature = "ethernet_nat")]
    NAT,
    /// Userland NAT with HTTPS stripping
    #[cfg(feature = "ethernet_nat_https_stripping")]
    NATHttpsStripping,
    /// Raw sockets based bridge
    #[cfg(feature = "ethernet_raw")]
    Bridge(u32),
    /// Tap interface based bridge
    #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
    TapBridge(String),
}

// Clippy is wrong, I don't think you can conditionally tag #[default] on an enum based on features
#[allow(clippy::derivable_impls)]
impl Default for EthernetLinkType {
    fn default() -> Self {
        #[cfg(feature = "ethernet_nat")]
        {
            Self::NAT
        }
        #[cfg(not(feature = "ethernet_nat"))]
        {
            Self::Down
        }
    }
}

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

    /// Whether the adapter's receiver is on. The host side drops the
    /// frames for the guest while it is off.
    #[serde(skip, default = "rx_enabled_default")]
    rx_enabled: Arc<AtomicBool>,

    /// The link type, kept in the state so that a restored adapter opens the
    /// link again
    link: EthernetLinkType,

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

impl EthernetPort {
    pub fn new(mac: [u8; 6]) -> Self {
        Self {
            mac,
            tx: None,
            rx: None,
            rx_enabled: rx_enabled_default(),
            link: Default::default(),
            #[cfg(feature = "ethernet_nat")]
            nat_stats: None,
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            tap_stop: Arc::new(AtomicBool::new(false)),
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

    /// Changes the MAC address, restarting a running link so the host
    /// side filters on the new one
    pub fn set_mac(&mut self, mac: [u8; 6]) -> Result<()> {
        if mac == self.mac {
            return Ok(());
        }
        self.mac = mac;
        if self.connected() {
            self.set_link(self.link())?;
        }
        Ok(())
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
        {
            self.tap_stop
                .store(true, std::sync::atomic::Ordering::Release);
        }
        // Stays down if the new link fails to start, so that send() does
        // not retry it on every frame
        self.link = EthernetLinkType::Down;

        match &link {
            EthernetLinkType::Down => {
                log::info!("Ethernet link down");
            }
            #[cfg(feature = "ethernet_raw")]
            EthernetLinkType::Bridge(i) => {
                log::info!("Ethernet link bridge to interface {}", i);
                self.start_bridge(*i)?;
            }
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            EthernetLinkType::TapBridge(name) => {
                log::info!("Ethernet link TAP bridge: {}", name);
                self.start_tap_bridge(name)?;
            }
            #[cfg(feature = "ethernet_nat")]
            EthernetLinkType::NAT => {
                let (nat_tx, emulator_rx) = crossbeam_channel::bounded(PACKET_QUEUE_SIZE);
                let (emulator_tx, nat_rx) = crossbeam_channel::bounded(PACKET_QUEUE_SIZE);

                let mut nat = snow_nat::NatEngine::new(
                    nat_tx,
                    self.rx_enabled.clone(),
                    nat_rx,
                    NAT_MAC_ADDRESS,
                    [10, 0, 0, 1],
                    8,
                    #[cfg(feature = "ethernet_nat_https_stripping")]
                    false,
                );
                self.rx = Some(emulator_rx);
                self.tx = Some(emulator_tx);
                self.nat_stats = Some(nat.stats());
                std::thread::spawn(move || {
                    nat.run();
                });
            }
            #[cfg(feature = "ethernet_nat_https_stripping")]
            EthernetLinkType::NATHttpsStripping => {
                let (nat_tx, emulator_rx) = crossbeam_channel::bounded(PACKET_QUEUE_SIZE);
                let (emulator_tx, nat_rx) = crossbeam_channel::bounded(PACKET_QUEUE_SIZE);

                let mut nat = snow_nat::NatEngine::new(
                    nat_tx,
                    self.rx_enabled.clone(),
                    nat_rx,
                    NAT_MAC_ADDRESS,
                    [10, 0, 0, 1],
                    8,
                    true, // Enable HTTPS stripping
                );
                self.rx = Some(emulator_rx);
                self.tx = Some(emulator_tx);
                self.nat_stats = Some(nat.stats());
                std::thread::spawn(move || {
                    nat.run();
                });
            }
        }
        self.link = link;
        Ok(())
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

fn rx_enabled_default() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(true))
}

/// The Ethernet function of every adapter model is its port: the frontend works on the port
impl Ethernet for EthernetPort {
    fn update_status(&mut self, status: &mut EthernetStatus) -> bool {
        let new = EthernetStatus {
            mac: self.mac,
            link: Self::link(self),
            capture: Self::capture_status(self),
        };
        replace_status(status, new)
    }

    fn link(&self) -> EthernetLinkType {
        Self::link(self)
    }

    fn set_link(&mut self, link: EthernetLinkType) -> Result<()> {
        Self::set_link(self, link)
    }

    fn set_mac(&mut self, mac: [u8; 6]) -> Result<()> {
        Self::set_mac(self, mac)
    }

    fn start_capture(&mut self, filename: &Path) -> Result<()> {
        Self::start_capture(self, filename)
    }

    fn stop_capture(&mut self) -> Option<(PathBuf, usize)> {
        Self::stop_capture(self)
    }

    fn capture_status(&self) -> Option<EthernetCaptureStatus> {
        Self::capture_status(self)
    }
}
