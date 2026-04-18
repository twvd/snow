//! NCR5380 SCSI controller

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use log::*;
use num_derive::FromPrimitive;
use num_derive::ToPrimitive;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::bus::{Address, BusMember};
use crate::dbgprop_byte;
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::mac::scsi::STATUS_GOOD;
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::cdrom::ScsiTargetCdrom;
use crate::mac::scsi::disk::ScsiTargetDisk;
use crate::mac::scsi::disk_image::DiskImage;
#[cfg(feature = "ethernet")]
use crate::mac::scsi::ethernet::ScsiTargetEthernet;
use crate::mac::scsi::scsi_cmd_len;
use crate::mac::scsi::target::ScsiTarget;
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::scsi::toolbox::BlueSCSI;
use crate::renderer::AudioProvider;
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

    set_req: LatchingEvent,

    #[serde(skip)]
    toolbox: BlueSCSI,
    scsi_debug: bool,
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

    /// Returns the length of image data to write to savestates or None if no data
    #[cfg(feature = "savestates")]
    pub fn get_savestate_img_len(&self, id: usize) -> Option<usize> {
        self.targets[id]
            .as_ref()
            .and_then(|t| t.savestate_img_len())
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
            set_req: Default::default(),
            targets: Default::default(),
            toolbox: BlueSCSI::default(),
            scsi_debug: false,
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

    /// Attaches a disk backed by a custom disk image at the given SCSI ID.
    pub(crate) fn attach_disk_image_at(
        &mut self,
        image: Box<dyn DiskImage>,
        scsi_id: usize,
    ) -> Result<()> {
        if scsi_id >= Self::MAX_TARGETS {
            bail!("SCSI ID out of range: {}", scsi_id);
        }
        self.targets[scsi_id] = Some(Box::new(ScsiTargetDisk::new(image)));
        Ok(())
    }

    /// Attaches a CD-ROM drive at the given SCSI ID
    pub fn attach_cdrom_at(
        &mut self,
        scsi_id: usize,
        audio_provider: Option<&mut (dyn AudioProvider + '_)>,
    ) {
        self.targets[scsi_id] = Some(Box::new(ScsiTargetCdrom::new(audio_provider)));
    }

    /// Inserts a CD-ROM with the custom disk image at the given SCSI ID.
    pub fn insert_cdrom_image_at(
        &mut self,
        image: Box<dyn DiskImage>,
        scsi_id: usize,
    ) -> Result<()> {
        if scsi_id >= Self::MAX_TARGETS {
            bail!("SCSI ID out of range: {}", scsi_id);
        }
        let Some(target) = self.targets[scsi_id].as_mut() else {
            bail!("No target attached at SCSI ID {}", scsi_id);
        };
        target.load_image(image)
    }

    /// Attaches an Ethernet adapter at the given SCSI ID
    #[cfg(feature = "ethernet")]
    pub fn attach_ethernet_at(&mut self, scsi_id: usize) {
        self.targets[scsi_id] = Some(Box::new(ScsiTargetEthernet::default()));
    }

    /// Detaches a target from the given SCSI ID
    pub fn detach_target(&mut self, scsi_id: usize) {
        self.targets[scsi_id] = None;
    }

    pub fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> {
        for t in self.targets.iter_mut().flatten() {
            t.set_audio_provider(provider)?;
        }

        Ok(())
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
        if self.targets[id].is_none() {
            // No device present at this ID
            self.set_phase(ScsiBusPhase::Free);
            return;
        }

        self.sel_id = id;
        self.sel_atn = self.reg_icr.assert_atn();
        self.set_phase(ScsiBusPhase::Command);
    }

    fn set_phase(&mut self, phase: ScsiBusPhase) {
        //debug!("Bus phase: {:?}", phase);

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
            _ => (),
        }
    }

    fn cmd_run(&mut self, outdata: Option<&[u8]>) -> Result<()> {
        let cmd = &self.cmdbuf;

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
                if len == 0 {
                    // [SPC-3] 6.7:
                    // "A parameter list length of zero specifies that the Data-Out Buffer shall be empty. This condition
                    // shall not be considered as an error."
                    if let Err(e) = self.cmd_run(Some(&[])) {
                        log::error!("SCSI command run error: {:#}", e);
                    }
                } else {
                    self.set_phase(ScsiBusPhase::DataOut);
                }
            }
            Err(e) => return Err(e),
        }

        Ok(())
    }

    pub fn get_drq(&self) -> bool {
        self.reg_mr.dma_mode()
            && matches!(
                self.busphase,
                ScsiBusPhase::DataIn
                    | ScsiBusPhase::DataOut
                    | ScsiBusPhase::Status
                    | ScsiBusPhase::Command
                    | ScsiBusPhase::MessageIn
                    | ScsiBusPhase::MessageOut
            )
    }

    pub fn read_dma(&mut self) -> u8 {
        self.read_datareg()
    }

    pub fn write_dma(&mut self, val: u8) {
        self.write_datareg(val);
    }

    fn write_datareg(&mut self, val: u8) {
        self.reg_odr = val;

        // Pseudo-DMA path: writes to the DMA window auto-pulse ACK, so we
        // advance the REQ/ACK handshake here instead of waiting for an
        // explicit ICR ACK toggle.
        if self.reg_mr.dma_mode()
            && matches!(self.busphase, ScsiBusPhase::DataOut | ScsiBusPhase::Command)
        {
            self.assert_ack();
            self.deassert_ack();
            return;
        }

        // Legacy PIO DataOut path (byte buffered here, ACK handled via ICR).
        if self.busphase == ScsiBusPhase::DataOut {
            self.responsebuf.push_back(val);
            self.dataout_len -= 1;
            if self.dataout_len == 0 {
                let datavec = Vec::from_iter(self.responsebuf.iter().cloned());
                if let Err(e) = self.cmd_run(Some(&datavec)) {
                    log::error!("SCSI command run error: {:#}", e);
                }
            }
        }
    }

    fn read_datareg(&mut self) -> u8 {
        let val = self.reg_cdr;
        if self.busphase == ScsiBusPhase::DataIn && self.reg_mr.dma_mode() {
            // Pump next byte to CDR for next read
            if let Some(b) = self.responsebuf.pop_front() {
                self.reg_cdr = b;
                self.assert_req();
            } else {
                // Transfer completed
                self.deassert_req();

                self.set_phase(ScsiBusPhase::Status);
            }
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
        match self.busphase {
            ScsiBusPhase::DataOut => {
                if self.dataout_len > 0 {
                    self.assert_req();
                    self.responsebuf.push_back(self.reg_odr);
                    self.dataout_len -= 1;
                    if self.dataout_len == 0 {
                        let datavec = Vec::from_iter(self.responsebuf.iter().cloned());
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
                if self.cmdbuf.is_empty() {
                    self.cmdlen = scsi_cmd_len(self.reg_odr).unwrap_or_else(|| {
                        log::error!("Cmd length unknown for {:02X}", self.reg_odr);
                        6
                    });
                }
                self.cmdbuf.push(self.reg_odr);
                if self.cmdbuf.len() >= self.cmdlen {
                    if let Err(e) = self.cmd_run(None) {
                        error!("SCSI command ({:02X}) error: {}", self.cmdbuf[0], e);
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
                if let Some(b) = self.responsebuf.pop_front() {
                    self.reg_cdr = b;
                    self.reg_csr.set_req(true);
                } else {
                    // Transfer completed
                    self.set_phase(ScsiBusPhase::Status);
                }
            }
            _ => {}
        }
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
            NcrReadReg::CDR | NcrReadReg::IDR => Some(self.read_datareg()),
            NcrReadReg::MR => Some(self.reg_mr.0),
            NcrReadReg::ICR => Some(self.reg_icr.0),
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
                    .with_dma_end(!matches!(
                        self.busphase,
                        ScsiBusPhase::DataIn | ScsiBusPhase::DataOut,
                    ))
                    .0,
            ),
            NcrReadReg::RESET => {
                self.reg_bsr.set_irq(false);
                Some(0)
            }
            _ => Some(0),
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
            NcrWriteReg::ODR => {
                self.write_datareg(val);
                self.try_complete_selection();
                Some(())
            }
            NcrWriteReg::ICR => {
                let set = NcrRegIcr(val & !self.reg_icr.0);
                let clr = NcrRegIcr(!val & self.reg_icr.0);

                self.reg_icr.0 = val;

                if set.assert_ack() {
                    self.assert_ack();
                } else if clr.assert_ack() {
                    self.deassert_ack();
                }

                match self.busphase {
                    ScsiBusPhase::Arbitration => {
                        if set.assert_sel() {
                            self.reg_bsr.set_irq(true);
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
                Some(())
            }
            _ => {
                //warn!("Unknown SCSI register write: {:?} = {:02X}", reg, val);
                Some(())
            }
        }
    }
}

impl Tickable<&dyn EmuContext> for ScsiController {
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        for target in self.targets.iter_mut().flatten() {
            target.tick(ticks, ctx)?;
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
        for (id, o_t) in self.targets.iter().enumerate() {
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
                    dbgprop_byte!("Status", self.status),
                ]
            ),
            dbgprop_enum!("Bus phase", self.busphase),
            dbgprop_udec!("Selected ID", self.sel_id),
            dbgprop_bool!("Attention", self.sel_atn),
            dbgprop_header!("Buffers"),
            dbgprop_udec!("Command buffer len", self.cmdbuf.len()),
            dbgprop_udec!("Command length", self.cmdlen),
            dbgprop_udec!("Response buffer len", self.responsebuf.len()),
            dbgprop_udec!("Data out len", self.dataout_len),
        ]
    }
}
