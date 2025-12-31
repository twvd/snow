//! Daynaport SCSI Ethernet adapter

use crate::debuggable::Debuggable;
use crate::mac::scsi::target::{ScsiTarget, ScsiTargetEvent, ScsiTargetType};
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::STATUS_CHECK_CONDITION;
use crate::mac::scsi::STATUS_GOOD;
use crate::util::{deserialize_arc_rwlock, serialize_arc_rwlock};

use anyhow::{bail, Result};
#[cfg(any(
    feature = "ethernet_raw",
    all(feature = "ethernet_tap", target_os = "linux")
))]
use crossbeam_channel::TrySendError;
use rand::Rng;
use serde::{Deserialize, Serialize};
#[cfg(feature = "ethernet_nat")]
use snow_nat::NatEngineStats;

use std::path::Path;
#[cfg(any(
    feature = "ethernet_nat",
    all(feature = "ethernet_tap", target_os = "linux")
))]
use std::sync::atomic::Ordering;
#[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, RwLock};

type BasicPacket = Vec<u8>;

/// Maximum amount of packets to buffer in the RX/TX queues
const PACKET_QUEUE_SIZE: usize = 512;

/// Link mode for ethernet device
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum EthernetLinkType {
    /// No link
    Down,
    /// Userland NAT
    #[cfg(feature = "ethernet_nat")]
    NAT,
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

/// Statistics for tap bridge
#[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
#[derive(Default)]
struct BridgeStats {
    rx_total: AtomicUsize,
    rx_total_bytes: AtomicUsize,
    rx_unicast: AtomicUsize,
    rx_multicast: AtomicUsize,
    rx_broadcast: AtomicUsize,
    rx_dropped_filtered: AtomicUsize,
    rx_dropped_too_large: AtomicUsize,
    rx_dropped_invalid: AtomicUsize,
    rx_dropped_full: AtomicUsize,

    tx_total: AtomicUsize,
    tx_total_bytes: AtomicUsize,
}

/// DaynaPORT SCSI/Link Ethernet adapter
#[derive(Serialize, Deserialize)]
pub(crate) struct ScsiTargetEthernet {
    /// Check condition code
    cc_code: u8,

    /// Check condition ASC
    cc_asc: u16,

    /// MAC address
    macaddress: [u8; 6],

    /// Transmit queue (Mac -> network)
    #[serde(skip)]
    tx: Option<crossbeam_channel::Sender<BasicPacket>>,

    /// Receive queue (network -> Mac)
    #[serde(skip)]
    rx: Option<crossbeam_channel::Receiver<BasicPacket>>,

    /// Link type
    #[serde(skip)]
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

    /// Interface enabled
    enabled: bool,

    /// Subscribed layer 2 multicast groups
    #[serde(
        serialize_with = "serialize_arc_rwlock",
        deserialize_with = "deserialize_arc_rwlock"
    )]
    multicast_groups: Arc<RwLock<Vec<[u8; 6]>>>,
}

impl Default for ScsiTargetEthernet {
    fn default() -> Self {
        let mut rand = rand::rng();

        Self {
            cc_code: 0,
            cc_asc: 0,
            macaddress: [
                0x00,
                0x80,
                0x19,
                rand.random(),
                rand.random(),
                rand.random(),
            ],
            tx: None,
            rx: None,
            link: Default::default(),
            #[cfg(feature = "ethernet_nat")]
            nat_stats: None,
            enabled: false,
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            tap_stop: Arc::new(AtomicBool::new(false)),
            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
            tap_stats: None,
            multicast_groups: Default::default(),
        }
    }
}

impl ScsiTargetEthernet {
    fn tx_packet(&mut self, packet: &[u8]) {
        if self.rx.is_none() && self.tx.is_none() && self.link != EthernetLinkType::Down {
            // Initialize link
            let link = self.link.clone();
            if let Err(e) = self.eth_set_link(link) {
                log::error!("Failed to set ethernet link: {}", e);
            }
        }

        if let Some(ref tx) = self.tx {
            match tx.try_send(packet.to_owned()) {
                Ok(_) => {}
                Err(e) => log::error!("Failed to send packet {:?}", e),
            }
        }
    }

