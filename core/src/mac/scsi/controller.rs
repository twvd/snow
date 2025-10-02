//! NCR5380 SCSI controller

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use num_derive::FromPrimitive;
use num_derive::ToPrimitive;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::bus::{Address, BusMember};
use crate::dbgprop_byte;
use crate::debuggable::Debuggable;
use crate::mac::scsi::cdrom::ScsiTargetCdrom;
use crate::mac::scsi::disk::ScsiTargetDisk;
use crate::mac::scsi::scsi_cmd_len;
use crate::mac::scsi::target::ScsiTarget;
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::scsi::toolbox::BlueSCSI;
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::STATUS_GOOD;
use crate::tickable::{Tickable, Ticks};

const SCSI_TRACE: bool = false;

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq, strum::IntoStaticStr, Serialize, Deserialize)]
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

#[allow(non_camel_case_types)]
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, PartialEq, Eq, Clone, Copy, FromPrimitive, ToPrimitive)]
enum NcrReg {
    /// Current Data Register / Output Data Register (0)
    CDR_ODR,
    /// Initiator Command Register (10)
    ICR,
    /// Mode Register (20)
    MR,
    /// Target Command Register (30)
    TCR,
    /// Current SCSI bus status (40)
    CSR,
    /// Bus and Status register (50)
    BSR,
    /// Input Data Register (60)
    IDR,
    /// Reset parity/interrupt (read) / Start DMA transfer (write) (70)
    RESET,
}

bitfield! {
    /// NCR 5380 Mode Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    struct NcrRegMr(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub arbitrate: bool @ 0,
        pub dma_mode: bool @ 1,
        pub monitor_busy: bool @ 2,
        pub eop_irq: bool @ 3,
        pub parity_irq: bool @ 4,
        pub parity_check: bool @ 5,
        pub target_mode: bool @ 6,
        pub block_dma: bool @ 7,
    }
}

