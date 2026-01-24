//! RPC interface for external control of the emulator
//!
//! This module provides a JSON-RPC 2.0 interface over Unix domain sockets (and optionally TCP)
//! for external processes to control the emulator.
//!
//! # Usage
//!
//! Start Snow with `--rpc` to enable the RPC interface. The socket will be created at
//! `$XDG_RUNTIME_DIR/snow-<PID>.sock` or `/tmp/snow-<PID>.sock`.
//!
//! # Example
//!
//! ```bash
//! echo '{"jsonrpc":"2.0","method":"status.get","id":1}' | socat - UNIX-CONNECT:/tmp/snow-$PID.sock
//! ```

mod handlers;
mod server;
mod types;

pub use handlers::RpcHandler;
pub use server::{RpcConfig, RpcMessage, RpcServer};
pub use types::*;
