// Clippy confuguration
//#![warn(clippy::pedantic)]
#![warn(clippy::explicit_iter_loop)]
#![warn(clippy::large_enum_variant)]
#![warn(clippy::large_types_passed_by_value)]
#![warn(clippy::large_stack_frames)]
#![warn(clippy::needless_pass_by_value)]
#![warn(clippy::semicolon_if_nothing_returned)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::doc_markdown)]
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
