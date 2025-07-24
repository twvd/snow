use proc_bitfield::bitfield;

use crate::types::Word;

bitfield! {
    /// PMOVE format 1
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct Pmove1Extword(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        pub write: bool @ 9,
        pub preg: usize @ 10..=12,
    }
}
