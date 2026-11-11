//! Packet loss at every position. A simulated workstation runs a
//! multi-packet FPRead and an FPWrite with its WriteContinue against the
//! node, retransmitting as ATP does (Inside AppleTalk, 2nd ed., chapter
//! 9): a request is sent again with the bitmap of the response packets
//! still missing. Each run drops one packet, the first, the second and
//! so on, in either direction, and the outcome must be the same as
//! without loss, with each command run once.

use crate::afp::{AfpSettings, MaxVersion};
use crate::atp::{ATP_SOCKET, ATP_TYPE};
use crate::ddp::{build_short, parse_short};
use crate::link::{DdpHeaderKind, Packet};
use crate::node::Node;

const CLIENT: u8 = 0x20;
/// The workstation's requesting socket and its session socket (WSS)
const REQ_SOCKET: u8 = 0x40;
const WSS: u8 = 0x41;
const TREQ_XO: u8 = 0x60;
const TRESP: u8 = 0x80;
const EOM: u8 = 0x10;
/// The most data one ATP response packet carries
const PACKET_DATA: usize = 578;

/// The workstation side, with one packet to lose
struct Client {
    node: Node,
    tid: u16,
    /// The packets exchanged so far in the current transaction, both ways
    seen: usize,
    /// Which of them is lost
    drop_at: Option<usize>,
}

impl Client {
    /// Whether the next packet on the line arrives
    fn arrives(&mut self) -> bool {
        let n = self.seen;
        self.seen += 1;
        Some(n) != self.drop_at
    }

    /// Hand a packet to the node, if it arrives
    fn deliver(&mut self, src: u8, atp: &[u8]) -> Vec<Packet> {
        if !self.arrives() {
            return Vec::new();
        }
        let packet = Packet {
            kind: DdpHeaderKind::Short,
            node: CLIENT,
            datagram: build_short(ATP_SOCKET, src, ATP_TYPE, atp),
        };
        self.node.handle(&packet).unwrap_or_default()
    }

    /// Run one transaction: an ATP XO request with these user bytes and
    /// data, retransmitted until every response packet is in. A
    /// WriteContinue from the node is answered with `write_data`. Returns
    /// the user bytes of the first response packet and the response data.
    fn transact(&mut self, user: [u8; 4], data: &[u8], write_data: &[u8]) -> ([u8; 4], Vec<u8>) {
        self.tid = self.tid.wrapping_add(1);
        let mut slots: [Option<([u8; 4], Vec<u8>)>; 8] = Default::default();
        let mut last = None;
        for _ in 0..20 {
            let missing = (0..8u8)
                .filter(|&i| slots[usize::from(i)].is_none())
                .fold(0u8, |b, i| b | 1 << i);
            let mut treq = vec![TREQ_XO, missing];
            treq.extend_from_slice(&self.tid.to_be_bytes());
            treq.extend_from_slice(&user);
            treq.extend_from_slice(data);
            let mut queue = self.deliver(REQ_SOCKET, &treq);
            while let Some(packet) = queue.pop() {
                if !self.arrives() {
                    continue;
                }
                let (header, atp) = parse_short(&packet.datagram).unwrap();
                let control = atp[0];
                let tid = u16::from_be_bytes([atp[2], atp[3]]);
                if header.dst_socket == REQ_SOCKET && control & 0xC0 == TRESP && tid == self.tid {
                    let seq = usize::from(atp[1]);
                    slots[seq] = Some((atp[4..8].try_into().unwrap(), atp[8..].to_vec()));
                    if control & EOM != 0 {
                        last = Some(seq);
                    }
                } else if header.dst_socket == WSS && control & 0xC0 == 0x40 {
                    // The node's WriteContinue: the data, in the packets its
                    // bitmap asks for
                    let chunks: Vec<&[u8]> = write_data.chunks(PACKET_DATA).collect();
                    for (seq, chunk) in chunks.iter().enumerate() {
                        if atp[1] & 1 << seq == 0 {
                            continue;
                        }
                        let eom = if seq + 1 == chunks.len() { EOM } else { 0 };
                        let mut tresp = vec![TRESP | eom, seq as u8, atp[2], atp[3], 0, 0, 0, 0];
                        tresp.extend_from_slice(chunk);
                        queue.extend(self.deliver(WSS, &tresp));
                    }
                }
            }
            if let Some(last) = last
                && slots[..=last].iter().all(Option::is_some)
            {
                let user = slots[0].as_ref().unwrap().0;
                let data = slots[..=last]
                    .iter()
                    .flat_map(|s| s.clone().unwrap().1)
                    .collect();
                return (user, data);
            }
        }
        panic!(
            "transaction {} never completed (lost packet {:?})",
            self.tid, self.drop_at
        );
    }

