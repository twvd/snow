//! Apple Desktop Bus transceiver and peripherals

pub mod mouse;
pub mod transceiver;

pub use mouse::AdbMouse;
pub use transceiver::AdbTransceiver;

use arrayvec::ArrayVec;

pub type AdbDeviceResponse = ArrayVec<u8, 8>;

pub trait AdbDevice {
    fn reset(&mut self);
    fn flush(&mut self);
    fn talk(&mut self, reg: u8) -> AdbDeviceResponse;
    fn listen(&mut self, reg: u8) -> AdbDeviceResponse;
}

pub type AdbDeviceInstance = Box<dyn AdbDevice + Send>;
