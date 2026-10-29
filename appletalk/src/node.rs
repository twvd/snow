//! AppleTalk node
//!
//! Receives DDP datagrams from the link layers and dispatches them to the
//! protocol modules that own each socket.

use crossbeam_channel::{Receiver, Sender};

use crate::afp::{Afp, AfpSettings, MaxVersion};
use crate::asp::Asp;
use crate::atp::{ATP_SOCKET, ATP_TYPE, Atp};
use crate::ddp::{DdpShortHeader, build_short, parse_short};
use crate::link::{DdpHeaderKind, Packet};
use crate::nbp::{NBP_SOCKET, NBP_TYPE, Nbp, NbpEntity};

#[cfg(feature = "test_module")]
use crate::test::{TEST_SOCKET, TEST_TYPE, TestModule};

/// The AppleTalk Echo Protocol: socket 4, DDP type 4
const AEP_SOCKET: u8 = 4;
const AEP_TYPE: u8 = 4;
const AEP_ECHO_REQUEST: u8 = 1;
const AEP_ECHO_REPLY: u8 = 2;

/// AppleTalk node
pub struct Node {
    rx: Receiver<Packet>,
    tx: Sender<Packet>,
    #[cfg(feature = "test_module")]
    test: TestModule,
    nbp: Nbp,
    atp: Atp,
    asp: Asp,
}

impl Node {
    /// Create a node with the given node ID (its LLAP address on the
    /// link) and network number (0 on a nonextended network). The AFP
    /// server shares the given directory as a volume (the
    /// SNOW_AFP_ROOT environment variable when unset) under the given
    /// volume name (default "Snow"), advertising every AFP version up
    /// to the given maximum.
    #[allow(clippy::too_many_arguments)] // the AFP server's configuration
    pub fn new(
        node: u8,
        net: u16,
        rx: Receiver<Packet>,
        tx: Sender<Packet>,
        share_root: Option<std::path::PathBuf>,
        volume_name: Option<String>,
        max_version: MaxVersion,
        settings: AfpSettings,
    ) -> Self {
        Self {
            rx,
            tx,
            #[cfg(feature = "test_module")]
            test: TestModule,
            nbp: Nbp::new(
                node,
                net,
                vec![NbpEntity {
                    object: "Snow".to_string(),
                    entity_type: "AFPServer".to_string(),
                    zone: "*".to_string(),
                    socket: ATP_SOCKET,
                    enumerator: 1,
                }],
            ),
            atp: Atp::new(),
            asp: Asp::new(Afp::new_with(
                share_root,
                volume_name,
                max_version,
                settings,
            )),
        }
    }

    /// Handle one packet, returning the reply packets if any
    pub fn handle(&mut self, packet: &Packet) -> Option<Vec<Packet>> {
        let DdpHeaderKind::Short = packet.kind else {
            log::debug!("Dropped extended DDP datagram");
            return None;
        };
        let (header, data) = match parse_short(&packet.datagram) {
            Ok(parsed) => parsed,
            Err(e) => {
                log::debug!("Dropped invalid short DDP datagram: {}", e);
                return None;
            }
        };
        #[cfg(feature = "wire_debug")]
        if crate::wire::wire_debug_enabled() {
            log::info!(
                "NODE <- node {}: dst_socket {:02x} src_socket {:02x} type {:02x} | {}",
                packet.node,
                header.dst_socket,
                header.src_socket,
                header.ddp_type,
                crate::wire::hex(data)
            );
        }
        if header.dst_socket == AEP_SOCKET && header.ddp_type == AEP_TYPE {
            return self.aep_echo(packet, &header, data).map(|p| vec![p]);
        }
        if header.dst_socket == NBP_SOCKET && header.ddp_type == NBP_TYPE {
            return self.nbp.handle(packet, data).map(|p| vec![p]);
        }
        if header.dst_socket == ATP_SOCKET && header.ddp_type == ATP_TYPE {
            return self.atp.handle(packet, data, &mut self.asp);
        }
        #[cfg(feature = "test_module")]
        if header.dst_socket == TEST_SOCKET && header.ddp_type == TEST_TYPE {
            return self.test.handle(packet, &header, data).map(|p| vec![p]);
        }
        log::debug!(
            "Dropped DDP datagram for closed socket {}",
            header.dst_socket
        );
        None
    }

