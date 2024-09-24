//! Apple Desktop Bus transceiver and peripherals

pub mod keyboard;
pub mod mouse;
pub mod transceiver;

pub use keyboard::AdbKeyboard;
pub use mouse::AdbMouse;
use proc_bitfield::bitfield;
pub use transceiver::AdbTransceiver;

use arrayvec::ArrayVec;

pub type AdbDeviceResponse = ArrayVec<u8, 8>;

pub trait AdbDevice {
    fn reset(&mut self);
    fn flush(&mut self);
    fn talk(&mut self, reg: u8) -> AdbDeviceResponse;
    fn listen(&mut self, reg: u8) -> AdbDeviceResponse;
    fn get_srq(&self) -> bool;
}

pub type AdbDeviceInstance = Box<dyn AdbDevice + Send>;

bitfield! {
    /// Register 3
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct AdbReg3(pub u16): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Handler ID
        pub handler_id: u8 @ 0..=7,
        /// ADB address
        pub address: u8 @ 8..=11,
        /// Service request
        pub srq: bool @ 13,
        /// Exceptional event
        pub exceptional: bool @ 14,
    }
}
