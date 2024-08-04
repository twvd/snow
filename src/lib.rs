// Clippy confuguration
#![allow(clippy::new_without_default)]
#![allow(clippy::unit_arg)]
#![allow(clippy::single_match)]

pub mod bus;
pub mod cpu_m68k;
pub mod emulator;
pub mod frontend;
pub mod mac;
pub mod tickable;
pub mod types;
pub mod util;

#[cfg(test)]
pub mod test;
