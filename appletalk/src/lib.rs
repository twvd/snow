//! AppleTalk protocol stack for Snow
//!
//! Host-side implementation of the AppleTalk protocols (DDP, NBP, ATP, ASP)
//! and the built-in AFP file server, speaking to the emulated machine as a
//! second node on its network.

pub mod afp;
pub mod asp;
pub mod atp;
pub mod ddp;
pub mod ids;
pub mod link;
pub mod name;
pub mod nbp;
pub mod node;

#[cfg(feature = "test_module")]
pub mod test;
pub mod wire;
