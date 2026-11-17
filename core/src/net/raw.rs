//! Raw socket bridge

use super::{EthernetPort, PACKET_QUEUE_SIZE};

use anyhow::Result;
use crossbeam_channel::TrySendError;

impl EthernetPort {
    pub(super) fn start_bridge(&mut self, ifidx: u32) -> Result<()> {
        // This all doesn't work very well:
        //  - without MAC rewrite enabled, it doesn't work on some (all?) adapters
        //  - on modern adapters with TSO, GSO, etc, the Mac receives way too large packets
        //  - with MAC rewrite enabled, the Mac will see all of the hosts packets
        //    (better not be streaming 4K while running the emulator!)
        // Code kept for posterity

        use anyhow::bail;

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
                let t_emu_mac = self.mac;
                let rx_enabled = self.rx_enabled.clone();
                let t_physical_mac: pnet::datalink::MacAddr = if REWRITE_MACS {
                    interface.mac.unwrap()
                } else {
                    t_emu_mac.into()
                };
                std::thread::spawn(move || {
                    loop {
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

                                if !rx_enabled.load(std::sync::atomic::Ordering::Relaxed) {
                                    continue;
                                }

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
                    }
                });
                // Virtual TX -> Physical TX
                std::thread::spawn(move || {
                    loop {
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
}
