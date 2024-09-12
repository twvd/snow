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
//!
//! ## Target -> Initiator data transfer flow
//! ```mermaid
//! stateDiagram
//!     [*] --> Selection: Initiator selects Target
//!     Selection --> Command: Initiator sends READ (6) Command
//!     Command --> Data: Target enters Data Phase
//!     Data: Data Phase\n(C/D=0, I/O=1, MSG=0, REQ asserted)
//!     Data --> REQ_ACK: REQ/ACK Handshake for Data Transfer
//!     REQ_ACK --> More_Data: Data transfer continues (REQ/ACK Handshake)
//!     More_Data --> REQ_ACK: Next block of data ready on the bus
//!     REQ_ACK --> End_Data: All blocks transferred
//!     End_Data --> Status_Transition: Target changes Phase Signals
//!     Status_Transition --> Status: Status Phase begins (C/D=1, I/O=1, MSG=0)
//!     Status --> REQ_ACK_Status: REQ/ACK Handshake for Status Byte
//!     REQ_ACK_Status --> Message: Status Byte sent, Target enters Message Phase
//!     Message --> REQ_ACK_Message: REQ/ACK Handshake for Message (Usually 0x00)
//!     REQ_ACK_Message --> End: Command complete
//! ```

use std::collections::VecDeque;

use anyhow::Result;
use log::*;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};

pub const STATUS_GOOD: u8 = 0;
pub const STATUS_CHECK_CONDITION: u8 = 2;

pub const DISK_BLOCKS: usize = 41820;
pub const DISK_BLOCKSIZE: usize = 512;

#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
/// SCSI bus phases
enum ScsiBusPhase {
    Free,
    Arbitration,
    Selection,
    Reselection,
    Command,
    /// Target -> Initiator
    DataIn,
    /// Initiator -> Target
    DataOut,
    Status,
    MessageIn,
    MessageOut,
}

/// Result of a command
enum ScsiCmdResult {
    /// Immediately turn to the Status phase
    Status(u8),
    /// Returns data to the initiator
    DataIn(Vec<u8>),
    /// Expects data written to target
    DataOut(usize),
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

        /// Status code
        pub status: u8 @ 0..=2,
    }
}

bitfield! {
    /// NCR 5380 Bus and Status Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    struct NcrRegBsr(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// ACK bus condition
        pub ack: bool @ 0,
        /// ATN bus condition
        pub atn: bool @ 1,
        /// Busy error (loss of BSY condition)
        pub busy_err: bool @ 2,
        /// Phase match
        pub phase_match: bool @ 3,
        /// Interrupt request active
        pub irq: bool @ 4,
        /// Parity error during transfer
        pub parity_err: bool @ 5,
        /// DMA request
        pub dma_req: bool @ 6,
        /// End of DMA transfer
        pub dma_end: bool @ 7,
    }
}

/// NCR 5380 SCSI controller
pub struct ScsiController {
    pub dbg_pc: Address,
    busphase: ScsiBusPhase,
    reg_mr: NcrRegMr,
    reg_icr: NcrRegIcr,
    reg_csr: NcrRegCsr,
    reg_cdr: u8,
    reg_odr: u8,
    reg_bsr: NcrRegBsr,
    status: u8,

    /// Selected SCSI ID
    sel_id: usize,

    /// Selected with attention
    sel_atn: bool,

    /// Command buffer
    cmdbuf: Vec<u8>,

    /// Active command length
    cmdlen: usize,

    /// DataOut phase length
    dataout_len: usize,

    /// Response buffer
    responsebuf: VecDeque<u8>,

    /// Disk
    disk: Vec<u8>,
}

impl ScsiController {
    pub fn new() -> Self {
        let disk = if let Ok(v) = std::fs::read("hdd.bin") {
            assert_eq!(v.len(), DISK_BLOCKS * DISK_BLOCKSIZE);
            v
        } else {
            vec![0; DISK_BLOCKS * DISK_BLOCKSIZE]
        };
        Self {
            dbg_pc: 0,
            busphase: ScsiBusPhase::Free,
            reg_mr: NcrRegMr(0),
            reg_icr: NcrRegIcr(0),
            reg_csr: NcrRegCsr(0),
            reg_bsr: NcrRegBsr(0).with_phase_match(true),
            reg_cdr: 0,
            reg_odr: 0,
            sel_id: 0,
            sel_atn: false,
            cmdbuf: vec![],
            responsebuf: VecDeque::default(),
            cmdlen: 0,
            dataout_len: 0,
            status: 0,
            disk,
        }
    }

