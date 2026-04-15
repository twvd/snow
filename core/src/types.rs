use num::PrimInt;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

/// Event type for mouse events to an emulated mouse
#[derive(Default, Serialize, Deserialize, Clone)]
pub struct MouseEvent {
    pub button: Option<bool>,
    pub rel_movement: Option<(i32, i32)>,
}

/// Communication channel (sender) for sending samples to the host audio device.
pub type AudioSampleSender = crossbeam_channel::Sender<u8>;

pub type Byte = u8;
pub type SignedByte = i8;
pub type Word = u16;
pub type SignedWord = i16;
pub type Long = u32;
pub type SignedLong = i32;
pub type DoubleLong = u64;

pub trait MyIntTraits: PrimInt {
    /// See `u32::overflowing_add`
    fn overflowing_add(self, rhs: Self) -> (Self, bool);

    /// See `u32::overflowing_sub`
    fn overflowing_sub(self, rhs: Self) -> (Self, bool);
}

impl MyIntTraits for Byte {
    fn overflowing_add(self, rhs: Self) -> (Self, bool) {
        self.overflowing_add(rhs)
    }

    fn overflowing_sub(self, rhs: Self) -> (Self, bool) {
        self.overflowing_sub(rhs)
    }
}

impl MyIntTraits for SignedByte {
    fn overflowing_add(self, rhs: Self) -> (Self, bool) {
        self.overflowing_add(rhs)
    }

    fn overflowing_sub(self, rhs: Self) -> (Self, bool) {
        self.overflowing_sub(rhs)
    }
}

impl MyIntTraits for Word {
    fn overflowing_add(self, rhs: Self) -> (Self, bool) {
        self.overflowing_add(rhs)
    }

    fn overflowing_sub(self, rhs: Self) -> (Self, bool) {
        self.overflowing_sub(rhs)
    }
}

impl MyIntTraits for SignedWord {
    fn overflowing_add(self, rhs: Self) -> (Self, bool) {
        self.overflowing_add(rhs)
    }

    fn overflowing_sub(self, rhs: Self) -> (Self, bool) {
        self.overflowing_sub(rhs)
    }
}

impl MyIntTraits for Long {
    fn overflowing_add(self, rhs: Self) -> (Self, bool) {
        self.overflowing_add(rhs)
    }

    fn overflowing_sub(self, rhs: Self) -> (Self, bool) {
        self.overflowing_sub(rhs)
    }
}

impl MyIntTraits for SignedLong {
    fn overflowing_add(self, rhs: Self) -> (Self, bool) {
        self.overflowing_add(rhs)
    }

    fn overflowing_sub(self, rhs: Self) -> (Self, bool) {
        self.overflowing_sub(rhs)
    }
}

pub trait MyUIntTraits: MyIntTraits {
    type Signed: MySIntTraits;

    /// Reinterpret the bits of this value as a signed integer
    fn cast_signed(self) -> Self::Signed;
}

impl MyUIntTraits for Byte {
    type Signed = SignedByte;

    fn cast_signed(self) -> Self::Signed {
        self.cast_signed()
    }
}

impl MyUIntTraits for Word {
    type Signed = SignedWord;

    fn cast_signed(self) -> Self::Signed {
        self.cast_signed()
    }
}

impl MyUIntTraits for Long {
    type Signed = SignedLong;

    fn cast_signed(self) -> Self::Signed {
        self.cast_signed()
    }
}

pub trait MySIntTraits: MyIntTraits + std::convert::Into<SignedLong> {}

impl MySIntTraits for SignedByte {}
impl MySIntTraits for SignedWord {}
impl MySIntTraits for SignedLong {}

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
#[derive(Debug, Default, Serialize, Deserialize)]
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
