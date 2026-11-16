//! The device model: the I/O devices of a machine that the frontend shows, configures or acts on
//!
//! See docs/misc/device-model.md.

mod function;

pub use function::*;

/// What the emulator can ask of any device, whichever bus it is on
pub(crate) trait DeviceOps {
    /// The device, seen through its function. Called by the emulator to run a command of the
    /// frontend on the device, or to read its status.
    fn function(&mut self) -> FunctionMut<'_>;
}
