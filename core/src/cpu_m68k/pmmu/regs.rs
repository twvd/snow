use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

bitfield! {
    /// Root pointer registers
    #[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct RootPointerReg(pub u64): Debug, FromStorage, IntoStorage, DerefStorage {
        /// If 1, this indicates that 'limit' is the LOWER limit
        /// If 0, this indicates that 'limit' is the UPPER limit
        pub lu: bool @ 63,

        /// Minimum/maximum (see 'lu') index to be used at the next table lookup
        pub limit: u16 @ 48..=62,

        /// Shared Globally
        pub sg: bool @ 41,

        /// Descriptor type
        pub dt: u8 @ 32..=33,

        /// Table base address (physical address)
        pub table_addr: u32 @ 4..=31,
    }
}

bitfield! {
    /// PMMU cache status register (PCSR)
    #[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct PcsrReg(pub u16): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Task Alias
        pub ta: u8 @ 0..=2,

        pub flush: bool @ 15,

        /// Lock Warning
        pub lw: bool @ 14,
    }
}

bitfield! {
    /// Translation Control
    #[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct TcReg(pub u32): Debug, FromStorage, IntoStorage, DerefStorage {
        pub enable: bool @ 31,

        /// Supervisor Root Pointer Enable
        pub sre: bool @ 25,

        /// Function Code Lookup
        pub fcl: bool @ 24,

        /// Page Size
        pub ps: u8 @ 20..=23,

        /// Initial Shift
        pub is: u32 @ 16..=19,

        pub tia: u8 @ 12..=15,
        pub tib: u8 @ 8..=11,
        pub tic: u8 @ 4..=7,
        pub tid: u8 @ 0..=3,
    }
}

bitfield! {
    /// Access Level
    #[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct AccessLevelReg(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub al: u8@5..=7,
    }
}

bitfield! {
    /// Access Control
    #[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct AccessControlReg(pub u16): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Module Control
        pub mc: bool @ 7,

        /// Access Level Control
        pub alc: u8 @ 4..=5,

        /// Module Descriptor Size
        pub mds: u8 @ 0..=1,
    }
}

bitfield! {
    /// PMMU status register
    #[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct PsrReg(pub u16): Debug, FromStorage, IntoStorage, DerefStorage {
        pub bus_error: bool @ 15,
        pub limit_violation: bool @ 14,
        pub supervisor_violation: bool @ 13,
        pub access_level_violatiom: bool @ 12,
        pub write_protected: bool @ 11,
        pub invalid: bool @ 10,
        pub modified: bool @ 9,
        pub gate: bool @ 8,
        pub globally_shared: bool @ 7,
        pub level_number: u8 @ 0..=2,
    }
}

/// PMMU register file
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, Default)]
pub struct PmmuRegisterFile {
    pub crp: RootPointerReg,
    pub srp: RootPointerReg,
    pub drp: RootPointerReg,
    pub pcsr: PcsrReg,
    pub cal: AccessLevelReg,
    pub val: AccessLevelReg,
    pub scc: u8,
    pub ac: AccessControlReg,
    pub tc: TcReg,
}
