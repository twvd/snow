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

bitfield! {
    /// NCR 5380 SCSI Bus Status
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    struct NcrRegCsr(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        pub dbp: bool @ 0,
        pub sel: bool @ 1,
        pub io: bool @ 2,
        pub cd: bool @ 3,
        pub msg: bool @ 4,
        pub req: bool @ 5,
        pub bsy: bool @ 6,
        pub rst: bool @ 7,
    }
}

/// NCR 5380 SCSI controller
pub struct ScsiController {
    busphase: ScsiBusPhase,
    reg_mr: NcrRegMr,
    reg_icr: NcrRegIcr,
    reg_csr: NcrRegCsr,
    reg_odr: u8,

    /// Selected SCSI ID
    sel_id: usize,

    /// Selected with attention
    sel_atn: bool,

    /// Command buffer
    cmdbuf: Vec<u8>,
}

impl ScsiController {
    pub fn new() -> Self {
        Self {
            busphase: ScsiBusPhase::Free,
            reg_mr: NcrRegMr(0),
            reg_icr: NcrRegIcr(0),
            reg_csr: NcrRegCsr(0),
            reg_odr: 0,
            sel_id: 0,
            sel_atn: false,
            cmdbuf: vec![],
        }
    }

    fn set_phase(&mut self, phase: ScsiBusPhase) {
        trace!("Bus phase: {:?}", phase);

        self.busphase = phase;
        self.reg_csr.0 = 0;

        match self.busphase {
            ScsiBusPhase::Arbitration => {
                self.reg_icr.set_aip(true);
            }
            ScsiBusPhase::Selection => {
                self.reg_icr.set_aip(false);
            }
            ScsiBusPhase::Command => {
                self.cmdbuf.clear();
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(true);
                self.reg_csr.set_req(true);
            }
            ScsiBusPhase::Status => {
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(true);
                self.reg_csr.set_io(true);
                self.reg_csr.set_req(true);
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
            NcrReg::MR => Some(self.reg_mr.0),
            NcrReg::ICR => Some(self.reg_icr.0),
            NcrReg::CSR => Some(self.reg_csr.0),
            NcrReg::BSR => {
                // 'phase match'
                Some(0x08)
            }
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
                self.reg_odr = val;
                Some(())
            }
            NcrReg::ICR => {
                let set = NcrRegIcr(val & !self.reg_icr.0);
                let clr = NcrRegIcr(!val & self.reg_icr.0);

                self.reg_icr.0 = val;

                match self.busphase {
                    ScsiBusPhase::Arbitration => {
                        if set.assert_sel() {
                            self.set_phase(ScsiBusPhase::Selection);
                        }
                    }
                    ScsiBusPhase::Command => {
                        if set.assert_ack() {
                            self.reg_csr.set_req(false);
                        }
                        if clr.assert_ack() {
                            self.cmdbuf.push(self.reg_odr);
                            if self.cmdbuf.len() >= 6 {
                                trace!("cmd: {:X?}", self.cmdbuf);
                                self.set_phase(ScsiBusPhase::Status);
                            } else {
                                self.reg_csr.set_req(true);
                            }
                        }
                    }

                    _ => (),
                }
                Some(())
            }
            NcrReg::MR => {
                let set = NcrRegMr(val & !self.reg_mr.0);
                let clr = NcrRegMr(!val & self.reg_mr.0);
                self.reg_mr.0 = val;

                if set.arbitrate() {
                    self.set_phase(ScsiBusPhase::Arbitration);
                }

                match self.busphase {
                    ScsiBusPhase::Free => {}
                    ScsiBusPhase::Selection => {
                        if clr.arbitrate() {
                            self.sel_id = usize::from(self.reg_odr & 0x7F);
                            self.sel_atn = self.reg_odr & 0x80 != 0;

                            trace!(
                                "Selected SCSI ID: {:02X}, attention = {}",
                                self.sel_id,
                                self.sel_atn
                            );

                            if self.sel_id == 1 {
                                self.set_phase(ScsiBusPhase::Command);
                            }
                        }
                    }
                    _ => (),
                }
                Some(())
            }
            _ => Some(()),
        }
    }
}
