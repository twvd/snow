//! AppleTalk protocol stack for Snow
//!
//! Host-side implementation of the AppleTalk protocols (DDP, NBP, ATP, ASP)
//! and the built-in AFP file server, speaking to the emulated machine as a
//! second node on its network.

pub mod afp;
pub mod asp;
pub mod atp;
pub mod ddp;
pub mod dtdb;
pub mod ids;
pub mod link;
pub mod meta;
pub mod name;
pub mod nbp;
pub mod node;
pub mod rfork;

#[cfg(feature = "test_module")]
pub mod test;
pub mod wire;

/// A scratch folder for a test, under one folder per test process, so
/// two test runs at once (from two worktrees) never share files
#[cfg(test)]
fn test_dir(name: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    std::env::temp_dir()
        .join(format!("snow-appletalk-tests-{}", std::process::id()))
        .join(name)
}
