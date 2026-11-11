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

/// What a link layer hands the node
pub enum LinkEvent {
    /// A datagram for the node
    Datagram(Packet),
    /// A node is acquiring its address: it has just started, and holds no
    /// session any more
    NodeStarted(u8),
    /// The node's own address changed: another station holds the old one
    AddressChanged(u8),
    /// One second of the link's time has passed. The emulator sends it
    /// in emulated time, so a paused or fast guest sees the protocol
    /// timers run at its own pace.
    Tick,
}
