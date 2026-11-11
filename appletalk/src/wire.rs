//! Wire-level diagnostics: the switch for the packet and command logs

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the wire-level logs print: off until the server's wire debug
/// setting turns them on
static WIRE_DEBUG: AtomicBool = AtomicBool::new(false);

/// Whether the wire-level logs print
pub fn wire_debug_enabled() -> bool {
    WIRE_DEBUG.load(Ordering::Relaxed)
}

/// Turn the wire-level logs on or off
pub fn set_wire_debug(on: bool) {
    WIRE_DEBUG.store(on, Ordering::Relaxed);
}

/// A debug log line that prints only while the wire-level dumps are on:
/// the protocol modules log every packet and command
macro_rules! wire_log {
    ($($arg:tt)*) => {
        if $crate::wire::wire_debug_enabled() {
            log::debug!($($arg)*);
        }
    };
}
pub(crate) use wire_log;

/// Format bytes as a space-separated hex string
pub fn hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 3);
    for (i, b) in data.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&format!("{b:02x}"));
    }
    out
}