    /// An AFP command on session 1
    fn command(&mut self, afp: &[u8]) -> (i32, Vec<u8>) {
        let (user, data) = self.transact([2, 1, 0, self.tid as u8], afp, &[]);
        (i32::from_be_bytes(user), data)
    }
}

/// A node sharing a writable folder holding `file`, with a session open,
/// logged in, the volume open and `file` open for reading and writing;
/// the fork reference number comes back
fn setup(name: &str, contents: &[u8]) -> (Client, std::path::PathBuf, u16) {
    let dir = crate::test_dir(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("file"), contents).unwrap();
    let (_, rx) = crossbeam_channel::bounded(1);
    let (tx, _) = crossbeam_channel::unbounded();
    let settings = AfpSettings {
        read_only: false,
        ..AfpSettings::default()
    };
    let node = Node::new(
        0xFE,
        0,
        rx,
        tx,
        dir.clone(),
        None,
        MaxVersion::default(),
        None,
        settings,
    );
    let mut client = Client {
        node,
        tid: 0,
        seen: 0,
        drop_at: None,
    };
    let (user, _) = client.transact([4, WSS, 1, 0], &[], &[]);
    assert_eq!(user, [ATP_SOCKET, 1, 0, 0], "OpenSess");
    assert_eq!(client.command(b"\x12\x06AFP2.1\x0fNo User Authent").0, 0);
    assert_eq!(client.command(b"\x18\x00\x00\x20\x04Snow").0, 0);
    let mut open = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 4];
    open.extend_from_slice(b"file");
    let (result, reply) = client.command(&open);
    assert_eq!(result, 0, "OpenFork");
    (client, dir, u16::from_be_bytes([reply[2], reply[3]]))
}

/// The packets of one lossless run of `run`, so every position is tried
fn packets_in(name: &str, contents: &[u8], run: &dyn Fn(&mut Client, u16)) -> usize {
    let (mut client, _, fork) = setup(name, contents);
    client.seen = 0;
    run(&mut client, fork);
    client.seen
}

#[test]
fn a_multi_packet_read_survives_any_one_lost_packet() {
    let contents: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
    let read = |client: &mut Client, fork: u16| {
        let mut request = vec![27, 0];
        request.extend_from_slice(&fork.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&1500u32.to_be_bytes());
        request.extend_from_slice(&[0, 0]);
        let (result, data) = client.command(&request);
        assert!(result == 0 || result == -5009, "{result}");
        data
    };
    let total = packets_in("snow-loss-read", &contents, &|c, f| {
        read(c, f);
    });
    assert!(total >= 4, "a request and three response packets");
    for k in 0..total {
        let (mut client, _, fork) = setup("snow-loss-read", &contents);
        client.seen = 0;
        client.drop_at = Some(k);
        assert_eq!(read(&mut client, fork), contents, "lost packet {k}");
    }
}

#[test]
fn a_write_survives_any_one_lost_packet() {
    let data: Vec<u8> = (0..1200u32).map(|i| (i * 7) as u8).collect();
    // An append: run twice, it would write the data twice
    let write = |client: &mut Client, fork: u16, data: &[u8]| {
        let mut block = vec![33, 0x80];
        block.extend_from_slice(&fork.to_be_bytes());
        block.extend_from_slice(&0u32.to_be_bytes());
        block.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let (user, _) = client.transact([6, 1, 0, client.tid as u8], &block, data);
        assert_eq!(i32::from_be_bytes(user), 0);
    };
    let total = packets_in("snow-loss-write", b"head", &|c, f| write(c, f, &data));
    assert!(
        total >= 6,
        "the Write, the WriteContinue, three data packets, the reply"
    );
    for k in 0..total {
        let (mut client, dir, fork) = setup("snow-loss-write", b"head");
        client.seen = 0;
        client.drop_at = Some(k);
        write(&mut client, fork, &data);
        let written = std::fs::read(dir.join("file")).unwrap();
        assert_eq!(written.len(), 4 + data.len(), "lost packet {k}");
        assert_eq!(&written[4..], &data[..], "lost packet {k}");
    }
}
