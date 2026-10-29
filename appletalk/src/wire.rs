//! Wire-format diagnostic helpers for `wire_debug` builds

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the wire-level dumps print. Compiled-in `wire_debug` builds
/// stay quiet until this is turned on at runtime (the frontend's
/// advanced settings, or the SNOW_AT_WIRE_DEBUG environment variable).
static WIRE_DEBUG: OnceLock<AtomicBool> = OnceLock::new();

/// Whether the wire-level dumps print
pub fn wire_debug_enabled() -> bool {
    WIRE_DEBUG
        .get_or_init(|| {
            AtomicBool::new(std::env::var_os("SNOW_AT_WIRE_DEBUG").is_some_and(|v| v != "0"))
        })
        .load(Ordering::Relaxed)
}

/// Turn the wire-level dumps on or off at runtime
pub fn set_wire_debug(on: bool) {
    WIRE_DEBUG
        .get_or_init(|| AtomicBool::new(false))
        .store(on, Ordering::Relaxed);
}

/// Format bytes as a space-separated hex string
#[cfg(feature = "wire_debug")]
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
