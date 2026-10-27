//! AppleTalk protocol stack for Snow
//!
//! Host-side implementation of the AppleTalk protocols (DDP, NBP, ATP, ASP)
//! and the built-in AFP file server, speaking to the emulated machine as a
//! second node on its network.

pub mod ddp;
pub mod link;
pub mod nbp;
pub mod node;

#[cfg(feature = "test_module")]
pub mod test;
pub mod wire;
