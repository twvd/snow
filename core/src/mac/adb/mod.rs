//! Apple Desktop Bus transceiver and peripherals

pub mod keyboard;
pub mod mouse;
pub mod transceiver;

pub use keyboard::AdbKeyboard;
pub use mouse::AdbMouse;
pub use transceiver::AdbTransceiver;

use arrayvec::ArrayVec;
use proc_bitfield::bitfield;

use crate::keymap::KeyEvent;
use crate::types::MouseEvent;

pub type AdbDeviceResponse = ArrayVec<u8, 8>;

/// Dispatchable ADB events
pub enum AdbEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
}

#[typetag::serde(tag = "type")]
pub trait AdbDevice: Send {
    fn reset(&mut self);
    fn flush(&mut self);
    fn talk(&mut self, reg: u8) -> AdbDeviceResponse;
    fn listen(&mut self, reg: u8, data: &[u8]);
    fn get_srq(&self) -> bool;
    fn get_address(&self) -> u8;
    fn event(&mut self, event: &AdbEvent);
}

pub type AdbDeviceInstance = Box<dyn AdbDevice>;

bitfield! {
    /// Register 3
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct AdbReg3(pub u16): Debug, FromStorage, IntoStorage, DerefStorage {
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