bitfield! {
    /// NCR 5380 Initiator Control Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    struct NcrRegIcr(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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
    struct NcrRegCsr(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub phase_match_bits: u8 @ 2..=4,

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
    struct NcrRegBsr(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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

bitfield! {
    /// NCR 5380 Target Control Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    struct NcrRegTcr(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub phase_match_bits: u8 @ 0..=2,

        pub assert_io: bool @ 0,
        pub assert_cd: bool @ 1,
        pub assert_msg: bool @ 2,
        pub assert_req: bool @ 3,

        // 53C80 only
        pub last_byte_sent: bool @ 7,
    }
}

/// NCR 5380 SCSI controller
#[derive(Serialize, Deserialize)]
pub struct ScsiController {
    busphase: ScsiBusPhase,
    reg_mr: NcrRegMr,
    reg_icr: NcrRegIcr,
    reg_csr: NcrRegCsr,
    reg_cdr: u8,
    reg_odr: u8,
    reg_bsr: NcrRegBsr,
    reg_tcr: NcrRegTcr,
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

    /// Attached targets
    #[serde(with = "BigArray")]
    pub(crate) targets: [Option<Box<dyn ScsiTarget>>; Self::MAX_TARGETS],

    #[serde(skip)]
    toolbox: BlueSCSI,
    scsi_debug: bool,

    /// Delays response to ACK de-assert to avoid race conditions mostly
    /// in the Mac II ROM
    deassert_ack_delay: Ticks,
}

impl ScsiController {
    pub const MAX_TARGETS: usize = 7;

    pub fn get_irq(&self) -> bool {
        self.reg_bsr.irq()
    }

    /// Returns the capacity of a target or None if detached or no media
    pub fn get_disk_capacity(&self, id: usize) -> Option<usize> {
        self.targets[id].as_ref().and_then(|t| t.capacity())
    }

    /// Returns the image filename of a target or None if detached or no media
    pub fn get_disk_imagefn(&self, id: usize) -> Option<&Path> {
        self.targets[id].as_ref().and_then(|t| t.image_fn())
    }

    /// Gets the target type (if attached) of an ID
    pub fn get_target_type(&self, id: usize) -> Option<ScsiTargetType> {
        self.targets[id].as_ref().map(|t| t.target_type())
    }

    pub fn set_shared_dir(&mut self, path: Option<PathBuf>) {
        self.toolbox = BlueSCSI::new(path);
    }

    pub fn new() -> Self {
        Self {
            busphase: ScsiBusPhase::Free,
            reg_mr: NcrRegMr(0),
            reg_icr: NcrRegIcr(0),
            reg_csr: NcrRegCsr(0),
            reg_bsr: NcrRegBsr(0),
            reg_cdr: 0,
            reg_odr: 0,
            reg_tcr: NcrRegTcr(0),
            sel_id: 0,
            sel_atn: false,
            cmdbuf: vec![],
            responsebuf: VecDeque::default(),
            cmdlen: 0,
            dataout_len: 0,
            status: 0,
            targets: Default::default(),
            toolbox: BlueSCSI::default(),
            scsi_debug: false,
            deassert_ack_delay: 0,
        }
    }

    /// Loads a disk image (filename) and attaches a hard drive at the given SCSI ID
    pub fn attach_hdd_at(&mut self, filename: &Path, scsi_id: usize) -> Result<()> {
        if scsi_id >= Self::MAX_TARGETS {
            bail!("SCSI ID out of range: {}", scsi_id);
        }
        if !Path::new(filename).exists() {
            bail!("File {} does not exist", filename.to_string_lossy());
        }
        self.targets[scsi_id] = Some(Box::new(ScsiTargetDisk::load_disk(filename)?));
        Ok(())
    }

    /// Attaches a CD-ROM drive at the given SCSI ID
    pub fn attach_cdrom_at(&mut self, scsi_id: usize) {
        self.targets[scsi_id] = Some(Box::new(ScsiTargetCdrom::default()));
    }

    /// Detaches a target from the given SCSI ID
    pub fn detach_target(&mut self, scsi_id: usize) {
        self.targets[scsi_id] = None;
    }

    /// Translates a SCSI ID on the bus (bit position) to a numeric ID
    fn translate_id(mut bitp: u8) -> Result<usize> {
        if bitp.count_ones() != 1 {
            bail!("Invalid ID on bus: {:02X}", bitp);
        }
        for id in 0..8 {
            bitp >>= 1;
            if bitp == 0 {
                return Ok(id);
            }
        }
        unreachable!()
    }

    /// Asserts the REQ line
    fn assert_req(&mut self) {
        self.reg_csr.set_req(true);
    }

    /// De-asserts the REQ line
    fn deassert_req(&mut self) {
        self.reg_csr.set_req(false);
    }

    fn set_phase(&mut self, phase: ScsiBusPhase) {
        if SCSI_TRACE {
            log::debug!("Bus phase: {:?}", phase);
        }

        self.busphase = phase;
        self.reg_csr.0 = 0;
        self.deassert_ack_delay = 0;

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
                self.reg_csr.set_msg(false);
                self.assert_req();
            }
            ScsiBusPhase::DataIn => {
                if self.responsebuf.is_empty() {
                    return self.set_phase(ScsiBusPhase::Status);
                }
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(false);
                self.reg_csr.set_io(true);
                self.reg_csr.set_msg(false);
                self.reg_cdr = self.responsebuf.pop_front().unwrap();

                self.assert_req();
            }
            ScsiBusPhase::DataOut => {
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(false);
                self.reg_csr.set_io(false);
                self.reg_csr.set_msg(false);

                self.assert_req();
            }
            ScsiBusPhase::Status => {
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(true);
                self.reg_csr.set_io(true);

                self.reg_csr.set_msg(false);
                self.reg_cdr = self.status;

                self.assert_req();
            }
            ScsiBusPhase::MessageIn => {
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(true);
                self.reg_csr.set_io(true);
                self.reg_csr.set_msg(true);
                self.reg_cdr = 0;

                self.assert_req();
            }
            ScsiBusPhase::Free => {
                assert!(!self.reg_csr.req());
            }
            _ => (),
        }
    }

    fn cmd_run(&mut self, outdata: Option<&[u8]>) -> Result<()> {
        let cmd = &self.cmdbuf;
        if SCSI_TRACE {
            log::debug!("SCSI ID {} command: {:02X?}", self.sel_id, cmd);
        }

        let result = match cmd[0] {
            0xD0..=0xD9 => Ok(self
                .toolbox
                .handle_command(cmd, outdata, &mut self.scsi_debug)),
            _ => {
                let Some(target) = self.targets[self.sel_id].as_mut() else {
                    bail!("SCSI command to disconnected target ID {}", self.sel_id);
                };
                target.cmd(cmd, outdata)
            }
        };

        match result {
            Ok(ScsiCmdResult::Status(s)) => {
                self.status = s;

                self.set_phase(ScsiBusPhase::Status);
            }
            Ok(ScsiCmdResult::DataIn(data)) => {
                self.status = STATUS_GOOD;
                self.responsebuf = VecDeque::from(data);
                self.set_phase(ScsiBusPhase::DataIn);
            }
            Ok(ScsiCmdResult::DataOut(len)) => {
                self.dataout_len = len;
                self.responsebuf.clear();
                self.set_phase(ScsiBusPhase::DataOut);

                if self.dataout_len == 0 {
                    // Legal according to spec
                    return self.cmd_run(Some(&[]));
                }
            }
            Err(e) => return Err(e),
        }

        Ok(())
    }

    pub fn get_drq(&self) -> bool {
        self.reg_mr.dma_mode() && (self.reg_csr.req() || self.deassert_ack_delay > 0)
    }

    pub fn read_dma(&mut self) -> u8 {
        self.read_datareg()
    }

    pub fn write_dma(&mut self, val: u8) {
        self.write_datareg(val);
    }

    fn write_datareg(&mut self, val: u8) {
        if self.deassert_ack_delay > 0 {
            self.deassert_ack_real();
            self.deassert_ack_delay = 0;
        }

        if SCSI_TRACE {
            log::debug!(
                "Write data register: {:02X} dma = {}",
                val,
                self.reg_mr.dma_mode()
            );
        }
        self.reg_odr = val;

        if self.reg_mr.dma_mode() {
            self.assert_ack();
            self.deassert_ack();
        }
    }

    fn read_datareg(&mut self) -> u8 {
        if self.deassert_ack_delay > 0 {
            self.deassert_ack_real();
            self.deassert_ack_delay = 0;
        }

        let val = self.reg_cdr;

        if self.reg_mr.dma_mode() {
            self.assert_ack();
            self.deassert_ack();
        }
        val
    }

    fn assert_ack(&mut self) {
        self.deassert_req();

        match self.busphase {
            ScsiBusPhase::DataOut => {
                let val = self.reg_odr;
                self.responsebuf.push_back(val);
                self.dataout_len -= 1;
            }
            _ => (),
        }
    }

    fn deassert_ack(&mut self) {
        self.deassert_ack_delay = 5000;
    }

    fn deassert_ack_real(&mut self) {
        match self.busphase {
            ScsiBusPhase::Command => {
                let val = self.reg_odr;
                if self.cmdbuf.is_empty() {
                    self.cmdlen = scsi_cmd_len(val).unwrap_or_else(|| {
                        log::error!("Cmd length unknown for {:02X}", val);
                        6
                    });
                }
                self.cmdbuf.push(val);
                if self.cmdbuf.len() >= self.cmdlen {
                    // Command complete, execute it

                    if let Err(e) = self.cmd_run(None) {
                        log::error!("SCSI command ({:02X}) error: {}", self.cmdbuf[0], e);
                    }
                } else {
                    self.assert_req();
                }
            }
            ScsiBusPhase::DataIn => {
                // Pump next byte to CDR for next read
                if let Some(b) = self.responsebuf.pop_front() {
                    self.reg_cdr = b;
                    self.assert_req();
                } else {
                    // Transfer completed
                    self.set_phase(ScsiBusPhase::Status);
                    if self.reg_mr.dma_mode() {
                        self.reg_bsr.set_dma_end(true);
                        if self.reg_mr.eop_irq() {
                            self.reg_bsr.set_irq(true);
                        }
                    }
                }
            }
            ScsiBusPhase::DataOut => {
                if self.dataout_len == 0 {
                    // TODO inefficient
                    let datavec = Vec::from_iter(self.responsebuf.iter().cloned());
                    if let Err(e) = self.cmd_run(Some(&datavec)) {
                        log::error!("SCSI command run error: {:#}", e);
                    }
                    if self.reg_mr.dma_mode() {
                        self.reg_bsr.set_dma_end(true);
                        if self.reg_mr.eop_irq() {
                            self.reg_bsr.set_irq(true);
                        }
                    }
                } else {
                    self.assert_req();
                }
            }
            ScsiBusPhase::Status => {
                self.set_phase(ScsiBusPhase::MessageIn);
            }
            ScsiBusPhase::MessageIn => {
                self.set_phase(ScsiBusPhase::Free);
            }
            _ => {}
        }
    }

    fn phase_match(&self) -> bool {
        self.reg_csr.phase_match_bits() == self.reg_tcr.phase_match_bits()
    }
}

