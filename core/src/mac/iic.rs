//! IIC bus, as a microcontroller drives it
//!
//! The Cuda runs the transactions on behalf of the CPU. The machine owns the
//! devices on its bus and hands them to the Cuda through this trait, so what is
//! on the bus is up to the machine.

/// The devices on an IIC bus, as seen from the master
///
/// Each transaction starts with an address byte, and what follows goes to the
/// device which acknowledged it. The transfer of the bits takes no time.
pub trait IicBus {
    /// Starts a transaction with the address byte, of which bit 0 is the read
    /// flag. Returns whether a device acknowledged it.
    fn start(&mut self, address: u8) -> bool;

    /// Writes a byte to the addressed device. Returns whether it acknowledged.
    fn write(&mut self, val: u8) -> bool;

    /// Reads a byte from the addressed device
    fn read(&mut self) -> u8;
}

/// A bus without any device: nothing acknowledges and reads float high, as on
/// the pull-ups of the lines
pub struct NoIic;

impl IicBus for NoIic {
    fn start(&mut self, _address: u8) -> bool {
        false
    }

    fn write(&mut self, _val: u8) -> bool {
        false
    }

    fn read(&mut self) -> u8 {
        0xFF
    }
}
