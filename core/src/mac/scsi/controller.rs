//! NCR5380 SCSI controller

use anyhow::{Result, bail};

use log::*;
use num_derive::FromPrimitive;
use num_derive::ToPrimitive;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};
use crate::dbgprop_byte;
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::mac::scsi::bus::{CmdOutcome, ScsiBus};
use crate::mac::scsi::scsi_cmd_len;
use crate::tickable::{Tickable, Ticks};
use crate::types::LatchingEvent;

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

/// NCR 5380 readable registers
#[allow(non_camel_case_types)]
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, PartialEq, Eq, Clone, Copy, FromPrimitive, ToPrimitive)]
enum NcrReadReg {
    /// Current Data Register (0)
    CDR,
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
    /// Reset parity/interrupt (70)
    RESET,
}

// NCR 5380 writable registers
#[allow(non_camel_case_types)]
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, PartialEq, Eq, Clone, Copy, FromPrimitive, ToPrimitive)]
enum NcrWriteReg {
    /// Output Data Register (0)
    ODR,
    /// Initiator Command Register (10)
    ICR,
    /// Mode Register (20)
    MR,
    /// Target Command Register (30)
    TCR,
    /// Select Enable register (40)
    SELEN,
    /// Start DMA send (50)
    StartDMASend,
    /// Start DMA target receive (60)
    StartDMATargetReceive,
    /// Start DMA initiator receive (70)
    StartDMAInitiatorReceive,
}

bitfield! {
    /// NCR 5380 Mode Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    struct NcrRegMr(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
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
        pub writable: u8 @ 0..=6,
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
    reg_tcr: NcrRegTcr,
    reg_cdr: u8,
    reg_odr: u8,
    reg_bsr: NcrRegBsr,
    reg_selen: u8,

    /// DMA has been armed (Start DMA Send / Target Receive / Initiator
    /// Receive register written). Gates DRQ and the phase-mismatch IRQ
    /// per the 5380 datasheet: phase match is a polled flag, but it only
    /// generates an interrupt while DMA mode is active and a DMA
    /// direction has been armed.
    dma_armed: bool,

    /// Selected with attention
    sel_atn: bool,

    /// The bus and its targets
    pub(crate) bus: ScsiBus,

    set_req: LatchingEvent,

    #[serde(skip)]
    scsi_trace_phase: bool,
    #[serde(skip)]
    scsi_trace_irq: bool,
}

impl ScsiController {
    pub fn get_irq(&self) -> bool {
        self.reg_bsr.irq()
    }

