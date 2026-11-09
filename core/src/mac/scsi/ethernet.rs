//! Daynaport SCSI Ethernet adapter

use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::emulator::devices::{CreateContext, DeviceConfig, FunctionMut, configure_ethernet};
use crate::mac::scsi::STATUS_CHECK_CONDITION;
use crate::mac::scsi::STATUS_GOOD;
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::target::{ScsiTarget, ScsiTargetCommon, ScsiTargetEvent, ScsiTargetType};
use crate::net::EthernetPort;

use anyhow::Result;
use rand::Rng;
use serde::{Deserialize, Serialize};

#[cfg(feature = "savestates")]
use std::path::Path;

/// DaynaPORT SCSI/Link Ethernet adapter
#[derive(Serialize, Deserialize)]
pub(crate) struct ScsiTargetEthernet {
    common: ScsiTargetCommon,

    /// Host network connection
    port: EthernetPort,

    /// Interface enabled
    enabled: bool,
}

impl Default for ScsiTargetEthernet {
    fn default() -> Self {
        let mut rand = rand::rng();

        Self {
            common: Default::default(),
            port: EthernetPort::new([
                0x00,
                0x80,
                0x19,
                rand.random(),
                rand.random(),
                rand.random(),
            ]),
            enabled: false,
        }
    }
}

#[typetag::serde]
impl ScsiTarget for ScsiTargetEthernet {
    fn common(&mut self) -> &mut ScsiTargetCommon {
        &mut self.common
    }

    /// The host side of the link isn't in the state: it opens again
    #[cfg(feature = "savestates")]
    fn after_restore(&mut self, _media: Option<&Path>) -> Result<()> {
        self.port.set_link(self.port.link())
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
        let mut result = vec![0; 37];

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

        // The SCSI/Link returns up to 37 bytes of Inquiry data (DaynaPORT
        // SCSI/Link command set, Inquiry), more than the additional length
        // in byte 4 describes. The DaynaPORT Mac driver asks for exactly 37
        // bytes and skips the device if it gets fewer.
        result.resize(cmd[4].min(37).into(), 0);
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

    fn blocksize(&self) -> Option<usize> {
        None
    }

    fn blocks(&self) -> Option<usize> {
        None
    }

    fn read(&mut self, _block_offset: usize, _block_count: usize) -> Result<ScsiCmdResult> {
        unreachable!()
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {
        unreachable!()
    }

    fn function(&mut self) -> FunctionMut<'_> {
        FunctionMut::Ethernet(&mut self.port)
    }

    fn specific_cmd(&mut self, cmd: &[u8], outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        match cmd[0] {
            0x08 => {
                // READ(6)
                const FCS: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
                const MAX_PACKETS_PER_READ: usize = 8;

                let read_len = ((cmd[3] as usize) << 8) | (cmd[4] as usize);
                if read_len == 1 {
                    // ROM trying to address adapter as disk at boot
                    return Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION));
                }

                if !self.port.rx_pending() {
                    // Link down or no data
                    return Ok(ScsiCmdResult::DataIn(vec![0; 6]));
                }

                // A frame that can never fit the allocation length is dropped
                // on its own, the others are kept for this or the next READ
                let mut packets = vec![];
                while packets.len() < MAX_PACKETS_PER_READ
                    && let Some(packet) = self.port.recv()
                {
                    let resp_len = 6 + packet.len().max(64) + 4;
                    if read_len < resp_len {
                        log::error!(
                            "RX packet too large (is {}, have {}): {:02X?}",
                            resp_len,
                            read_len,
                            &packet
                        );
                        continue;
                    }
                    packets.push(packet);
                }
                if packets.is_empty() {
                    return Ok(ScsiCmdResult::DataIn(vec![0; 6]));
                }

                let mut response = vec![];
                let count = packets.len();
                for (i, packet) in packets.into_iter().enumerate() {
                    let more = i + 1 < count;
                    let packet_len = packet.len().max(64);
                    let frame_len = packet_len + 4; // FCS

                    let checksum = FCS.checksum(&packet).to_be_bytes();
                    response.push((frame_len >> 8) as u8);
                    response.push(frame_len as u8);
                    response.push(0);
                    response.push(0);
                    response.push(0);
                    response.push(if more { 0x10 } else { 0 });
                    response.extend(&packet);
                    // Pad to minimum packet size if needed
                    response.resize(response.len() + (packet_len - packet.len()), 0);
                    response.extend(checksum);
                }
                Ok(ScsiCmdResult::DataIn(response))
            }
            0x09 => {
                // Stats, no more than the allocation length in bytes 3 and 4
                // (SCSI-2 7.2.6)
                let alloc_len = ((cmd[3] as usize) << 8) | (cmd[4] as usize);
                let mut result = vec![0; 18];
                result[0..6].copy_from_slice(&self.port.mac());
                result.truncate(alloc_len);
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x0A => {
                // WRITE(6)
                if let Some(od) = outdata {
                    if cmd[5] & 0x80 != 0 {
                        // Multiple packet mode
                        let next_len = if od.len() == 4 {
                            // First DataOut, read packet next
                            ((od[0] as usize) << 8) | (od[1] as usize)
                        } else {
                            // Packet + 4 byte next header
                            let packet_len = od.len() - 4;
                            self.port.send(&od[0..packet_len]);

                            ((od[packet_len] as usize) << 8) | (od[packet_len + 1] as usize)
                        };

                        if next_len == 0 {
                            Ok(ScsiCmdResult::Status(STATUS_GOOD))
                        } else {
                            // Next: packet + next header
                            Ok(ScsiCmdResult::DataOut(next_len + 4))
                        }
                    } else {
                        self.port.send(od);
                        Ok(ScsiCmdResult::Status(STATUS_GOOD))
                    }
                } else {
                    // After command phase..

                    if cmd[5] & 0x80 != 0 {
                        // Multiple packet mode has a 4 byte header
                        // Read the first header, and from then on start reading
                        Ok(ScsiCmdResult::DataOut(4))
                    } else {
                        // Single packet mode
                        let write_len = ((cmd[3] as usize) << 8) | (cmd[4] as usize);
                        Ok(ScsiCmdResult::DataOut(write_len))
                    }
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

                    self.port.add_multicast(mac);

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
                    self.port.flush_rx();
                }

                self.enabled = enable;

                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            _ => Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION)),
        }
    }
}

