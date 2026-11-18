//! The drive connector of the floppy controller, as a device on it sees it

use super::stream::FloppyStream;
use crate::devices::DeviceOps;
use crate::tickable::Ticks;

/// What the floppy controller sees of a device on a drive connector.
///
/// The methods here are for what happens rarely: the lines of the connector, and whole bytes
/// for a device such as a UniDisk 3.5. The bits of a floppy disk do not go through this trait: a
/// floppy drive hands out its `FloppyStream` with `stream()`, once per controller tick, and
/// the controller reads and writes the bits on it directly. See `FloppyStream` for why.
#[typetag::serde(tag = "type")]
pub(crate) trait SwimBusDevice: DeviceOps + Send {
    /// Called by the machine to specify the connector and the tick rate, once, when the bus
    /// attaches the device and before any other method.
    ///
    /// `connector` is the place of the device on its bus, from 0. `tick_rate` is the number of
    /// ticks in one second, for the ticks that `tick()` counts: the device needs it to turn a
    /// duration into ticks, such as the time its head needs to settle, or one turn of its disk.
    /// Neither is in the device's config: nobody chooses them, and only the machine knows them.
    fn connected(&mut self, connector: usize, tick_rate: Ticks);

    /// The control lines of the connector changed: CA0 to CA2 and SEL on a Mac, in bits 1 to 3
    /// and 0. Called by the controller on the device it selects, when a line changes and when
    /// it selects the device.
    fn set_lines(&mut self, lines: u8);

    /// The strobe line rose, with the control lines at `lines`. Called by the controller on
    /// the device it selects and enables. A device that is chained with others returns true
    /// when it passes the selection on to the next device of the chain.
    fn strobe(&mut self, lines: u8) -> bool;

    /// The level of the sense line, with the control lines at `lines`. Called by the
    /// controller on the device it selects, when the guest reads the line.
    fn sense(&self, lines: u8) -> bool;

    /// The PWM line of the connector, on the Macs that set the speed of their drives with it:
    /// the length of one pulse. Called by the controller on every device, once per scan line of
    /// the screen. Nothing by default.
    fn set_pwm(&mut self, _pulse: u8) {}

    /// Time passing, in ticks of the controller. Called by the controller on every device,
    /// whether selected or not, before it calls another method of a device: a device has the
    /// time of the controller whenever it is called.
    fn tick(&mut self, ticks: Ticks);

    /// The machine was reset. Called by the controller on every device, when the controller
    /// itself is reset. Nothing by default: a drive keeps its disk and the place of its head.
    fn reset(&mut self) {}

    /// For a floppy drive: its disk, spinning or not, which `FloppyStream::spinning` tells.
    /// Called by the controller on the device it selects, once per tick. None by default: a
    /// device with no stream exchanges whole bytes with the controller.
    ///
    /// Whether the disk spins must only change while one of the device's methods runs, called
    /// by the controller or through its function. A device whose disk doesn't spin is asked
    /// again after such a call, and not on every tick.
    fn stream(&mut self) -> Option<&mut FloppyStream> {
        None
    }

    /// For a device that exchanges whole bytes with the controller: the byte it sends, if one
    /// is due after `ticks` more ticks. Called by a controller on the device it selects, when
    /// that device has no stream. Nothing by default.
    fn read_byte(&mut self, _ticks: Ticks) -> Option<u8> {
        None
    }

    /// For a device that exchanges whole bytes with the controller: a byte the controller
    /// wrote. Nothing by default.
    fn write_byte(&mut self, _byte: u8) {}

    /// The device as the floppy drive it is, for the tests that look inside a drive
    #[cfg(test)]
    fn as_floppy_drive(&mut self) -> &mut super::drive::FloppyDrive {
        panic!("Not a floppy drive")
    }
}