impl BusMember<Address> for ScsiController {
    fn read(&mut self, addr: Address) -> Option<u8> {
        let is_write = addr & 1 != 0;
        let dack = addr & 0b0010_0000_0000 != 0;
        let reg = NcrReg::from_u32((addr >> 4) & 0b111).unwrap();

        let val = match reg {
            NcrReg::CDR_ODR | NcrReg::IDR => Some(self.read_datareg()),
            NcrReg::MR => Some(self.reg_mr.0),
            NcrReg::ICR => Some(self.reg_icr.0),
            NcrReg::CSR => {
                let val = self.reg_csr.0;
                Some(val)
            }
            NcrReg::BSR => {
                let val = self
                    .reg_bsr
                    .with_dma_req(self.get_drq())
                    .with_phase_match(self.phase_match())
                    .0;

                if self.deassert_ack_delay > 0 {
                    self.deassert_ack_real();
                    self.deassert_ack_delay = 0;
                }
                Some(val)
            }
            NcrReg::RESET => {
                self.reg_bsr.set_irq(false);
                Some(0)
            }
            NcrReg::TCR => Some(self.reg_tcr.with_last_byte_sent(self.reg_bsr.dma_end()).0),
        };

        if SCSI_TRACE && reg != NcrReg::CSR && reg != NcrReg::BSR {
            log::debug!(
                "SCSI read: write = {}, dack = {}, reg = {:?}, value = {:02X?}",
                is_write,
                dack,
                reg,
                val
            );
        }

        val
    }

