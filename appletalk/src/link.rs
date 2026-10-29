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
