use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

/// A keyboard event. Inner value is the scancode
pub enum KeyEvent {
    KeyDown(u8),
    KeyUp(u8),
}

/// Communication channel (sender) for keyboard events to an emulated keyboard
pub type KeyEventSender = crossbeam_channel::Sender<KeyEvent>;

/// Communication channel (receiver) for keyboard events to an emulated keyboard
pub type KeyEventReceiver = crossbeam_channel::Receiver<KeyEvent>;

/// Communication channel (sender) for click events to an emulated mouse
pub type ClickEventSender = crossbeam_channel::Sender<bool>;

/// Communication channel (receiver) for click events to an emulated mouse
pub type ClickEventReceiver = crossbeam_channel::Receiver<bool>;

pub type Byte = u8;
pub type Word = u16;
pub type Long = u32;

bitfield! {
    /// General purpose 16-bit field
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Field16(pub u16): Debug, FromRaw, IntoRaw, DerefRaw {
        pub msb: u8 @ 8..16,
        pub lsb: u8 @ 0..8,
    }
}

/// A self-clearing latch for events etc.
#[derive(Debug, Default)]
pub struct LatchingEvent {
    val: bool,
}

impl LatchingEvent {
    /// Returns the current value and clears the event.
    pub fn get_clear(&mut self) -> bool {
        let v = self.val;
        self.val = false;
        v
    }

    /// Sets the event.
    pub fn set(&mut self) {
        self.val = true;
    }
}