    #[allow(clippy::cognitive_complexity)]
    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        let is_write = addr & 1 != 0;
        let dack = addr & 0b0010_0000_0000 != 0;
        let reg = NcrReg::from_u32((addr >> 4) & 0b111).unwrap();

        if SCSI_TRACE {
            log::debug!(
                "SCSI write: val = {:02X}, write = {}, dack = {}, reg = {:?}",
                val,
                is_write,
                dack,
                reg
            );
        }

        match reg {
            NcrReg::CDR_ODR => {
                self.write_datareg(val);
                Some(())
            }
            NcrReg::ICR => {
                let set = NcrRegIcr(val & !self.reg_icr.0);
                let clr = NcrRegIcr(!val & self.reg_icr.0);

                self.reg_icr.0 = val;

                if set.assert_ack() {
                    self.assert_ack();
                }
                if clr.assert_ack() {
                    self.deassert_ack();
                }

                match self.busphase {
                    ScsiBusPhase::Arbitration => {
                        if set.assert_sel() {
                            self.reg_bsr.set_irq(true);
                            self.set_phase(ScsiBusPhase::Selection);
                        }
                    }
                    ScsiBusPhase::Selection => {
                        if set.assert_databus() {
                            let Ok(id) = Self::translate_id(self.reg_odr & 0x7F) else {
                                log::error!("Invalid ID on bus! ODR = {:02X}", self.reg_odr);
                                self.set_phase(ScsiBusPhase::Free);
                                return Some(());
                            };
                            if self.targets[id].is_none() {
                                // No device present at this ID
                                if SCSI_TRACE {
                                    log::debug!("Not selecting {},  no device", id);
                                }
                                self.set_phase(ScsiBusPhase::Free);
                                return Some(());
                            }

                            // Select this ID
                            self.sel_id = id;
                            self.sel_atn = self.reg_odr & 0x80 != 0;

                            if SCSI_TRACE {
                                log::debug!(
                                    "Selected SCSI ID: {:02X}, attention = {}",
                                    self.sel_id,
                                    self.sel_atn
                                );
                            }

                            self.set_phase(ScsiBusPhase::Command);
                        }
                    }
                    ScsiBusPhase::Command => {}

                    ScsiBusPhase::MessageIn => {}
                    ScsiBusPhase::DataIn => {}
                    ScsiBusPhase::DataOut => if clr.assert_ack() {},

                    _ => (),
                }
                Some(())
            }
            NcrReg::MR => {
                let set = NcrRegMr(val & !self.reg_mr.0);
                let clr = NcrRegMr(!val & self.reg_mr.0);
                self.reg_mr.0 = val;

                if set.arbitrate() {
                    // Initiate arbitration
                    self.set_phase(ScsiBusPhase::Arbitration);
                    self.reg_cdr = self.reg_odr; // Initiator ID
                    return Some(());
                }

                if clr.dma_mode() {
                    self.reg_bsr.set_dma_end(false);
                }
                Some(())
            }
            NcrReg::RESET => {
                // Start DMA transfer
                if self.deassert_ack_delay > 0 {
                    self.deassert_ack_real();
                    self.deassert_ack_delay = 0;
                }
                if !self.phase_match() {
                    log::warn!(
                        "SCSI phase mismatch: {:03b} (actual) <-> {:03b} (expected)",
                        self.reg_csr.phase_match_bits(),
                        self.reg_tcr.phase_match_bits(),
                    );
                    self.reg_bsr.set_phase_match(false);
                    self.reg_bsr.set_irq(true);
                }
                Some(())
            }
            NcrReg::TCR => {
                self.reg_tcr.0 = val;
                Some(())
            }
            _ => {
                //warn!("Unknown SCSI register write: {:?} = {:02X}", reg, val);
                Some(())
            }
        }
    }
}

