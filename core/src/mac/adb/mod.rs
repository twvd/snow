//! Apple Desktop Bus transceiver and peripherals
//!
//! ## ADB transaction flow
//!
//! ```mermaid
//! stateDiagram-v2
//!     direction LR
//!     [*] --> Idle: Start
//!     Idle --> Transmit: Begin transaction
//!     Transmit --> Acknowledge: Command sent / Data sent
//!     Acknowledge --> Receive: Acknowledge received
//!     Acknowledge --> Idle: Acknowledge received (No data to receive)
//!     Receive --> Idle: Data received
//!     Idle --> [*]: End transaction
//!
//!     state Idle {
//!         ST0=1 / ST1=1
//!         :Bus idle, awaiting command;
//!     }
//!     state Transmit {
//!         ST0=0 / ST1=0
//!         :Send command or data;
//!     }
//!     state Acknowledge {
//!         ST0=1 / ST1=0
//!         :Wait for acknowledgment from device;
//!     }
//!     state Receive {
//!         ST0=0 / ST1=1
//!         :Receive data from ADB device;
//!     }
//! ```

pub mod transceiver;

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
