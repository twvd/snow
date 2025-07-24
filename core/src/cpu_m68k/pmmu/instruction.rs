//! M68851 PMMU - Instruction decoding

use proc_bitfield::bitfield;

use crate::types::Word;

bitfield! {
    /// PMOVE format 1
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct Pmove1Extword(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        /// True: register to EA
        /// False: EA to register
        pub write: bool @ 9,

        /// PMMU register select
        pub preg: usize @ 10..=12,
    }
}
