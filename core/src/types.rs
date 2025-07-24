use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::keymap::KeyEvent;

/// Communication channel (sender) for keyboard events to an emulated keyboard
pub type KeyEventSender = crossbeam_channel::Sender<KeyEvent>;

/// Communication channel (receiver) for keyboard events to an emulated keyboard
pub type KeyEventReceiver = crossbeam_channel::Receiver<KeyEvent>;

/// Communication channel (sender) for mouse events to an emulated mouse
#[derive(Default)]
pub struct MouseEvent {
    pub button: Option<bool>,
    pub rel_movement: Option<(i32, i32)>,
}
pub type MouseEventSender = crossbeam_channel::Sender<MouseEvent>;

/// Communication channel (receiver) for click events to an emulated mouse
pub type MouseEventReceiver = crossbeam_channel::Receiver<MouseEvent>;

/// Communication channel (sender) for sending samples to the host audio device.
pub type AudioSampleSender = crossbeam_channel::Sender<u8>;

pub type Byte = u8;
pub type Word = u16;
pub type Long = u32;
pub type DoubleLong = u64;

bitfield! {
    /// General purpose 16-bit field
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct Field16(pub u16): Debug, FromStorage, IntoStorage, DerefStorage {
        pub msb: u8 @ 8..16,
        pub lsb: u8 @ 0..8,
    }
}

bitfield! {
    /// General purpose 32-bit field
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct Field32(pub u32): Debug, FromStorage, IntoStorage, DerefStorage {
        pub be0: u8 @ 24..32,
        pub be1: u8 @ 16..24,
        pub be2: u8 @ 8..16,
        pub be3: u8 @ 0..8,
    }
}

impl Field32 {
    #[inline(always)]
    pub fn set_be(&mut self, idx: usize, val: u8) {
        match idx {
            0 => self.set_be0(val),
            1 => self.set_be1(val),
            2 => self.set_be2(val),
            3 => self.set_be3(val),
            _ => panic!("Index out of bounds"),
        }
    }

    #[inline(always)]
    pub fn be(&mut self, idx: usize) -> u8 {
        match idx {
            0 => self.be0(),
            1 => self.be1(),
            2 => self.be2(),
            3 => self.be3(),
            _ => panic!("Index out of bounds"),
        }
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

    /// Peeks at the event without clearing it
    pub fn peek(&self) -> bool {
        self.val
    }
}