    /// AEP: answer an Echo Request (function 1) with an Echo Reply
    /// (function 2) carrying the same data
    fn aep_echo(&self, packet: &Packet, header: &DdpShortHeader, data: &[u8]) -> Option<Packet> {
        if data.first() != Some(&AEP_ECHO_REQUEST) {
            return None;
        }
        let mut reply = Vec::with_capacity(data.len());
        reply.push(AEP_ECHO_REPLY);
        reply.extend_from_slice(&data[1..]);
        Some(Packet {
            kind: packet.kind,
            node: packet.node,
            datagram: build_short(header.src_socket, AEP_SOCKET, AEP_TYPE, &reply),
        })
    }

    /// Run the node until the receive channel is closed
    pub fn run(mut self) {
        // The receive carries a timeout so the loop doubles as the
        // timer source: the periodic tick runs the housekeeping that
        // needs one (the ASP tickles). The next-tick deadline is
        // also checked after every packet, so a long copy does not
        // starve the housekeeping
        let tick_interval = std::time::Duration::from_secs(1);
        let mut next_tick = std::time::Instant::now() + tick_interval;
        loop {
            let wait = next_tick.saturating_duration_since(std::time::Instant::now());
            let packet = match self.rx.recv_timeout(wait) {
                Ok(packet) => packet,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    self.tick();
                    next_tick = std::time::Instant::now() + tick_interval;
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            };
            // A request must never kill the node: a panic drops the
            // packet and the server keeps serving
            let replies =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.handle(&packet)));
            let Some(replies) = (match replies {
                Ok(replies) => replies,
                Err(_) => {
                    log::error!("AppleTalk node: a request panicked; dropping it");
                    None
                }
            }) else {
                continue;
            };
            for reply in replies {
                if self.tx.send(reply).is_err() {
                    return;
                }
            }
            // The housekeeping does not wait for a quiet line
            let now = std::time::Instant::now();
            if now >= next_tick {
                self.tick();
                next_tick = now + tick_interval;
            }
        }
    }

    /// The periodic housekeeping tick (the node loop's timer source):
    /// the ASP tickles
    fn tick(&mut self) {
        for packet in self.asp.tick(std::time::Instant::now()) {
            // A tickle carries no information once its moment has
            // passed: if the bridge's queue is full (a paused
            // emulator does not drain it), drop it instead of
            // blocking the node thread
            if self.tx.try_send(packet).is_err() {
                break;
            }
        }
    }
}

#[cfg(all(test, feature = "test_module"))]
mod tests {
    use super::*;
    use crate::ddp::{build_short, parse_short};

    fn request(dst_socket: u8, data: &[u8]) -> Packet {
        Packet {
            kind: DdpHeaderKind::Short,
            node: 0x20,
            datagram: build_short(dst_socket, 0x30, TEST_TYPE, data),
        }
    }

    fn node() -> Node {
        let (_, rx) = crossbeam_channel::bounded(16);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(16);
        drop(reply_rx);
        Node::new(
            0xD7,
            0,
            rx,
            reply_tx,
            None,
            None,
            MaxVersion::default(),
            AfpSettings::default(),
        )
    }

    #[test]
    fn test_echo() {
        let (tx, rx) = crossbeam_channel::bounded(16);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(16);
        let node = Node::new(
            0xD7,
            0,
            rx,
            reply_tx,
            None,
            None,
            MaxVersion::default(),
            AfpSettings::default(),
        );

        tx.send(request(TEST_SOCKET, b"SNT1hello")).unwrap();
        drop(tx);

        node.run();

        let reply = reply_rx.try_recv().unwrap();
        assert_eq!(reply.node, 0x20);
        let (header, data) = parse_short(&reply.datagram).unwrap();
        assert_eq!(header.dst_socket, 0x30);
        assert_eq!(header.src_socket, TEST_SOCKET);
        assert_eq!(header.ddp_type, TEST_TYPE);
        assert_eq!(data, &b"SNT1hello"[..]);
    }