impl Debuggable for ScsiTargetEthernet {
    fn get_debug_properties(&self) -> DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_string};

        let mac = self.port.mac();
        let mut result = vec![
            dbgprop_string!(
                "MAC address",
                format!(
                    "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                )
            ),
            dbgprop_bool!("Interface enabled", self.enabled),
        ];
        result.extend(self.port.debug_properties());
        result
    }
}

/// Makes a DaynaPORT from its entry
fn create(config: &DeviceConfig, _ctx: &mut CreateContext) -> Result<Box<dyn ScsiTarget>> {
    let mut adapter = ScsiTargetEthernet::default();
    configure_ethernet(&mut adapter.port, config)?;
    Ok(Box::new(adapter))
}

pub const INFO: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::DaynaPortScsiLink,
    name: "DaynaPORT SCSI/Link",
    bus: crate::emulator::devices::BusKind::Scsi,
    fits: crate::emulator::devices::ModelInfo::fits_all,
    function: crate::emulator::devices::FunctionKind::Ethernet,
    fixed: false,
    one_per_bus: true,
    rom: None,
    path_keys: &[],
    adds_buses: &[],
    monitors: &[],
    create: Some(crate::emulator::devices::CreateFn::Scsi(create)),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_max_8_packets() {
        // Create a ScsiTargetEthernet instance
        let mut eth = ScsiTargetEthernet::default();

        // Create channels for testing
        let tx = eth.port.connect_test_rx();

        // Send 10 test packets to the RX queue
        for i in 0..10 {
            let packet = vec![
                0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // dest MAC (broadcast)
                0x00, 0x80, 0x19, 0x01, 0x02, 0x03, // src MAC
                0x08, 0x00, // EtherType (IP)
                i,
            ]; // payload with packet number
            tx.send(packet).unwrap();
        }

        // Create READ(6) command with sufficient buffer size
        // READ(6) format: [0x08, 0, 0, length_high, length_low, 0]
        let cmd = [0x08, 0, 0, 0xFF, 0xFF, 0]; // Large buffer

        // Execute the READ(6) command
        let result = eth.specific_cmd(&cmd, None).unwrap();

        // Verify we got data back
        if let ScsiCmdResult::DataIn(data) = result {
            // Count how many packets were returned by looking for packet headers
            // Each packet has a 6-byte header starting with frame length
            let mut packet_count = 0;
            let mut offset = 0;

            while offset < data.len() {
                if offset + 6 > data.len() {
                    break;
                }

                // Read frame length from header
                let frame_len = ((data[offset] as usize) << 8) | (data[offset + 1] as usize);

                // Check if this is a valid header (not just zeros)
                if frame_len == 0 {
                    break;
                }

                packet_count += 1;

                // Move to next packet (6-byte header + frame_len)
                offset += 6 + frame_len;
            }

            // Verify exactly 8 packets were returned
            assert_eq!(packet_count, 8, "Expected 8 packets, got {}", packet_count);

            // Verify the "more" flag is set correctly on the 8th packet
            // The "more" flag should be 0 (not set) for the last packet
            let mut offset = 0;
            for i in 0..8 {
                let frame_len = ((data[offset] as usize) << 8) | (data[offset + 1] as usize);
                let more_flag = data[offset + 5];

                if i < 7 {
                    // First 7 packets should have "more" flag set (0x10)
                    assert_eq!(more_flag, 0x10, "Packet {} should have 'more' flag set", i);
                } else {
                    // 8th packet should NOT have "more" flag set
                    assert_eq!(more_flag, 0, "Packet {} should NOT have 'more' flag set", i);
                }

                offset += 6 + frame_len;
            }

            // Verify 2 packets remain in the queue
            assert_eq!(
                eth.port.rx_len(),
                2,
                "Expected 2 packets remaining in queue"
            );
        } else {
            panic!("Expected DataIn result");
        }
    }
    #[test]
    fn test_read_drops_only_oversized_packet() {
        let mut eth = ScsiTargetEthernet::default();
        let tx = eth.port.connect_test_rx();

        tx.send(vec![0x11; 100]).unwrap();
        tx.send(vec![0x22; 1000]).unwrap();
        tx.send(vec![0x33; 100]).unwrap();

        // Room for a 100-byte frame, not for the 1000-byte one
        let cmd = [0x08, 0, 0, 0x01, 0x00, 0];
        let ScsiCmdResult::DataIn(data) = eth.specific_cmd(&cmd, None).unwrap() else {
            panic!("Expected DataIn result");
        };

        let frame_len = 100 + 4;
        assert_eq!(data.len(), 2 * (6 + frame_len));
        assert_eq!(data[5], 0x10);
        assert_eq!(data[6], 0x11);
        assert_eq!(data[6 + frame_len + 5], 0);
        assert_eq!(data[6 + frame_len + 6], 0x33);
        assert_eq!(eth.port.rx_len(), 0);
    }
}