    #[cfg(feature = "ethernet_raw")]
    fn start_bridge(&mut self, ifidx: u32) -> Result<()> {
        // This all doesn't work very well:
        //  - without MAC rewrite enabled, it doesn't work on some (all?) adapters
        //  - on modern adapters with TSO, GSO, etc, the Mac receives way too large packets
        //  - with MAC rewrite enabled, the Mac will see all of the hosts packets
        //    (better not be streaming 4K while running the emulator!)
        // Code kept for posterity

        // Enable to rewrite MAC-addresses of the emulated system to the hosts MAC-address
        const REWRITE_MACS: bool = false;

        let Some(interface) = pnet::datalink::interfaces()
            .into_iter()
            .find(|i| i.index == ifidx)
        else {
            bail!("Cannot find interface index {}", ifidx)
        };

        let (bridge_tx, emulator_rx) = crossbeam_channel::bounded(PACKET_QUEUE_SIZE);
        let (emulator_tx, bridge_rx) = crossbeam_channel::bounded(PACKET_QUEUE_SIZE);
        self.tx = Some(emulator_tx);
        self.rx = Some(emulator_rx);

        let bridge_config = pnet::datalink::Config {
            promiscuous: true,
            ..Default::default()
        };

        match pnet::datalink::channel(&interface, bridge_config) {
            Ok(pnet::datalink::Channel::Ethernet(mut physical_tx, mut physical_rx)) => {
                log::info!(
                    "Starting ethernet bridge for interface '{}'",
                    interface.name
                );

                // Physical RX -> Virtual RX
                let t_emu_mac = self.macaddress;
                let t_physical_mac: pnet::datalink::MacAddr = if REWRITE_MACS {
                    interface.mac.unwrap()
                } else {
                    t_emu_mac.into()
                };
                std::thread::spawn(move || loop {
                    match physical_rx.next() {
                        Ok(packet) => {
                            // Squelch echos and filter on packets destined for us
                            let mut packet = packet.to_vec();
                            let Some(mut ethpacket) =
                                pnet::packet::ethernet::MutableEthernetPacket::new(&mut packet)
                            else {
                                log::warn!("Dropped RX invalid packet: {:02X?}", packet);
                                continue;
                            };
                            let dest = ethpacket.get_destination();
                            let src = ethpacket.get_source();
                            if (dest != t_physical_mac && !dest.is_broadcast())
                                || src == t_physical_mac
                            {
                                continue;
                            }

                            // Rewrite MAC-addresses
                            if REWRITE_MACS && dest == t_physical_mac {
                                ethpacket.set_destination(t_emu_mac.into());
                            }

                            //log::debug!("Physical RX: {:?} {:02X?}", &ethpacket, &packet);

                            match bridge_tx.try_send(packet.to_vec()) {
                                Ok(_) => {}
                                Err(TrySendError::Disconnected(_)) => {
                                    log::info!("Bridge terminated (bridge_tx closed)");
                                    return;
                                }
                                Err(TrySendError::Full(_)) => {
                                    log::error!("bridge_tx queue overflow");
                                }
                            }
                        }
                        Err(e) => {
                            log::info!("Bridge terminated (physical_rx closed: {})", e);
                            return;
                        }
                    }
                });
                // Virtual TX -> Physical TX
                std::thread::spawn(move || loop {
                    use pnet::packet::{MutablePacket, Packet};

                    match bridge_rx.recv() {
                        Ok(mut packet) => {
                            let Some(mut ethpacket) =
                                pnet::packet::ethernet::MutableEthernetPacket::new(&mut packet)
                            else {
                                log::warn!("Dropped TX invalid packet: {:02X?}", packet);
                                continue;
                            };
                            if REWRITE_MACS && ethpacket.get_source() == t_emu_mac {
                                ethpacket.set_source(t_physical_mac);
                            }
                            if REWRITE_MACS
                                && ethpacket.get_ethertype()
                                    == pnet::packet::ethernet::EtherTypes::Arp
                            {
                                if let Some(mut arppacket) =
                                    pnet::packet::arp::MutableArpPacket::new(
                                        ethpacket.payload_mut(),
                                    )
                                {
                                    arppacket.set_sender_hw_addr(t_physical_mac);
                                }
                            }

                            // Minimum frame size
                            //if out_packet.len() < 64 {
                            //    out_packet.resize(64, 0);
                            //}
                            //log::debug!("Physical TX: {:02X?}", &ethpacket);

                            match physical_tx.send_to(ethpacket.packet(), None).unwrap() {
                                Ok(_) => {}
                                Err(e) => {
                                    log::info!("Bridge terminated (physical_tx closed: {})", e);
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            log::info!("Bridge terminated (bridge_rx closed: {})", e);
                            return;
                        }
                    }
                });
            }
            Ok(_) => {
                bail!("Failed opening bridge channel");
            }
            Err(e) => {
                bail!("Failed opening bridge channel: {:?}", e);
            }
        }

        Ok(())
    }

    #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
    fn start_tap_bridge(&mut self, tap_name: &str) -> Result<()> {
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;

        // This creates a new bool, rather than flipping a possibly still used one back to false.
        self.tap_stop = Arc::new(AtomicBool::new(false));

        let tap_stats = Arc::new(BridgeStats::default());
        self.tap_stats = Some(tap_stats.clone());

        log::info!("Opening TAP device '{}'", tap_name);

        // Open the existing TAP device
        let mut config = tun::Configuration::default();
        config.layer(tun::Layer::L2).tun_name(tap_name);

        config.platform_config(|config| {
            config.ensure_root_privileges(true);
        });

        let dev = match tun::create(&config) {
            Ok(dev) => dev,
            Err(e) => {
                log::error!("Failed to open TAP device '{}': {}", tap_name, e);
                return Ok(());
            }
        };

        log::info!("TAP device '{}' opened successfully", tap_name);

        let (bridge_tx, emulator_rx) = crossbeam_channel::bounded(PACKET_QUEUE_SIZE);
        let (emulator_tx, bridge_rx) = crossbeam_channel::bounded(PACKET_QUEUE_SIZE);
        self.tx = Some(emulator_tx);
        self.rx = Some(emulator_rx);

        let (mut reader, mut writer) = dev.split();

        let rx_tap_stop = self.tap_stop.clone();
        let tx_tap_stop = self.tap_stop.clone();
        let tx_stats = tap_stats.clone();
        let rx_stats = tap_stats;
        let t_mac = self.macaddress;
        let t_multicast_groups = Arc::clone(&self.multicast_groups);

        // TAP RX -> Emulator RX (Physical -> Virtual)
        std::thread::spawn(move || {
            use nix::errno::Errno;
            use nix::poll;
            use std::os::fd::BorrowedFd;

            // reader must remain alive as long as fd
            let fd = unsafe { BorrowedFd::borrow_raw(reader.as_raw_fd()) };

            let mut buffer = vec![0u8; 65536];

            loop {
                let mut pfd = [poll::PollFd::new(fd, poll::PollFlags::POLLIN)];
                match poll::poll(&mut pfd, 100_u16) {
                    Ok(1) => match reader.read(&mut buffer) {
                        Ok(0) => {
                            log::info!("TAP bridge terminated (TAP device closed)");
                            return;
                        }
                        Ok(size) => {
                            rx_stats.rx_total.fetch_add(1, Ordering::Relaxed);
                            rx_stats.rx_total_bytes.fetch_add(size, Ordering::Relaxed);

                            if size > 1514 {
                                log::warn!("Dropped RX too large packet ({})", size);
                                rx_stats
                                    .rx_dropped_too_large
                                    .fetch_add(1, Ordering::Relaxed);
                                continue;
                            }

                            let Some(ethpacket) =
                                pnet::packet::ethernet::EthernetPacket::new(&buffer)
                            else {
                                log::warn!("Dropped RX invalid packet: {:02X?}", &buffer);
                                rx_stats.rx_dropped_invalid.fetch_add(1, Ordering::Relaxed);
                                continue;
                            };

                            let dest = ethpacket.get_destination();
                            let src = ethpacket.get_source();
                            let multicast_sub =
                                t_multicast_groups.read().unwrap().contains(&dest.octets());
                            if (dest != t_mac && !dest.is_broadcast() && !multicast_sub)
                                || src == t_mac
                            {
                                // Not for us or echo (filtered)
                                rx_stats.rx_dropped_filtered.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }

                            // Packet accepted
                            if multicast_sub {
                                rx_stats.rx_multicast.fetch_add(1, Ordering::Relaxed);
                            } else if dest.is_broadcast() {
                                rx_stats.rx_broadcast.fetch_add(1, Ordering::Relaxed);
                            } else {
                                rx_stats.rx_unicast.fetch_add(1, Ordering::Relaxed);
                            }

                            // Send to emulator
                            match bridge_tx.try_send(buffer[..size].to_vec()) {
                                Ok(_) => {}
                                Err(TrySendError::Disconnected(_)) => {
                                    log::info!("TAP bridge terminated (bridge_tx closed)");
                                    return;
                                }
                                Err(TrySendError::Full(_)) => {
                                    rx_stats.rx_dropped_full.fetch_add(1, Ordering::Relaxed);
                                    log::error!("bridge_tx queue overflow");
                                }
                            }
                        }
                        Err(e) => {
                            log::info!("TAP bridge terminated (read error: {})", e);
                            return;
                        }
                    },
                    Ok(_) | Err(Errno::EAGAIN) | Err(Errno::EINTR) => {}
                    Err(e) => {
                        log::error!("TAP bridge poll() error: {:?}", e);
                        return;
                    }
                }

                if rx_tap_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    log::info!("TAP bridge terminated (tap_stop)");
                    return;
                }
            }
        });

        // Emulator TX -> TAP TX (Virtual -> Physical)
        std::thread::spawn(move || {
            use crossbeam_channel::RecvTimeoutError;

            loop {
                match bridge_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(packet) => {
                        tx_stats.tx_total.fetch_add(1, Ordering::Relaxed);
                        tx_stats
                            .tx_total_bytes
                            .fetch_add(packet.len(), Ordering::Relaxed);
                        match writer.write_all(&packet) {
                            Ok(_) => {}
                            Err(e) => {
                                log::info!("TAP bridge terminated (write error: {})", e);
                                return;
                            }
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => (),
                    Err(e) => {
                        log::info!("TAP bridge terminated (bridge_rx closed: {})", e);
                        return;
                    }
                }

                if tx_tap_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    log::info!("TAP bridge terminated (tap_stop)");
                }
            }
        });

        Ok(())
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetEthernet {
    #[cfg(feature = "savestates")]
    fn after_deserialize(&mut self, _imgfn: &Path) -> Result<()> {
        todo!()
    }

    fn set_blocksize(&mut self, _blocksize: usize) -> bool {
        false
    }

    fn take_event(&mut self) -> Option<ScsiTargetEvent> {
        None
    }

    fn target_type(&self) -> ScsiTargetType {
        ScsiTargetType::Ethernet
    }

    fn unit_ready(&mut self) -> Result<ScsiCmdResult> {
        Ok(ScsiCmdResult::Status(STATUS_GOOD))
    }

    fn inquiry(&mut self, cmd: &[u8]) -> Result<ScsiCmdResult> {
        let mut result = vec![0; 36];

        // 0 Peripheral qualifier (5-7), peripheral device type (4-0)
        result[0] = 3; // Processor
        result[1] = 0;

        // SCSI version compliance
        result[2] = 0x01;
        result[3] = 0x02;

        // 4 Additional length (N-4), min. 32
        result[4] = 31; //result.len() as u8 - 4;
        result[7] = 0x18;

        // 8..16 Vendor identification
        result[8..16].copy_from_slice(b"Dayna   ");

        // 16..32 Product identification
        result[16..32].copy_from_slice(b"SCSI/Link       ");

        // 32..36 Revision
        result[32..36].copy_from_slice(b"2.0f");

        result.resize(cmd[4].min(36).into(), 0);
        Ok(ScsiCmdResult::DataIn(result))
    }

    fn mode_sense(&mut self, page: u8) -> Option<Vec<u8>> {
        log::debug!("Mode sense: {:02X}", page);
        None
    }

    fn ms_density(&self) -> u8 {
        0
    }

    fn ms_media_type(&self) -> u8 {
        0
    }

    fn ms_device_specific(&self) -> u8 {
        0
    }

    fn set_cc(&mut self, code: u8, asc: u16) {
        self.cc_code = code;
        self.cc_asc = asc;
    }

    fn req_sense(&mut self) -> (u8, u16) {
        (self.cc_code, self.cc_asc)
    }

    fn blocksize(&self) -> Option<usize> {
        None
    }

    fn blocks(&self) -> Option<usize> {
        None
    }

    fn read(&self, _block_offset: usize, _block_count: usize) -> Vec<u8> {
        unreachable!()
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {
        unreachable!()
    }

    fn image_fn(&self) -> Option<&Path> {
        None
    }

    fn load_media(&mut self, _path: &Path) -> Result<()> {
        unreachable!()
    }

    fn branch_media(&mut self, _path: &Path) -> Result<()> {
        unreachable!()
    }

    fn media(&self) -> Option<&[u8]> {
        None
    }

    fn specific_cmd(&mut self, cmd: &[u8], outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        match cmd[0] {
            0x08 => {
                // READ(6)
                const FCS: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

                let read_len = ((cmd[3] as usize) << 8) | (cmd[4] as usize);
                if read_len == 1 {
                    // ROM trying to address adapter as disk at boot
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                let Some(rx) = self.rx.as_ref() else {
                    // Link down
                    return Ok(ScsiCmdResult::DataIn(vec![0; 6]));
                };

                if rx.is_empty() {
                    // No data
                    return Ok(ScsiCmdResult::DataIn(vec![0; 6]));
                }

                let mut response = vec![];
                loop {
                    let packet = rx.try_recv()?;
                    let more = !rx.is_empty();
                    let packet_len = packet.len().max(64);
                    let frame_len = packet_len + 4; // FCS
                    let resp_len = 6 + frame_len;
                    if read_len < resp_len {
                        log::error!(
                            "RX packet too large (is {}, have {}): {:02X?}",
                            resp_len,
                            read_len,
                            &packet
                        );
                        return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                    }

                    let checksum = FCS.checksum(&packet).to_be_bytes();
                    response.push((frame_len >> 8) as u8);
                    response.push(frame_len as u8);
                    response.push(0);
                    response.push(0);
                    response.push(0);
                    response.push(if more { 0x10 } else { 0 });
                    response.extend(packet);
                    response.extend(checksum);

                    if !more {
                        break;
                    }
                }
                Ok(ScsiCmdResult::DataIn(response))
            }
            0x09 => {
                // Stats
                let mut result = vec![0; 18];
                result[0..6].copy_from_slice(&self.macaddress);
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x0A => {
                // WRITE(6)
                if let Some(od) = outdata {
                    if cmd[5] & 0x80 != 0 {
                        let len = ((od[0] as usize) << 8) | (od[1] as usize);
                        if od.len() < (len + 4) {
                            bail!("Invalid write len {} <> {}", len, od.len());
                        }
                        self.tx_packet(&od[4..(len + 4)]);
                    } else {
                        self.tx_packet(od);
                    }
                    //log::debug!("write finished {:02X?}", od);
                    Ok(ScsiCmdResult::Status(STATUS_GOOD))
                } else {
                    let mut write_len = ((cmd[3] as usize) << 8) | (cmd[4] as usize);
                    if cmd[5] & 0x80 != 0 {
                        write_len += 8;
                    }
                    //log::debug!("write start {} {}", write_len, cmd[5] & 0x80 != 0);
                    Ok(ScsiCmdResult::DataOut(write_len))
                }
            }
            0x0D => {
                // Multicast subscribe
                if let Some(od) = outdata {
                    let mac: [u8; 6] = od[0..6].try_into().unwrap();

                    log::info!(
                        "Subscribed to multicast group {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                        mac[0],
                        mac[1],
                        mac[2],
                        mac[3],
                        mac[4],
                        mac[5]
                    );

                    let mut groups = self.multicast_groups.write().unwrap();
                    if !groups.contains(&mac) {
                        groups.push(mac);

                        // clippy is eager..
                        drop(groups);
                    }

                    Ok(ScsiCmdResult::Status(STATUS_GOOD))
                } else {
                    Ok(ScsiCmdResult::DataOut(6))
                }
            }
            0x0E => {
                // Enable/disable interface
                let enable = cmd[5] & 0x80 != 0;
                log::debug!("Interface enable: {}", enable);

                if !self.enabled && enable {
                    // Drain RX queue
                    if let Some(rx) = &self.rx {
                        while rx.try_recv().is_ok() {}
                    }
                }

                self.enabled = enable;

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            _ => Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION)),
        }
    }

    fn eth_set_link(&mut self, link: EthernetLinkType) -> Result<()> {
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
                    nat_rx,
                    [0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA],
                    [10, 0, 0, 1],
                    8,
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

    fn eth_link(&self) -> Option<EthernetLinkType> {
        Some(self.link.clone())
    }
}

impl Debuggable for ScsiTargetEthernet {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_string, dbgprop_udec};
        #[cfg(feature = "ethernet_nat")]
        use crate::{dbgprop_group, dbgprop_str};

        let mut result = vec![
            dbgprop_string!(
                "MAC address",
                format!(
                    "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                    self.macaddress[0],
                    self.macaddress[1],
                    self.macaddress[2],
                    self.macaddress[3],
                    self.macaddress[4],
                    self.macaddress[5],
                )
            ),
            dbgprop_bool!("Interface enabled", self.enabled),
        ];

        if let Some(tx) = &self.tx {
            result.push(dbgprop_udec!("TX queue length", tx.len()));
        }
        if let Some(rx) = &self.rx {
            result.push(dbgprop_udec!("RX queue length", rx.len()));
        }
        result.push(dbgprop_group!(
            "Multicast groups",
            Vec::from_iter(self.multicast_groups.read().unwrap().iter().map(
                |mac| dbgprop_string!(
                    "Member of",
                    format!(
                        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                    )
                )
            ))
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
                        "RX accepted: unicast",
                        stats.rx_unicast.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "RX accepted: multicast",
                        stats.rx_multicast.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "RX accepted: broadcast",
                        stats.rx_broadcast.load(Ordering::Relaxed)
                    ),
                    dbgprop_udec!(
                        "RX dropped: filtered",
                        stats.rx_dropped_filtered.load(Ordering::Relaxed)
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
}