    #[test]
    fn test_unknown_socket() {
        let (tx, rx) = crossbeam_channel::bounded(16);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(16);
        let node = Node::new(
            0xD7,
            0,
            rx,
            reply_tx,
            None,
            None,
            MaxVersion::default(),
            AfpSettings::default(),
        );

        tx.send(request(0x40, b"SNT1hello")).unwrap();
        drop(tx);

        node.run();

        assert!(reply_rx.try_recv().is_err());
    }

    #[test]
    fn test_nbp_broadcast_gets_reply() {
        // A BrdRq to the NBP socket is answered with a LkUp-Reply
        let mut payload = vec![(1 << 4) | 1, 7];
        payload.extend_from_slice(&[0, 0, 0x20, 0x30, 0]);
        payload.extend_from_slice(&[1, b'=']);
        payload.extend_from_slice(&[1, b'=']);
        payload.extend_from_slice(&[1, b'*']);
        let packet = Packet {
            kind: DdpHeaderKind::Short,
            node: 0x20,
            datagram: build_short(NBP_SOCKET, 0x30, NBP_TYPE, &payload),
        };

        let reply = &node().handle(&packet).unwrap()[0];
        assert_eq!(reply.node, 0x20);
        assert_eq!(reply.datagram[2], 0x30); // dst = requester socket
        assert_eq!(reply.datagram[3], NBP_SOCKET);
        assert_eq!(reply.datagram[4], NBP_TYPE);
        assert_eq!(reply.datagram[5], (3 << 4) | 1); // LkUp-Reply
        assert_eq!(reply.datagram[6], 7); // same NBP ID
        // The entity tuple carries the ATP socket
        assert_eq!(reply.datagram[10], ATP_SOCKET);
    }

    #[test]
    fn test_asp_session_and_command() {
        let mut node = node();

        // Open a session on the ATP socket, then send a command
        let mut payload = vec![0x40 | 0x20, 0x01, 0x12, 0x34, 4, 0x64, 0, 1];
        let open = Packet {
            kind: DdpHeaderKind::Short,
            node: 0x20,
            datagram: build_short(ATP_SOCKET, 0x30, ATP_TYPE, &payload),
        };
        let reply = &node.handle(&open).unwrap()[0];
        assert_eq!(reply.datagram[3], ATP_SOCKET);
        assert_eq!(reply.datagram[5], 0x80 | 0x10 | 0x20); // TResp+EOM+XO
        assert_eq!(reply.datagram[9], ATP_SOCKET); // SSS
        assert_eq!(reply.datagram[10], 1); // session ID

        // A command on session 1: FPGetSrvrParms returns the volume list
        payload = vec![0x40 | 0x20, 0x01, 0x12, 0x35, 2, 1, 0, 1, 16, 0];
        let cmd = Packet {
            kind: DdpHeaderKind::Short,
            node: 0x20,
            datagram: build_short(ATP_SOCKET, 0x30, ATP_TYPE, &payload),
        };
        let reply = &node.handle(&cmd).unwrap()[0];
        assert_eq!(reply.datagram[9..13], [0, 0, 0, 0]); // command result
        assert_eq!(reply.datagram[17], 1); // one volume
        assert_eq!(reply.datagram[19], 4); // volume name length
        assert_eq!(&reply.datagram[20..24], b"Snow");
    }

    #[test]
    fn test_bad_magic() {
        let (tx, rx) = crossbeam_channel::bounded(16);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(16);
        let node = Node::new(
            0xD7,
            0,
            rx,
            reply_tx,
            None,
            None,
            MaxVersion::default(),
            AfpSettings::default(),
        );

        tx.send(request(TEST_SOCKET, b"NOPE")).unwrap();
        drop(tx);

        node.run();

        assert!(reply_rx.try_recv().is_err());
    }
}
