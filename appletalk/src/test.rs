//! Test module for validating the AppleTalk transport stack
//!
//! Echoes DDP datagrams sent to the test socket, so a test program running
//! in the emulated machine can exercise the link and DDP layers.

use crate::ddp::{DdpShortHeader, build_short};
use crate::link::Packet;

/// Experimental statically assigned socket of the test module
pub const TEST_SOCKET: u8 = 0x64;

/// DDP type of the test protocol
pub const TEST_TYPE: u8 = 0x7F;

/// All test datagrams start with this magic
pub const TEST_MAGIC: [u8; 4] = *b"SNT1";

/// Test protocol module
pub struct TestModule;

impl TestModule {
    /// Echo a test datagram back to its source socket
    pub fn handle(&self, packet: &Packet, header: &DdpShortHeader, data: &[u8]) -> Option<Packet> {
        if !data.starts_with(&TEST_MAGIC) {
            log::debug!("Dropped test datagram without the magic");
            return None;
        }
        Some(Packet {
            kind: packet.kind,
            node: packet.node,
            datagram: build_short(header.src_socket, TEST_SOCKET, TEST_TYPE, data),
        })
    }
}