    pub fn new() -> Self {
        let env_flag = |name: &str| {
            std::env::var(name)
                .map(|v| v != "0" && !v.is_empty())
                .unwrap_or(false)
        };
        let scsi_trace_phase = env_flag("SNOW_SCSI_TRACE_PHASE");
        let scsi_trace_irq = env_flag("SNOW_SCSI_TRACE_IRQ");
        Self {
            busphase: ScsiBusPhase::Free,
            reg_mr: NcrRegMr(0),
            reg_icr: NcrRegIcr(0),
            reg_csr: NcrRegCsr(0),
            reg_tcr: NcrRegTcr(0),
            reg_bsr: NcrRegBsr(0),
            reg_cdr: 0,
            reg_odr: 0,
            reg_selen: 0,
            dma_armed: false,
            sel_atn: false,
            bus: ScsiBus::new(),
            set_req: Default::default(),
            scsi_trace_phase,
            scsi_trace_irq,
        }
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

    /// Asserts the REQ line (delayed)
    fn assert_req(&mut self) {
        // MacII has a race condition where it will get stuck if
        // REQ is immediately set on a Data -> Status transition.
        self.reg_csr.set_req(false);
        self.set_req.set();
    }

    /// De-asserts the REQ line
    fn deassert_req(&mut self) {
        self.reg_csr.set_req(false);
        self.set_req.get_clear();
    }

    /// Attempts to complete selection (sample target ID) if the bus state is
    /// valid (MR.arbitrate=0, ICR.assert_sel=1, exactly one non-initiator ID
    /// bit on ODR). Different drivers (MacOS vs A/UX) drive the 5380 in
    /// different orders, so instead of latching the target on a single
    /// register-write event we re-check after each relevant Selection-phase
    /// write.
    fn try_complete_selection(&mut self) {
        if self.busphase != ScsiBusPhase::Selection {
            return;
        }
        if self.reg_mr.arbitrate() || !self.reg_icr.assert_sel() {
            return;
        }
        let target_bits = self.reg_odr & 0x7F;
        if target_bits.count_ones() != 1 {
            // Initiator hasn't placed the target ID on the bus yet. Stay in
            // Selection and wait for the next write.
            return;
        }
        let id = Self::translate_id(target_bits).unwrap();
        if self.bus.targets[id].is_none() {
            // No device present at this ID
            self.set_phase(ScsiBusPhase::Free);
            return;
        }

        // Selection interrupt
        if self.reg_selen == self.reg_odr {
            if self.scsi_trace_irq {
                debug!("SCSI IRQ raised (selection complete, id={})", id);
            }
            self.reg_bsr.set_irq(true);
        }
        self.bus.sel_id = id;
        self.sel_atn = self.reg_icr.assert_atn();
        self.set_phase(ScsiBusPhase::Command);
    }

    fn set_phase(&mut self, phase: ScsiBusPhase) {
        if self.scsi_trace_phase {
            debug!(
                "SCSI phase: {:?} -> {:?} (id={}, atn={})",
                self.busphase, phase, self.bus.sel_id, self.sel_atn
            );
        }

        let prev_phase_match = self.phase_match();

        self.busphase = phase;
        self.reg_csr.0 = 0;
        self.deassert_req();

        match self.busphase {
            ScsiBusPhase::Arbitration => {
                self.reg_icr.set_aip(true);
            }
            ScsiBusPhase::Selection => {
                self.reg_icr.set_aip(false);
            }
            ScsiBusPhase::Command => {
                self.bus.cmdbuf.clear();
                self.bus.responsebuf.clear();
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(true);
                self.reg_csr.set_msg(false);
                self.assert_req();
            }
            ScsiBusPhase::DataIn => {
                if self.bus.responsebuf.is_empty() {
                    return self.set_phase(ScsiBusPhase::Status);
                }
                self.reg_csr.set_bsy(true);
                self.reg_csr.set_cd(false);
                self.reg_csr.set_io(true);
                self.reg_csr.set_msg(false);
                self.reg_cdr = self.bus.responsebuf.pop_front().unwrap();

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
                self.reg_cdr = self.bus.status;

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
            _ => (),
        }

        // NCR 5380 phase-mismatch interrupt: while DMA mode is active and
        // a DMA direction has been armed, a high->low transition of the
        // phase-match signal raises IRQ. Drivers use this edge to detect
        // that a pseudo-DMA transfer has ended (target changed phase).
        if self.reg_mr.dma_mode() && self.dma_armed && prev_phase_match && !self.phase_match() {
            if self.scsi_trace_irq {
                debug!("SCSI IRQ raised (DMA phase mismatch)");
            }
            self.reg_bsr.set_irq(true);
            self.dma_armed = false;
        }
    }

    /// Runs the command in the buffer and moves the bus to the phase the
    /// target asked for.
    fn cmd_run(&mut self, outdata: Option<&[u8]>) -> Result<()> {
        let outcome = self.bus.cmd_run(outdata)?;
        self.enter_outcome(outcome);
        Ok(())
    }

    /// Moves the bus to the phase the target asked for
    fn enter_outcome(&mut self, outcome: CmdOutcome) {
        match outcome {
            CmdOutcome::Status => self.set_phase(ScsiBusPhase::Status),
            CmdOutcome::DataIn => self.set_phase(ScsiBusPhase::DataIn),
            CmdOutcome::DataOut(_) => self.set_phase(ScsiBusPhase::DataOut),
            // The target holds the bus in the current phase without
            // requesting anything until the command completes (tick()).
            CmdOutcome::Busy => self.deassert_req(),
        }
    }

    /// The bus and its targets
    pub fn bus(&self) -> &ScsiBus {
        &self.bus
    }

    /// The bus and its targets
    pub fn bus_mut(&mut self) -> &mut ScsiBus {
        &mut self.bus
    }

    /// DRQ only occurs in DMA mode (NCR 5380 manual, DRQ pin). A phase
    /// mismatch prevents the recognition of REQ (manual 8.5), except in a
    /// send, where the chip leaves the bus: DRQ then lets the pending
    /// writes complete (as MAME does). A/UX relies on it: it writes its
    /// CDBs with long words, two bytes past the end of the command.
    pub fn get_drq(&self) -> bool {
        self.reg_mr.dma_mode()
            && (self.reg_csr.req() || self.set_req.peek())
            && (self.phase_match() || !self.reg_tcr.assert_io())
    }

    pub fn read_dma(&mut self) -> u8 {
        // Note that System 7.1 during bulk transfers will read blocks of 512
        // bytes at a time from the DMA region and then use PIO momentarily
        // for some reason.
        self.read_datareg()
    }

    pub fn write_dma(&mut self, val: u8) {
        self.write_datareg(val);
    }

    fn write_datareg(&mut self, val: u8) {
        self.reg_odr = val;

        // Pseudo-DMA path: writes to the DMA window auto-pulse ACK, so we
        // advance the REQ/ACK handshake here instead of waiting for an
        // explicit ICR ACK toggle. Until a DMA send is started, the write
        // only loads the register.
        if self.dma_armed && matches!(self.busphase, ScsiBusPhase::DataOut | ScsiBusPhase::Command)
        {
            self.assert_ack();
            self.deassert_ack();
        }
    }

    fn read_datareg(&mut self) -> u8 {
        let val = self.reg_cdr;
        // A DACK read. The manual has the Start DMA Initiator Receive
        // write initiate the receive, but the handshake doesn't wait for
        // it here: in DATA IN, A/UX on the IIcx sets DMA mode, polls DRQ
        // and reads the handshake area without writing that register
        // (Mac OS always writes it). A/UX works on real hardware, so the
        // chip presumably handshakes in DMA mode alone; not verified.
        if self.reg_mr.dma_mode() && self.phase_match() {
            self.assert_ack();
            self.deassert_ack();
        }
        val
    }

    fn assert_ack(&mut self) {
        match self.busphase {
            ScsiBusPhase::Command
            | ScsiBusPhase::DataOut
            | ScsiBusPhase::Status
            | ScsiBusPhase::MessageIn
            | ScsiBusPhase::DataIn => {
                self.deassert_req();
            }
            _ => {}
        }
    }

    fn deassert_ack(&mut self) {
        if self.bus.is_pending() {
            // The target is busy with the command and requested nothing, so
            // there is no handshake to complete
            return;
        }
        match self.busphase {
            ScsiBusPhase::DataOut => {
                if self.bus.dataout_len > 0 {
                    self.assert_req();
                    self.bus.responsebuf.push_back(self.reg_odr);
                    self.bus.dataout_len -= 1;
                    if self.bus.dataout_len == 0 {
                        let datavec = Vec::from_iter(self.bus.responsebuf.iter().cloned());
                        if let Err(e) = self.cmd_run(Some(&datavec)) {
                            log::error!("SCSI command run error: {:#}", e);
                        }
                    }
                } else {
                    // Transfer completed
                    self.set_phase(ScsiBusPhase::Status);
                }
            }
            ScsiBusPhase::Command => {
                if self.bus.cmdbuf.is_empty() {
                    self.bus.cmdlen = scsi_cmd_len(self.reg_odr).unwrap_or_else(|| {
                        log::error!("Cmd length unknown for {:02X}", self.reg_odr);
                        6
                    });
                }
                self.bus.cmdbuf.push(self.reg_odr);
                if self.bus.cmdbuf.len() >= self.bus.cmdlen {
                    if let Err(e) = self.cmd_run(None) {
                        error!("SCSI command ({:02X}) error: {}", self.bus.cmdbuf[0], e);
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
            ScsiBusPhase::DataIn => {
                if let Some(b) = self.bus.responsebuf.pop_front() {
                    self.reg_cdr = b;
                    self.assert_req();
                } else {
                    // Transfer completed
                    self.set_phase(ScsiBusPhase::Status);
                }
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
        let _is_write = addr & 1 != 0;
        let _dack = addr & 0b0010_0000_0000 != 0;
        let reg = NcrReadReg::from_u32((addr >> 4) & 0b111).unwrap();

        //if reg != NcrReadReg::CSR {
        //    debug!(
        //        "{:06X} SCSI read: write = {}, dack = {}, reg = {:?}",
        //        self.dbg_pc, is_write, dack, reg
        //    );
        //}

        match reg {
            // A CPU read (CS) doesn't handshake: that's the ICR ACK or, with
            // DACK, DMA (read_dma())
            NcrReadReg::CDR | NcrReadReg::IDR => Some(self.reg_cdr),
            NcrReadReg::MR => Some(self.reg_mr.0),
            NcrReadReg::ICR => Some(self.reg_icr.0),
            NcrReadReg::TCR => Some(self.reg_tcr.0),
            NcrReadReg::CSR => {
                let val = self.reg_csr.0;

                // MacII has a race condition where it will get stuck if
                // REQ is immediately set on a Data -> Status transition.
                if self.set_req.get_clear() {
                    self.reg_csr.set_req(true);
                }

                Some(val)
            }
            NcrReadReg::BSR => Some(
                self.reg_bsr
                    .with_dma_req(self.get_drq())
                    .with_dma_end(
                        self.reg_mr.dma_mode()
                            && !matches!(
                                self.busphase,
                                ScsiBusPhase::DataIn | ScsiBusPhase::DataOut,
                            ),
                    )
                    .with_phase_match(self.phase_match())
                    .0,
            ),
            NcrReadReg::RESET => {
                if self.scsi_trace_irq && self.reg_bsr.irq() {
                    debug!("SCSI IRQ cleared (RESET register read)");
                }
                self.reg_bsr.set_irq(false);
                Some(0)
            }
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        let _is_write = addr & 1 != 0;
        let _dack = addr & 0b0010_0000_0000 != 0;
        let reg = NcrWriteReg::from_u32((addr >> 4) & 0b111).unwrap();

        //debug!(
        //    "SCSI write: val = {:02X}, write = {}, dack = {}, reg = {:?}",
        //    val, is_write, dack, reg
        //);

        match reg {
            // A CPU write (CS) only loads the Output Data Register. The byte
            // is sent by the ACK handshake (ICR) or, with DACK, by DMA
            // (write_dma()).
            NcrWriteReg::ODR => {
                self.reg_odr = val;
                self.try_complete_selection();
                Some(())
            }
            NcrWriteReg::ICR => {
                let set = NcrRegIcr(val & !self.reg_icr.0);
                let clr = NcrRegIcr(!val & self.reg_icr.0);

                self.reg_icr.0 = val;

                // A bus reset takes the target off a command running in the
                // background
                if set.assert_rst() && self.bus.is_pending() {
                    self.bus.abort_pending();
                    self.set_phase(ScsiBusPhase::Free);
                }

                if set.assert_ack() {
                    self.assert_ack();
                } else if clr.assert_ack() {
                    self.deassert_ack();
                }

                match self.busphase {
                    ScsiBusPhase::Arbitration => {
                        if set.assert_sel() {
                            self.set_phase(ScsiBusPhase::Selection);
                        }
                    }
                    _ => (),
                }
                Some(())
            }
            NcrWriteReg::MR => {
                let set = NcrRegMr(val & !self.reg_mr.0);
                let clr = NcrRegMr(!val & self.reg_mr.0);
                self.reg_mr.0 = val;

                if set.arbitrate() {
                    // Initiate arbitration
                    self.set_phase(ScsiBusPhase::Arbitration);
                    self.reg_cdr = self.reg_odr; // Initiator ID
                    return Some(());
                }

                if clr.arbitrate() {
                    self.try_complete_selection();
                }

                // Leaving DMA mode disarms any pending DMA direction.
                if clr.dma_mode() {
                    self.dma_armed = false;
                }
                Some(())
            }
            NcrWriteReg::TCR => {
                self.reg_tcr.set_writable(val);
                Some(())
            }
            NcrWriteReg::SELEN => {
                self.reg_selen = val;
                Some(())
            }
            NcrWriteReg::StartDMASend
            | NcrWriteReg::StartDMATargetReceive
            | NcrWriteReg::StartDMAInitiatorReceive => {
                // The data byte written is discarded; any write arms DMA
                // for the selected direction. DMA mode must already be set
                // in MR. Arming enables DRQ generation and phase-mismatch
                // IRQ edge detection in set_phase().
                if self.reg_mr.dma_mode() {
                    self.dma_armed = true;
                }
                Some(())
            }
        }
    }
}

impl Tickable<&dyn EmuContext> for ScsiController {
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.bus.tick_targets(ticks, ctx)?;

        match self.bus.poll() {
            Some(Ok(outcome)) => self.enter_outcome(outcome),
            Some(Err(e)) => error!("SCSI command error: {:#}", e),
            None => (),
        }

        Ok(ticks)
    }
}

impl Debuggable for ScsiController {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{
            dbgprop_bool, dbgprop_enum, dbgprop_group, dbgprop_header, dbgprop_nest, dbgprop_udec,
        };

        let mut targets = vec![];
        for (id, o_t) in self.bus.targets.iter().enumerate() {
            if let Some(t) = o_t {
                targets.push(dbgprop_nest!(
                    format!("ID #{} - {:?}", id, t.target_type()),
                    t
                ));
            } else {
                targets.push(dbgprop_group!(format!("ID #{} - (no device)", id), vec![]));
            }
        }

        vec![
            dbgprop_group!("Targets", targets),
            dbgprop_group!(
                "Registers",
                vec![
                    dbgprop_byte!("MR", self.reg_mr.0),
                    dbgprop_byte!("ICR", self.reg_icr.0),
                    dbgprop_byte!("CSR", self.reg_csr.0),
                    dbgprop_byte!("CDR", self.reg_cdr),
                    dbgprop_byte!("ODR", self.reg_odr),
                    dbgprop_byte!("BSR", self.reg_bsr.0),
                    dbgprop_byte!("TCR", self.reg_tcr.0),
                    dbgprop_byte!("Status", self.bus.status),
                    dbgprop_bool!("DMA armed", self.dma_armed),
                ]
            ),
            dbgprop_enum!("Bus phase", self.busphase),
            dbgprop_udec!("Selected ID", self.bus.sel_id),
            dbgprop_bool!("Attention", self.sel_atn),
            dbgprop_header!("Buffers"),
            dbgprop_udec!("Command buffer len", self.bus.cmdbuf.len()),
            dbgprop_udec!("Command length", self.bus.cmdlen),
            dbgprop_udec!("Response buffer len", self.bus.responsebuf.len()),
            dbgprop_udec!("Data out len", self.bus.dataout_len),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::scsi::test_target::{SLOW_BLOCKSIZE, SlowState, SlowTarget, TestContext};
    use std::sync::{Arc, Mutex};

    const REG_ODR: Address = 0x00;
    const REG_ICR: Address = 0x10;
    const REG_MR: Address = 0x20;
    const REG_CSR: Address = 0x40;

    const ICR_ACK: u8 = 1 << 4;
    const ICR_SEL: u8 = 1 << 2;
    const ICR_RST: u8 = 1 << 7;
    const MR_ARBITRATE: u8 = 1 << 0;
    const CSR_REQ: u8 = 1 << 5;

    /// A 5380 with a target at ID 0 whose reads wait for the test
    fn ncr_with_slow_target() -> (ScsiController, Arc<Mutex<SlowState>>) {
        let mut ncr = ScsiController::new();
        let (target, state) = SlowTarget::new();
        ncr.bus_mut().targets[0] = Some(target);
        (ncr, state)
    }

    /// REQ as the initiator sees it, after the delay of the model
    fn req(ncr: &mut ScsiController) -> bool {
        ncr.read(REG_CSR).unwrap();
        ncr.read(REG_CSR).unwrap() & CSR_REQ != 0
    }

    /// Sends a byte with the ACK handshake
    fn handshake_out(ncr: &mut ScsiController, val: u8) {
        assert!(req(ncr), "no REQ for byte {:02X}", val);
        ncr.write(REG_ODR, val).unwrap();
        ncr.write(REG_ICR, ICR_ACK).unwrap();
        ncr.write(REG_ICR, 0).unwrap();
    }

    /// Arbitrates as ID 7, selects ID 0 and sends a READ(6) of one block,
    /// which stays pending
    fn start_slow_read(ncr: &mut ScsiController, state: &Mutex<SlowState>) {
        ncr.write(REG_ODR, 0x80).unwrap();
        ncr.write(REG_MR, MR_ARBITRATE).unwrap();
        ncr.write(REG_ICR, ICR_SEL).unwrap();
        ncr.write(REG_MR, 0).unwrap();
        ncr.write(REG_ODR, 0x01).unwrap();
        ncr.write(REG_ICR, 0).unwrap();
        assert_eq!(ncr.busphase, ScsiBusPhase::Command);

        for b in [0x08, 0, 0, 0, 1, 0] {
            handshake_out(ncr, b);
        }
        assert!(state.lock().unwrap().reading);
        // The target holds the bus without requesting anything
        assert_eq!(ncr.busphase, ScsiBusPhase::Command);
        assert!(!req(ncr));
    }

    #[test]
    fn pending_read_requests_data_when_done() {
        let (mut ncr, state) = ncr_with_slow_target();
        start_slow_read(&mut ncr, &state);

        ncr.tick(1, &TestContext).unwrap();
        assert!(!req(&mut ncr));

        state.lock().unwrap().release = Some(vec![0x55; SLOW_BLOCKSIZE]);
        ncr.tick(1, &TestContext).unwrap();
        assert_eq!(ncr.busphase, ScsiBusPhase::DataIn);
        assert!(req(&mut ncr));
        assert_eq!(ncr.read(REG_ODR).unwrap(), 0x55);
    }

    #[test]
    fn pending_read_ignores_handshakes() {
        let (mut ncr, state) = ncr_with_slow_target();
        start_slow_read(&mut ncr, &state);

        // A byte too many, as with pseudo-DMA writes past the CDB
        ncr.write(REG_ODR, 0x08).unwrap();
        ncr.write(REG_ICR, ICR_ACK).unwrap();
        ncr.write(REG_ICR, 0).unwrap();
        assert!(!req(&mut ncr));
        assert_eq!(ncr.bus().cmdbuf, [0x08, 0, 0, 0, 1, 0]);

        state.lock().unwrap().release = Some(vec![0x55; SLOW_BLOCKSIZE]);
        ncr.tick(1, &TestContext).unwrap();
        assert_eq!(ncr.busphase, ScsiBusPhase::DataIn);
    }

    #[test]
    fn bus_reset_drops_pending_read() {
        let (mut ncr, state) = ncr_with_slow_target();
        start_slow_read(&mut ncr, &state);

        ncr.write(REG_ICR, ICR_RST).unwrap();
        ncr.write(REG_ICR, 0).unwrap();
        assert_eq!(state.lock().unwrap().aborts, 1);
        assert_eq!(ncr.busphase, ScsiBusPhase::Free);

        // Nothing comes of the read afterwards, and the next command runs
        ncr.tick(1, &TestContext).unwrap();
        assert_eq!(ncr.busphase, ScsiBusPhase::Free);
        start_slow_read(&mut ncr, &state);
    }
}
