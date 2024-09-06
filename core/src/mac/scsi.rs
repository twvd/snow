//! SCSI bus and controller stuff
//!
//! ## Bus phases and transitions
//! ```mermaid
//! stateDiagram-v2
//!     [*] --> Idle
//!
//!     Idle --> Arbitration : Assert BSY
//!     Arbitration --> Selection : Assert SEL
//!     Arbitration --> Idle : Release BSY (Lose Arbitration)
//!     
//!     Selection --> Command : Assert C/D, REQ
//!     Command --> DataTransfer : Assert I/O, REQ
//!     DataTransfer --> Status : Assert REQ, Status Byte
//!     Status --> Message : Assert MSG, REQ
//!     Message --> Idle : Release BSY (End of Command)
//!     
//!     Idle --> Reselection : Assert BSY, SEL
//!     Reselection --> Command : Assert C/D, REQ
//!     
//!     StateChange --> Idle: Reset (Release all signals)
//! ```

use log::*;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};

#[allow(dead_code)]
#[derive(Debug)]
/// SCSI bus phases
enum ScsiBusPhase {
    Free,
    Arbitration,
    Selection,
    Reselection,
    Command,
    DataIn,
    DataOut,
    Status,
    MessageIn,
    MessageOut,
}

#[allow(non_camel_case_types)]
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, PartialEq, Eq, Clone, Copy, FromPrimitive)]
enum NcrReg {
    /// Current Data Register / Output Data Register
    CDR_ODR,
    /// Initiator Command Register
    ICR,
    /// Mode Register
    MR,
    /// Target Command Register
    TCR,
    /// Current SCSI bus status
    CSR,
    /// Bus and Status register
    BSR,
    /// Input Data Register
    IDR,
    /// Reset parity/interrupt
    RESET,
}

bitfield! {
    /// NCR 5380 Mode Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    struct NcrRegMr(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        pub arbitrate: bool @ 0,
        pub dma_mode: bool @ 1,
        pub monitor_busy: bool @ 2,
        pub eop_int: bool @ 3,
        pub parity_int: bool @ 4,
        pub parity_check: bool @ 5,
        pub target_mode: bool @ 6,
        pub block_dma: bool @ 7,
    }
}

bitfield! {
    /// NCR 5380 Initiator Control Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    struct NcrRegIcr(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        pub assert_databus: bool @ 0,
        pub assert_atn: bool @ 1,
        pub assert_sel: bool @ 2,
        pub assert_bsy: bool @ 3,
        pub assert_ack: bool @ 4,
        /// (w) Differential enable
        pub diff_en: bool @ 5,
        /// (r) Arbitration In Progress
        pub aip: bool @ 6,
        pub assert_rst: bool @ 7,
    }
}

/// NCR 5380 SCSI controller
pub struct ScsiController {
    busphase: ScsiBusPhase,
    reg_mr: NcrRegMr,
    reg_icr: NcrRegIcr,

    /// Selected SCSI ID
    sel_id: usize,

    /// Selected with attention
    sel_atn: bool,
}

impl ScsiController {
    pub fn new() -> Self {
        Self {
            busphase: ScsiBusPhase::Free,
            reg_mr: NcrRegMr(0),
            reg_icr: NcrRegIcr(0),
            sel_id: 0,
            sel_atn: false,
        }
    }

    fn set_phase(&mut self, phase: ScsiBusPhase) {
        trace!("Bus phase: {:?}", phase);
        self.busphase = phase;
        match self.busphase {
            ScsiBusPhase::Arbitration => {
                self.reg_icr.set_aip(true);
            }
            ScsiBusPhase::Selection => {
                self.reg_icr.set_aip(false);
            }
            _ => (),
        }
    }
}

impl BusMember<Address> for ScsiController {
    fn read(&mut self, addr: Address) -> Option<u8> {
        let is_write = addr & 1 != 0;
        let dack = addr & 0b0010_0000_0000 != 0;
        let reg = NcrReg::from_u32((addr >> 4) & 0b111).unwrap();

        //if reg != NcrReg::CSR {
        //    trace!(
        //        "SCSI read: write = {}, dack = {}, reg = {:?}",
        //        is_write,
        //        dack,
        //        reg
        //    );
        //}

        match reg {
            NcrReg::ICR => Some(self.reg_icr.0),
            _ => Some(0),
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        let is_write = addr & 1 != 0;
        let dack = addr & 0b0010_0000_0000 != 0;
        let reg = NcrReg::from_u32((addr >> 4) & 0b111).unwrap();

        //trace!(
        //    "SCSI write: val = {:02X}, write = {}, dack = {}, reg = {:?}",
        //    val,
        //    is_write,
        //    dack,
        //    reg
        //);

        match reg {
            NcrReg::CDR_ODR => {
                match self.busphase {
                    ScsiBusPhase::Selection => {
                        self.sel_id = usize::from(val & 0x7F);
                        self.sel_atn = val & 0x80 != 0;
                        trace!(
                            "Selected SCSI ID: {:02X}, attention = {}",
                            self.sel_id,
                            self.sel_atn
                        );
                    }
                    _ => (),
                }
                Some(())
            }
            NcrReg::ICR => {
                self.reg_icr.0 = val;

                match self.busphase {
                    ScsiBusPhase::Arbitration => {
                        if self.reg_icr.assert_sel() {
                            self.set_phase(ScsiBusPhase::Selection);
                        }
                    }

                    _ => (),
                }
                Some(())
            }
            NcrReg::MR => {
                let set = NcrRegMr(val & !self.reg_mr.0);
                self.reg_mr.0 = val;

                if set.arbitrate() {
                    self.set_phase(ScsiBusPhase::Arbitration);
                }

                match self.busphase {
                    ScsiBusPhase::Free => {}
                    _ => (),
                }
                Some(())
            }
            _ => Some(()),
        }
    }
}
