//! M68851 PMMU - Instruction decoding

use proc_bitfield::bitfield;

use crate::types::Word;

bitfield! {
    /// PMOVE format 1
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct Pmove1Extword(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        pub fd: bool @ 8,

        /// True: register to EA
        /// False: EA to register
        pub write: bool @ 9,

        /// PMMU register select
        pub preg: usize @ 10..=12,
    }
}

bitfield! {
    /// PMOVE format 3
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct Pmove3Extword(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        /// True: register to EA
        /// False: EA to register
        pub write: bool @ 9,

        /// PMMU register select
        pub preg: usize @ 10..=12,
    }
}

bitfield! {
    /// PTEST
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct PtestExtword(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        pub fc: u8 @ 0..=4,
        pub an: usize @ 5..=7,
        pub a_set: bool @ 8,
        pub read: bool @ 9,
        pub level: u8 @ 10..=12,
    }
}
