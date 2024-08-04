use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

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
#[derive(Default)]
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