impl Tickable for ScsiController {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        if self.deassert_ack_delay > 0 {
            self.deassert_ack_delay -= 1;
            if self.deassert_ack_delay == 0 {
                self.deassert_ack_real();
            }
        }
        Ok(ticks)
    }
}

impl Debuggable for ScsiController {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{
            dbgprop_bool, dbgprop_enum, dbgprop_group, dbgprop_header, dbgprop_string, dbgprop_udec,
        };

        vec![
            dbgprop_group!(
                "Registers",
                vec![
                    dbgprop_byte!("MR", self.reg_mr.0),
                    dbgprop_byte!("ICR", self.reg_icr.0),
                    dbgprop_byte!("CSR", self.reg_csr.0),
                    dbgprop_byte!("CDR", self.reg_cdr),
                    dbgprop_byte!("ODR", self.reg_odr),
                    dbgprop_byte!("BSR", self.reg_bsr.0),
                    dbgprop_byte!("Status", self.status),
                ]
            ),
            dbgprop_enum!("Bus phase", self.busphase),
            dbgprop_udec!("Selected ID", self.sel_id),
            dbgprop_bool!("Attention", self.sel_atn),
            dbgprop_header!("Buffers"),
            dbgprop_string!("Command", format!("{:02X?}", self.cmdbuf)),
            dbgprop_udec!("Command buffer len", self.cmdbuf.len()),
            dbgprop_udec!("Command length", self.cmdlen),
            dbgprop_udec!("Response buffer len", self.responsebuf.len()),
            dbgprop_udec!("Data out len", self.dataout_len),
            dbgprop_bool!("IRQ", self.get_irq()),
            dbgprop_bool!("DRQ", self.get_drq()),
        ]
    }
}