    fn set_phase(&mut self, phase: ScsiBusPhase) {
        //trace!("Bus phase: {:?}", phase);

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
                self.responsebuf.clear();
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(true);
                self.reg_csr.set_req(true);
            }
            ScsiBusPhase::DataIn => {
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(false);
                self.reg_csr.set_io(true);
                self.reg_csr.set_req(true);
                //if self.reg_mr.dma_mode() {
                self.reg_bsr.set_dma_req(true);
                //}
            }
            ScsiBusPhase::DataOut => {
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(false);
                self.reg_csr.set_io(false);
                self.reg_csr.set_req(true);
                //if self.reg_mr.dma_mode() {
                self.reg_bsr.set_dma_req(true);
                //}
            }
            ScsiBusPhase::Status => {
                self.reg_csr.set_bsy(true);
                //self.reg_bsr.set_dma_req(false);
                self.reg_csr.set_cd(true);
                self.reg_csr.set_io(true);
                self.reg_csr.set_req(true);
                self.reg_cdr = self.status;
                //trace!("Status code: {:02X}", 0x00);
            }
            ScsiBusPhase::MessageIn => {
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(true);
                self.reg_csr.set_io(true);
                self.reg_csr.set_req(true);
                self.reg_csr.set_msg(true);
                self.reg_cdr = 0;
            }
            _ => (),
        }
    }

    fn cmd_get_len(&self, cmdnum: u8) -> usize {
        match cmdnum {
                // UNIT READY
                0x00
                // REQUEST SENSE
                | 0x03
                // FORMAT UNIT
                | 0x04
                // READ(6)
                | 0x08
                // WRITE(6)
                | 0x0A
                // INQUIRY
                | 0x12
                // MODE SELECT(6)
                | 0x15
                // MODE SENSE(6)
                | 0x1A
                => 6,
                // READ CAPACITY(10)
                0x25
                // READ(10)
                | 0x28
                // WRITE(10)
                | 0x2A
                // READ BUFFER(10)
                | 0x3C
                => 10,
            _ => {
                warn!("cmd_get_len unknown command: {:02X}", cmdnum);
                6
            }
        }
    }

    fn cmd_run(&mut self, outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        let cmd = &self.cmdbuf;

        match cmd[0] {
            0x00 => {
                // UNIT READY
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            0x03 => {
                // REQUEST SENSE
                let result = vec![0; 13];
                // 0 = no error
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x04 => {
                // FORMAT UNIT(6)
                Ok(ScsiCmdResult::Status(STATUS_GOOD))
            }
            0x08 => {
                // READ(6)
                let blocknum = (u32::from_be_bytes(cmd[0..4].try_into()?) & 0x1F_FFFF) as usize;
                let blockcnt = if cmd[4] == 0 { 256 } else { cmd[4] as usize };

                if (blocknum + blockcnt) * DISK_BLOCKSIZE > self.disk.len() {
                    error!("Reading beyond disk");
                    Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                } else {
                    Ok(ScsiCmdResult::DataIn(
                        self.disk
                            [(blocknum * DISK_BLOCKSIZE)..((blocknum + blockcnt) * DISK_BLOCKSIZE)]
                            .to_vec(),
                    ))
                }
            }
            0x0A => {
                // WRITE(6)
                let blocknum = (u32::from_be_bytes(cmd[0..4].try_into()?) & 0x1F_FFFF) as usize;
                let blockcnt = if cmd[4] == 0 { 256 } else { cmd[4] as usize };
                if let Some(data) = outdata {
                    if (blocknum + blockcnt) * DISK_BLOCKSIZE > self.disk.len() {
                        error!("Writing beyond disk");
                        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                    } else {
                        self.disk
                            [(blocknum * DISK_BLOCKSIZE)..((blocknum + blockcnt) * DISK_BLOCKSIZE)]
                            .copy_from_slice(data);
                        std::fs::write("hdd.bin", &self.disk)?;
                        Ok(ScsiCmdResult::Status(STATUS_GOOD))
                    }
                } else {
                    Ok(ScsiCmdResult::DataOut(blockcnt * DISK_BLOCKSIZE))
                }
            }
            0x12 => {
                // INQUIRY
                let mut result = vec![0; 36];

                // 0 Peripheral qualifier (5-7), peripheral device type (4-0)
                result[0] = 0; // Magnetic disk

                // 4 Additional length (N-4), min. 32
                result[4] = result.len() as u8 - 4;

                // 8..16 Vendor identification
                result[8..(8 + 4)].copy_from_slice(b"SNOW");

                // 16..32 Product identification
                result[16..(16 + 11)].copy_from_slice(b"VIRTUAL HDD");
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x15 => {
                // MODE SELECT(6)
                Ok(ScsiCmdResult::DataIn(vec![0; 40]))
            }
            0x1A => {
                // MODE SENSE(6)
                let mut result = vec![0; 36];
                result[0] = 0x30;
                result[1] = 33;

                // The string below has to appear for HD SC Setup and possibly other tools to work.
                // https://68kmla.org/bb/index.php?threads/apple-rom-hard-disks.44920/post-493863
                result[14..(14 + 20)].copy_from_slice(b"APPLE COMPUTER, INC.");
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x25 => {
                // READ CAPACITY(10)
                let mut result = vec![0; 40];
                // Amount of blocks
                result[0..4].copy_from_slice(&((DISK_BLOCKS as u32) - 1).to_be_bytes());
                // Block size
                result[4..8].copy_from_slice(&(DISK_BLOCKSIZE as u32).to_be_bytes());
                Ok(ScsiCmdResult::DataIn(result))
            }
            0x28 => {
                // READ(10)
                let blocknum = (u32::from_be_bytes(cmd[2..6].try_into()?)) as usize;
                let blockcnt = (u16::from_be_bytes(cmd[7..9].try_into()?)) as usize;

                if (blocknum + blockcnt) * DISK_BLOCKSIZE > self.disk.len() {
                    error!("Reading beyond disk");
                    Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                } else {
                    Ok(ScsiCmdResult::DataIn(
                        self.disk
                            [(blocknum * DISK_BLOCKSIZE)..((blocknum + blockcnt) * DISK_BLOCKSIZE)]
                            .to_vec(),
                    ))
                }
            }
            0x2A => {
                // WRITE(10)
                let blocknum = (u32::from_be_bytes(cmd[2..6].try_into()?)) as usize;
                let blockcnt = (u16::from_be_bytes(cmd[7..9].try_into()?)) as usize;

                if let Some(data) = outdata {
                    if (blocknum + blockcnt) * DISK_BLOCKSIZE > self.disk.len() {
                        error!("Writing beyond disk");
                        Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
                    } else {
                        self.disk
                            [(blocknum * DISK_BLOCKSIZE)..((blocknum + blockcnt) * DISK_BLOCKSIZE)]
                            .copy_from_slice(data);
                        std::fs::write("hdd.bin", &self.disk)?;
                        Ok(ScsiCmdResult::Status(STATUS_GOOD))
                    }
                } else {
                    Ok(ScsiCmdResult::DataOut(blockcnt * DISK_BLOCKSIZE))
                }
            }
            0x3C => {
                // READ BUFFER(10)
                let result = vec![0; 4];
                // 0 reserved (0)
                // 1-3 buffer length (0)
                Ok(ScsiCmdResult::DataIn(result))
            }
            _ => {
                error!("Unknown command {:02X}", cmd[0]);
                Ok(ScsiCmdResult::Status(STATUS_CHECK_CONDITION))
            }
        }
    }
}

impl BusMember<Address> for ScsiController {
    fn read(&mut self, addr: Address) -> Option<u8> {
        let _is_write = addr & 1 != 0;
        let _dack = addr & 0b0010_0000_0000 != 0;
        let reg = NcrReg::from_u32((addr >> 4) & 0b111).unwrap();

        //if reg != NcrReg::CSR {
        //trace!(
        //    "{:06X} SCSI read: write = {}, dack = {}, reg = {:?}",
        //    self.dbg_pc,
        //    is_write,
        //    dack,
        //    reg
        //);
        //}

        match reg {
            NcrReg::CDR_ODR => {
                if self.busphase != ScsiBusPhase::DataIn {
                    Some(self.reg_cdr)
                } else if self.reg_mr.dma_mode() {
                    if let Some(b) = self.responsebuf.pop_front() {
                        if self.responsebuf.is_empty() {
                            // Transfer completed
                            self.set_phase(ScsiBusPhase::Status);
                        }
                        Some(b)
                    } else {
                        error!("CDR DMA buffer underrun!");
                        Some(0)
                    }
                } else {
                    error!("CDR read but DMA disabled");
                    Some(0)
                }
            }
            NcrReg::MR => Some(self.reg_mr.0),
            NcrReg::ICR => Some(self.reg_icr.0),
            NcrReg::CSR => Some(self.reg_csr.0),
            NcrReg::BSR => Some(self.reg_bsr.0),
            _ => Some(0),
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        let _is_write = addr & 1 != 0;
        let _dack = addr & 0b0010_0000_0000 != 0;
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
                if self.busphase == ScsiBusPhase::DataOut {
                    self.responsebuf.push_back(val);
                    self.dataout_len -= 1;
                    if self.dataout_len == 0 {
                        // TODO inefficient
                        let datavec = Vec::from_iter(self.responsebuf.iter().cloned());
                        if let Ok(ScsiCmdResult::Status(s)) = self.cmd_run(Some(&datavec)) {
                            self.status = s;
                            self.set_phase(ScsiBusPhase::Status);
                        } else {
                            todo!();
                        }
                    }
                    //}
                }
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
                            if self.cmdbuf.is_empty() {
                                self.cmdlen = self.cmd_get_len(self.reg_odr);
                            }
                            self.cmdbuf.push(self.reg_odr);
                            if self.cmdbuf.len() >= self.cmdlen {
                                //trace!("cmd: {:X?}", self.cmdbuf);

                                match self.cmd_run(None) {
                                    Ok(ScsiCmdResult::Status(status)) => {
                                        self.status = status;
                                        self.set_phase(ScsiBusPhase::Status);
                                    }
                                    Ok(ScsiCmdResult::DataIn(data)) => {
                                        self.status = STATUS_GOOD;

                                        // TODO this is inefficient
                                        self.responsebuf = VecDeque::from(data);

                                        self.set_phase(ScsiBusPhase::DataIn);
                                    }
                                    Ok(ScsiCmdResult::DataOut(len)) => {
                                        self.dataout_len = len;
                                        self.responsebuf.clear();
                                        self.set_phase(ScsiBusPhase::DataOut);
                                    }
                                    Err(e) => {
                                        error!(
                                            "SCSI command ({:02X}) error: {}",
                                            self.cmdbuf[0], e
                                        );
                                    }
                                }
                            } else {
                                self.reg_csr.set_req(true);
                            }
                        }
                    }
                    ScsiBusPhase::Status => {
                        if set.assert_ack() {
                            self.reg_csr.set_req(false);
                        }
                        if clr.assert_ack() {
                            self.set_phase(ScsiBusPhase::MessageIn);
                        }
                    }
                    ScsiBusPhase::MessageIn => {
                        if set.assert_ack() {
                            self.reg_csr.set_req(false);
                        }
                        if clr.assert_ack() {
                            self.set_phase(ScsiBusPhase::Free);
                        }
                    }
                    ScsiBusPhase::DataIn => {
                        if set.assert_ack() {
                            self.reg_csr.set_req(false);
                        }
                        if clr.assert_ack() && !self.responsebuf.is_empty() {
                            self.reg_csr.set_req(true);
                        }
                    }
                    ScsiBusPhase::DataOut => {
                        if set.assert_ack() {
                            self.reg_csr.set_req(false);
                        }
                        if clr.assert_ack() && self.dataout_len > 0 {
                            self.reg_csr.set_req(true);
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
                    self.reg_cdr = self.reg_odr; // Initiator ID
                }

                match self.busphase {
                    ScsiBusPhase::Free => {}
                    ScsiBusPhase::Selection => {
                        if clr.arbitrate() {
                            self.sel_id = usize::from(self.reg_odr & 0x7F);
                            self.sel_atn = self.reg_odr & 0x80 != 0;

                            //trace!(
                            //    "Selected SCSI ID: {:02X}, attention = {}",
                            //    self.sel_id,
                            //    self.sel_atn
                            //);

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