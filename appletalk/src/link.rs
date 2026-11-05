//! Interface between the AppleTalk node and the link layers

/// Whether a packet uses a short or an extended DDP header
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DdpHeaderKind {
    Short,
    Extended,
}

/// A DDP datagram exchanged with a link layer
#[derive(Clone)]
pub struct Packet {
    /// The DDP header kind of the datagram
    pub kind: DdpHeaderKind,
    /// LLAP node address of the peer (source of received packets,
    /// destination of sent ones)
    pub node: u8,
    /// The full DDP datagram, header and data
    pub datagram: Vec<u8>,
}

impl Packet {
    /// The link layer's report that a node is acquiring its address, so
    /// it has just started and holds no session any more. No datagram is
    /// empty, so this cannot be mistaken for one.
    pub fn node_started(node: u8) -> Self {
        Self {
            kind: DdpHeaderKind::Short,
            node,
            datagram: Vec::new(),
        }
    }

    /// Whether this is the report from `node_started`
    pub fn is_node_started(&self) -> bool {
        self.datagram.is_empty()
    }
}
