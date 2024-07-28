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
