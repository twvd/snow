//! No request can make the stack panic. The node catches a panic and goes
//! on serving, so in use one would only drop a request without a trace:
//! these tests feed every parser random and truncated input from a fixed
//! seed instead.

use crate::afp::{Afp, AfpSettings, MaxVersion};
use crate::atp::{ATP_SOCKET, ATP_TYPE};
use crate::ddp::build_short;
use crate::link::{DdpHeaderKind, LinkEvent, Packet};
use crate::nbp::{NBP_SOCKET, NBP_TYPE};
use crate::node::Node;

/// A small xorshift generator: the same input on every run
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }

    /// Up to `max` random bytes
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = self.next() as usize % (max + 1);
        (0..len).map(|_| self.byte()).collect()
    }
}

/// The AFP functions the server answers, so most random requests get past
/// the dispatch
const AFP_FUNCTIONS: [u8; 41] = [
    1, 2, 4, 6, 7, 8, 9, 10, 11, 14, 16, 17, 18, 20, 23, 24, 26, 27, 28, 29, 30, 31, 32, 33, 34,
    35, 39, 40, 41, 42, 48, 49, 51, 52, 53, 54, 55, 56, 57, 58, 192,
];

#[test]
fn random_afp_commands_do_not_panic() {
    let dir = crate::test_dir("snow-no-panic-afp");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("folder")).unwrap();
    std::fs::write(dir.join("file"), b"data").unwrap();
    let settings = AfpSettings {
        read_only: false,
        ..AfpSettings::default()
    };
    let mut afp = Afp::new_with(dir.clone(), None, MaxVersion::default(), settings);
    let mut rng = Rng(0x5EED_0000_0001);
    // A logged-in 2.2 session, so the version-specific paths run too
    afp.command(1, b"\x12\x06AFP2.2\x0fNo User Authent");
    for _ in 0..50_000 {
        let mut request = vec![AFP_FUNCTIONS[rng.next() as usize % AFP_FUNCTIONS.len()]];
        request.extend(rng.bytes(40));
        // Often aim at the volume, the root and a real name
        if request.len() >= 8 && rng.byte() < 192 {
            request[2..4].copy_from_slice(&1u16.to_be_bytes());
            request[4..8].copy_from_slice(&2u32.to_be_bytes());
        }
        afp.command(1 + rng.byte() % 2, &request);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn random_datagrams_do_not_panic() {
    let dir = crate::test_dir("snow-no-panic-node");
    std::fs::create_dir_all(&dir).unwrap();
    let (_, rx) = crossbeam_channel::bounded(1);
    let (tx, _keep) = crossbeam_channel::unbounded();
    let mut node = Node::new(
        0xFE,
        0,
        rx,
        tx,
        dir,
        None,
        MaxVersion::default(),
        None,
        AfpSettings::default(),
    );
    let mut rng = Rng(0x5EED_0000_0002);
    let packet = |datagram| Packet {
        kind: DdpHeaderKind::Short,
        node: 0x20,
        datagram,
    };
    // A session to send commands on
    node.handle(&packet(build_short(
        ATP_SOCKET,
        0x40,
        ATP_TYPE,
        &[0x40, 0x01, 0, 1, 4, 0x41, 1, 0],
    )));
    for i in 0..100_000u32 {
        let datagram = match i % 4 {
            // Anything at all, truncated or not
            0 => rng.bytes(64),
            // NBP
            1 => build_short(NBP_SOCKET, 0x40, NBP_TYPE, &rng.bytes(48)),
            // ATP requests, often an ASP command or write on the session
            _ => {
                let mut atp = vec![
                    0x40 | (rng.byte() & 0x3F),
                    rng.byte(),
                    rng.byte(),
                    rng.byte(),
                ];
                if rng.byte() < 192 {
                    atp.extend([
                        [2, 8][usize::from(rng.byte() & 1)],
                        1,
                        rng.byte(),
                        rng.byte(),
                    ]);
                    atp.push(AFP_FUNCTIONS[rng.next() as usize % AFP_FUNCTIONS.len()]);
                }
                atp.extend(rng.bytes(32));
                build_short(ATP_SOCKET, 0x40, ATP_TYPE, &atp)
            }
        };
        node.handle(&packet(datagram));
        if i % 1000 == 0 {
            node.handle_event(LinkEvent::Tick);
        }
    }
}

#[test]
fn random_resource_forks_do_not_panic() {
    let mut rng = Rng(0x5EED_0000_0003);
    let valid = crate::rfork::test_support::fork(
        &crate::rfork::test_support::bndl_with_fref(),
        Some(&crate::rfork::test_support::fref_entries()),
    );
    for _ in 0..20_000 {
        // A real fork with some bytes changed, or cut short, or noise
        let mut fork = valid.clone();
        match rng.byte() % 3 {
            0 => {
                for _ in 0..1 + rng.byte() % 8 {
                    let at = rng.next() as usize % fork.len();
                    fork[at] = rng.byte();
                }
            }
            1 => fork.truncate(rng.next() as usize % fork.len()),
            _ => fork = rng.bytes(512),
        }
        crate::rfork::bundle_icons(&fork);
    }
}
