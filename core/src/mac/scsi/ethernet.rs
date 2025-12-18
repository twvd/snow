//! Daynaport SCSI Ethernet adapter

use crate::mac::scsi::target::{ScsiTarget, ScsiTargetEvent, ScsiTargetType};
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::STATUS_CHECK_CONDITION;
use crate::mac::scsi::STATUS_GOOD;

use anyhow::{bail, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};

use std::path::Path;

type BasicPacket = Vec<u8>;

#[derive(Default, Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub enum EthernetLinkType {
    #[default]
    Down,
    #[cfg(feature = "ethernet_nat")]
    NAT,
    #[cfg(feature = "ethernet_raw")]
    Bridge(u32),
}

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
        }
    }
}

impl ScsiTargetEthernet {
    fn tx_packet(&self, packet: &[u8]) {
        if let Some(ref tx) = self.tx {
            match tx.send(packet.to_owned()) {
                Ok(_) => {}
                Err(e) => log::error!("Failed to send packet {:?}", e),
            }
        }
    }

    #[cfg(feature = "ethernet_raw")]
    fn start_bridge(&mut self, ifidx: u32) -> Result<()> {
        let Some(interface) = pnet::datalink::interfaces()
            .into_iter()
            .find(|i| i.index == ifidx)
        else {
            bail!("Cannot find interface index {}", ifidx)
        };

        let (bridge_tx, emulator_rx) = crossbeam_channel::unbounded();
        let (emulator_tx, bridge_rx) = crossbeam_channel::unbounded();
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
                let t_mac = self.macaddress;
                std::thread::spawn(move || loop {
                    match physical_rx.next() {
                        Ok(packet) => {
                            // Squelch echos and filter on packets destined for us
                            let Some(ethpacket) =
                                pnet::packet::ethernet::EthernetPacket::new(packet)
                            else {
                                log::warn!("Dropped invalid packet: {:02X?}", packet);
                                continue;
                            };
                            let dest = ethpacket.get_destination();
                            let src = ethpacket.get_source();
                            if (dest != t_mac && !dest.is_broadcast()) || src == t_mac {
                                continue;
                            }
                            log::debug!("Physical RX: {:?} {:02X?}", &ethpacket, &packet);

                            match bridge_tx.send(packet.to_vec()) {
                                Ok(_) => {}
                                Err(e) => {
                                    log::info!("Bridge terminated (bridge_tx closed: {})", e);
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            log::info!("Bridge terminated (physical_rx closed: {})", e);
                        }
                    }
                });
                // Virtual TX -> Physical TX
                std::thread::spawn(move || loop {
                    match bridge_rx.recv() {
                        Ok(packet) => {
                            log::debug!("Physical TX: {:02X?}", packet);

                            match physical_tx.send_to(&packet, None).unwrap() {
                                Ok(_) => {}
                                Err(e) => {
                                    log::info!("Bridge terminated (physical_tx closed: {})", e);
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            log::info!("Bridge terminated (bridge_rx closed: {})", e);
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
                let read_len = ((cmd[3] as usize) << 8) | (cmd[4] as usize);

                if let Some(packet) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
                    let packet_len = packet.len().max(64);
                    let resp_len = 6 + packet_len;
                    if read_len < resp_len {
                        log::error!(
                            "RX packet too large (is {}, have {}): {:02X?}",
                            resp_len,
                            read_len,
                            &packet
                        );
                        return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                    }

                    let mut response = vec![0; resp_len];
                    response[6..(6 + packet.len())].copy_from_slice(&packet);

                    // Length
                    response[0] = (packet_len >> 8) as u8;
                    response[1] = packet_len as u8;
                    if !self.rx.as_ref().unwrap().is_empty() {
                        // More packets available to read
                        // TODO this causes SCSI Manager issues
                        //response[5] = 0x10;
                    }
                    Ok(ScsiCmdResult::DataIn(response))
                } else {
                    // No data
                    Ok(ScsiCmdResult::DataIn(vec![0; 6]))
                }
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
                    Ok(ScsiCmdResult::Status(STATUS_GOOD))
                } else {
                    let mut write_len = ((cmd[3] as usize) << 8) | (cmd[4] as usize);
                    if cmd[5] & 0x80 != 0 {
                        write_len += 8;
                    }
                    Ok(ScsiCmdResult::DataOut(write_len))
                }
            }
            0x0E => {
                // Enable/disable interface
                let enable = cmd[5] & 0x80 != 0;
                log::debug!("Interface enable: {}", enable);
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

        self.link = link;
        match link {
            EthernetLinkType::Down => {
                log::info!("Ethernet link down");
            }
            #[cfg(feature = "ethernet_raw")]
            EthernetLinkType::Bridge(i) => {
                log::info!("Ethernet link bridge to interface {}", i);
                self.start_bridge(i)?;
            }
            #[cfg(feature = "ethernet_nat")]
            EthernetLinkType::NAT => {
                let (nat_tx, emulator_rx) = crossbeam_channel::unbounded();
                let (emulator_tx, nat_rx) = crossbeam_channel::unbounded();

                let mut nat = snow_nat::NatEngine::new(
                    nat_tx,
                    nat_rx,
                    [0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA],
                    [10, 0, 0, 1],
                    24,
                );
                self.rx = Some(emulator_rx);
                self.tx = Some(emulator_tx);
                std::thread::spawn(move || {
                    nat.run();
                });
            }
        }
        Ok(())
    }

    fn eth_link(&self) -> Option<EthernetLinkType> {
        Some(self.link)
    }
}
