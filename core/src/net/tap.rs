//! TAP interface bridge (Linux)

use super::{EthernetPort, PACKET_QUEUE_SIZE};

use anyhow::{Result, bail};
use crossbeam_channel::TrySendError;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Statistics for tap bridge
#[derive(Default)]
pub(super) struct BridgeStats {
    pub(super) rx_total: AtomicUsize,
    pub(super) rx_total_bytes: AtomicUsize,
    pub(super) rx_unicast: AtomicUsize,
    pub(super) rx_multicast: AtomicUsize,
    pub(super) rx_broadcast: AtomicUsize,
    pub(super) rx_dropped_filtered: AtomicUsize,
    pub(super) rx_dropped_too_large: AtomicUsize,
    pub(super) rx_dropped_invalid: AtomicUsize,
    pub(super) rx_dropped_full: AtomicUsize,

    pub(super) tx_total: AtomicUsize,
    pub(super) tx_total_bytes: AtomicUsize,
}

impl EthernetPort {
    pub(super) fn start_tap_bridge(&mut self, tap_name: &str) -> Result<()> {
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
            Err(e) => bail!("Failed to open TAP device '{}': {}", tap_name, e),
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
        let t_mac = self.mac;
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
                    return;
                }
            }
        });

        Ok(())
    }
}
