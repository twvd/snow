//! NCR 53C96 SCSI controller ("ESP")

use std::collections::VecDeque;

use anyhow::Result;
use log::*;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::mac::scsi::bus::{CmdOutcome, ScsiBus};
use crate::mac::scsi::scsi_cmd_len;
use crate::tickable::{Tickable, Ticks};
use crate::{dbgprop_bool, dbgprop_byte, dbgprop_udec};

/// Size of the data FIFO in bytes
const FIFO_SIZE: usize = 16;

/// Status register: bus phase (MSG/CD/IO)
const STAT_PHASE: u8 = 0b111;
/// Status register: transfer counter is zero
const STAT_TC: u8 = 1 << 4;
/// Status register: interrupt pending
const STAT_INT: u8 = 1 << 7;

/// Bus phases as they appear in the low bits of the status register.
/// Bit 0 = I/O, bit 1 = C/D, bit 2 = MSG.
const PHASE_DATA_OUT: u8 = 0;
const PHASE_DATA_IN: u8 = 1;
const PHASE_COMMAND: u8 = 2;
const PHASE_STATUS: u8 = 3;
const PHASE_MSG_OUT: u8 = 6;
const PHASE_MSG_IN: u8 = 7;

/// Interrupt register: function complete
const INTR_FC: u8 = 1 << 3;
/// Interrupt register: bus service
const INTR_BS: u8 = 1 << 4;
/// Interrupt register: disconnected
const INTR_DC: u8 = 1 << 5;
/// Interrupt register: illegal command
const INTR_IL: u8 = 1 << 6;
/// Interrupt register: SCSI bus reset detected
const INTR_RST: u8 = 1 << 7;

/// Sequence step: nothing transferred
const SEQ_0: u8 = 0;
/// Sequence step: stopped in the message out phase
const SEQ_MSG_OUT: u8 = 1;
/// Sequence step: transferring the command
const SEQ_CMD: u8 = 3;
/// Sequence step: command transferred completely
const SEQ_CMD_DONE: u8 = 4;

/// Command register: perform the transfer through DMA
const CMD_DMA: u8 = 0x80;

/// Configuration 1: do not report a SCSI bus reset
const CFG1_RESREPT: u8 = 1 << 6;

/// Configuration 3: leave the odd last byte of a 16-bit DMA transfer in the FIFO
const CFG3_SAVE_RESIDUAL: u8 = 1 << 2;

/// Length of the CDB of an opcode: the known commands, else from the group
/// code as the 53C96 decodes it as a target (data sheet p.16): 6 bytes for
/// group 0, the reserved groups 3 and 4 and the vendor unique group 6, 10
/// bytes for groups 1 and 2 and the vendor unique group 7, 12 for group 5.
fn cdb_len(op: u8) -> usize {
    scsi_cmd_len(op).unwrap_or(match op >> 5 {
        1 | 2 | 7 => 10,
        5 => 12,
        _ => 6,
    })
}

/// NCR 53C96 SCSI controller
#[derive(Serialize, Deserialize)]
pub struct Esp {
    /// The bus and its targets
    pub(crate) bus: ScsiBus,

    /// Data FIFO
    fifo: VecDeque<u8>,

    /// Transfer counter (16-bit, 0 is 65536), counts down during a transfer
    /// Used for DMA mode only
    tc: u32,

    /// Transfer count registers, loaded into tc by DMA commands
    stc: u32,

    /// Last command written to the command register
    cmd: u8,

    /// Status register
    stat: u8,

    /// Interrupt status register
    intr: u8,

    /// An interrupt that hasn't been serviced when a command was written, as
    /// (interrupt status, sequence step). The registers show it until the host
    /// reads the interrupt register. Meanwhile, `intr` and `seq` belong to the
    /// command, whose own interrupt is stacked behind this one (data sheet
    /// p.14).
    first_irq: Option<(u8, u8)>,

    /// Sequence step register
    seq: u8,

    /// Destination bus ID
    bus_id: u8,

    cfg1: u8,
    cfg2: u8,
    cfg3: u8,
    clkconv: u8,
    sel_timeout: u8,
    sync_period: u8,
    sync_offset: u8,

    /// Bytes still expected from the initiator in the data out phase
    dataout_buf: Vec<u8>,

    /// A target is successfully selected
    selected: bool,

    /// The command being executed asked for a DMA transfer.
    dma: bool,

    /// A DMA Transfer Information command is moving data
    dma_xfer: bool,

    /// The last command was a DMA transfer from the target, the DMA empties
    /// the FIFO
    dma_in: bool,

    /// The DMA bus is 16 bits wide (MODE pins)
    dma16: bool,

    /// A selection sequence waits for the rest of its CDB
    selecting: bool,

    /// A selection with ATN waits for its message byte by DMA
    identify_pending: bool,
}

impl Esp {
    pub fn new(dma16: bool) -> Self {
        Self {
            bus: ScsiBus::new(),
            fifo: VecDeque::with_capacity(FIFO_SIZE),
            tc: 0,
            stc: 0,
            cmd: 0,
            stat: 0,
            intr: 0,
            first_irq: None,
            seq: 0,
            bus_id: 0,
            // Host ID 7 after reset
            cfg1: 7,
            cfg2: 0,
            cfg3: 0,
            clkconv: 2,
            sel_timeout: 0,
            sync_period: 5,
            sync_offset: 0,
            dataout_buf: vec![],
            selected: false,
            dma: false,
            dma_xfer: false,
            dma_in: false,
            dma16,
            selecting: false,
            identify_pending: false,
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

    pub fn get_irq(&self) -> bool {
        self.stat & STAT_INT != 0 || self.first_irq.is_some()
    }

    /// DREQ, only asserted during a DMA command (data sheet p.10)
    pub fn get_drq(&self) -> bool {
        if self.dma_in {
            // As long as the FIFO holds a word (or a byte on an 8-bit bus) for
            // memory, and a last odd byte at the end of the transfer, unless
            // Config 3 leaves it to the processor (p.26)
            let threshold = if self.dma16 { 2 } else { 1 };
            let end = self.tc == 0 || self.phase() != PHASE_DATA_IN;
            self.fifo.len() >= threshold
                || (self.fifo.len() == 1 && end && self.cfg3 & CFG3_SAVE_RESIDUAL == 0)
        } else if self.dma && self.tc > 0 {
            // The FIFO always has room, the target takes the bytes at once
            match self.phase() {
                PHASE_COMMAND => self.selecting || self.dma_xfer,
                PHASE_DATA_OUT => self.dma_xfer && self.bus.dataout_len > 0,
                _ => false,
            }
        } else {
            false
        }
    }

    /// One DMA read cycle: a word on a 16-bit DMA bus, as (DB7-0, DB15-8).
    /// A lone last byte comes in DB7-0 with DB15-8 driven to ones (data
    /// sheet p.26).
    pub fn dack_read(&mut self) -> [u8; 2] {
        if !self.get_drq() {
            error!("ESP CHECK: DMA read without DREQ: {}", self.dbg());
        }
        let first = self.read_dma();
        if !self.dma16 {
            return [first, 0xFF];
        }
        let second = if self.fifo.is_empty() {
            0xFF
        } else {
            self.read_dma()
        };
        [first, second]
    }

    /// One DMA write cycle: a word on a 16-bit DMA bus, as (DB7-0, DB15-8).
    /// Only DB7-0 is taken for the last byte of a transfer (p.26).
    pub fn dack_write(&mut self, data: [u8; 2]) {
        self.write_dma(data[0]);
        if self.dma16 && !(self.dma && self.tc == 0) {
            self.write_dma(data[1]);
        }
    }

    fn read_dma(&mut self) -> u8 {
        let val = self.fifo_pop();
        if self.dma_xfer {
            self.dma_in_receive();
        }
        val
    }

    /// DMA transfer from the target: the counter counts the bytes received
    /// into the FIFO, which the DMA empties. The transfer is complete once
    /// the count runs out or the target has sent all its data (data sheet
    /// p.33, Transfer Information).
    fn dma_in_receive(&mut self) {
        while self.tc > 0
            && self.fifo.len() < FIFO_SIZE
            && let Some(b) = self.bus.responsebuf.pop_front()
        {
            self.fifo.push_back(b);
            self.advance_tc(1);
        }
        if self.bus.responsebuf.is_empty() {
            // The target goes on to the status phase
            self.set_phase(PHASE_STATUS);
        }
        if self.tc == 0 || self.phase() != PHASE_DATA_IN {
            self.dma_xfer = false;
            self.intr |= INTR_BS;
            self.raise_irq();
        }
    }

    fn write_dma(&mut self, val: u8) {
        if !self.selected {
            error!("ESP CHECK: DMA write while disconnected: {}", self.dbg());
            // Nothing is connected, so the byte goes out onto an idle bus and
            // disappears into an empty void of nothingness.
            //
            // For some reason this happens on System 7.1
            return;
        }

        match self.phase() {
            PHASE_COMMAND => {
                // Drain any CDB bytes pushed through PIO, this seems to happen
                while let Some(b) = self.fifo.pop_front() {
                    self.bus.cmdbuf.push(b);
                }
                if self.dma {
                    self.advance_tc(1);
                }
                if self.identify_pending {
                    self.identify_pending = false;
                } else {
                    self.command_byte(val);
                }
                if self.dma_xfer && self.tc == 0 {
                    // The count ran out before the end of the CDB
                    self.dma_xfer = false;
                    self.intr |= INTR_BS;
                    self.raise_irq();
                }
            }
            PHASE_DATA_OUT if self.dma_xfer => {
                let mut pending = self.bus.dataout_len;
                while let Some(b) = self.fifo.pop_front() {
                    self.dataout_buf.push(b);
                    pending = pending.saturating_sub(1);
                }
                self.dataout_buf.push(val);

                // Only the transition to zero completes the transfer; bytes
                // arriving when nothing is outstanding are discarded.
                let outstanding = pending > 0;
                self.bus.dataout_len = pending.saturating_sub(1);
                if self.dma {
                    self.advance_tc(1);
                }

                if outstanding && self.bus.dataout_len == 0 {
                    self.finish_dataout();
                } else if self.tc == 0 {
                    self.dma_xfer = false;
                    self.intr |= INTR_BS;
                    self.raise_irq();
                }
            }
            // No transfer going on: DACK writes the FIFO
            _ => self.fifo_push(val),
        }
    }

    fn phase(&self) -> u8 {
        self.stat & STAT_PHASE
    }

    fn set_phase(&mut self, phase: u8) {
        self.stat = (self.stat & !STAT_PHASE) | phase;
    }

    fn raise_irq(&mut self) {
        self.stat |= STAT_INT;
    }

    /// Hardware reset, or the Reset Chip command. The transfer count, the
    /// destination ID and our bus ID in Config 1 are kept (data sheet p.13,
    /// p.16, p.24).
    pub fn reset(&mut self) {
        self.fifo.clear();
        self.cmd = 0;
        self.stat = 0;
        self.intr = 0;
        self.first_irq = None;
        self.seq = 0;
        self.cfg1 &= 0b111;
        self.cfg2 = 0;
        self.cfg3 = 0;
        self.clkconv = 2;
        self.sync_period = 5;
        self.sync_offset = 0;
        self.disconnect();
    }

    /// Leaves the bus, as on a SCSI bus reset
    fn disconnect(&mut self) {
        self.set_phase(0);
        self.dataout_buf.clear();
        self.selected = false;
        self.dma = false;
        self.dma_xfer = false;
        self.dma_in = false;
        self.selecting = false;
        self.identify_pending = false;
    }

    fn fifo_push(&mut self, val: u8) {
        if self.fifo.len() >= FIFO_SIZE {
            error!("ESP CHECK: FIFO overflow: {}", self.dbg());
        }
        if self.fifo.len() < FIFO_SIZE {
            self.fifo.push_back(val);
        }
    }

    fn fifo_pop(&mut self) -> u8 {
        if self.fifo.is_empty() {
            error!("ESP CHECK: read from an empty FIFO: {}", self.dbg());
        }
        self.fifo.pop_front().unwrap_or(0)
    }

    /// Decrements the transfer counter and updates the TC status bit
    fn advance_tc(&mut self, n: u32) {
        self.tc = self.tc.saturating_sub(n);
        if self.tc == 0 {
            self.stat |= STAT_TC;
        }
    }

    /// A command is about to run: an interrupt which is still pending stays in
    /// front of the one the command may raise. A third one merges with the
    /// second, as the chip has room for two commands.
    fn stack_interrupt(&mut self) {
        if self.stat & STAT_INT != 0 && self.first_irq.is_none() {
            self.first_irq = Some((self.intr, self.seq));
            self.intr = 0;
            self.stat &= !STAT_INT;
        }
    }

    /// The sequence step the host sees
    fn visible_seq(&self) -> u8 {
        self.first_irq.map_or(self.seq, |(_, seq)| seq)
    }

    /// Reading the interrupt register while the interrupt is asserted clears
    /// it, the status bits but TC and the phase, and the sequence step (data
    /// sheet p.16, p.32). With a stacked interrupt, the registers change to
    /// describe it instead (p.14).
    fn read_interrupt(&mut self) -> u8 {
        if let Some((intr, _)) = self.first_irq.take() {
            if self.stat & STAT_INT == 0 && !self.selecting {
                self.seq = SEQ_0;
            }
            return intr;
        }
        let val = self.intr;
        if self.get_irq() {
            self.intr = 0;
            self.stat &= STAT_TC | STAT_PHASE;
            self.seq = SEQ_0;
        }
        val
    }

    /// Executes a command written to the command register
    fn run_command(&mut self, cmd: u8) {
        // Initiator and disconnected state commands are only taken in their
        // state, target commands never as the chip is never selected
        // (data sheet p.14, p.30, p.32, p.34)
        let legal = match cmd & 0x70 {
            0x00 => true,
            0x10 => self.selected,
            0x40 => !self.selected,
            _ => false,
        };
        if !legal {
            error!(
                "ESP CHECK: command ${:02X} illegal in this state: {}",
                cmd,
                self.dbg()
            );
            self.illegal_command();
            return;
        }

        self.dma = cmd & CMD_DMA != 0;
        self.dma_in = false;
        if self.dma {
            // DMA commands load the counter, a count of zero is the maximum.
            // Loading it clears the terminal count status (data sheet p.32).
            self.tc = if self.stc == 0 { 65536 } else { self.stc };
            self.stat &= !STAT_TC;
        }

        match cmd & !CMD_DMA {
            // Miscellaneous
            0x00 => (), // NOP
            0x01 => self.fifo.clear(),
            0x02 => self.reset(),
            0x03 => {
                // Reset SCSI bus: the chip disconnects, and sees the reset
                // as RSTO is wired to RSTI (p.29)
                self.disconnect();
                if self.cfg1 & CFG1_RESREPT == 0 {
                    self.intr |= INTR_RST;
                    self.raise_irq();
                }
            }

            // Initiator state
            0x10 => self.transfer_info(),
            0x11 => self.command_complete(),
            0x12 => {
                // Message accepted: the target disconnects
                self.intr |= INTR_DC;
                self.seq = SEQ_0;
                self.selected = false;
                self.fifo.clear();
                self.raise_irq();
            }
            0x18 => self.transfer_pad(),
            0x1A | 0x1B => error!("ESP CHECK: ATN command ${:02X}: {}", cmd, self.dbg()),

            // Disconnected state
            0x41 => self.select(false, false),
            0x42 => self.select(true, false),
            0x43 => self.select(true, true),
            // Enable selection/reselection, without an interrupt
            0x44 => (),
            // Disable selection/reselection: none was in progress (p.31)
            0x45 => {
                self.intr |= INTR_FC;
                self.raise_irq();
            }

            _ => {
                error!(
                    "ESP CHECK: unimplemented command ${:02X}: {}",
                    cmd,
                    self.dbg()
                );
                self.illegal_command();
            }
        }
    }

    /// An illegal command is ignored and cleared from the command register
    fn illegal_command(&mut self) {
        self.cmd = 0;
        self.intr |= INTR_IL;
        self.raise_irq();
    }

    /// Arbitrates and selects the target in the bus ID register
    fn select(&mut self, atn: bool, stop: bool) {
        let id = usize::from(self.bus_id & 0b111);
        self.seq = SEQ_0;
        self.bus.cmdbuf.clear();
        self.dataout_buf.clear();
        self.dma_xfer = false;

        if id >= ScsiBus::MAX_TARGETS || self.bus.targets[id].is_none() {
            // Nothing at this ID: report a disconnect
            self.stat &= !STAT_PHASE;
            self.intr |= INTR_DC;
            self.raise_irq();
            return;
        }
        self.bus.sel_id = id;

        self.selected = true;

        // The identify message (if any) is consumed here; only LUN 0 is
        // supported by the targets. Without one in the FIFO it is the first
        // DMA byte (data sheet p.31).
        if atn {
            if self.fifo.is_empty() && self.dma {
                self.identify_pending = true;
            } else {
                self.fifo_pop();
            }
        }

        if stop {
            // Select with ATN and stop: stay in the message out phase
            self.set_phase(PHASE_MSG_OUT);
            self.seq = SEQ_MSG_OUT;
            self.intr |= INTR_BS | INTR_FC;
            self.raise_irq();
            return;
        }

        // Whatever is in the FIFO is the start of the CDB
        self.set_phase(PHASE_COMMAND);
        while let Some(b) = self.fifo.pop_front() {
            self.bus.cmdbuf.push(b);
        }

        self.selecting = true;
        if self.cdb_complete() {
            // Command already here
            self.run_cdb();
        } else if !self.dma {
            // Wait for command
            self.intr |= INTR_BS | INTR_FC;
            self.seq = SEQ_CMD_DONE;
            self.selecting = false;
            self.raise_irq();
        }
        // With DMA, the sequence goes on as the CDB bytes come, through DMA
        // or the FIFO register, and interrupts once they are all sent (p.30).
        // Meanwhile the sequence step shows the command phase (table 8).
        else {
            self.seq = SEQ_CMD;
        }
    }

    /// Whether the command buffer holds a complete CDB
    fn cdb_complete(&self) -> bool {
        match self.bus.cmdbuf.first() {
            Some(op) => self.bus.cmdbuf.len() >= cdb_len(*op),
            None => false,
        }
    }

    fn command_byte(&mut self, val: u8) {
        self.bus.cmdbuf.push(val);
        if self.cdb_complete() {
            self.run_cdb();
        }
    }

    /// Executes the CDB in the command buffer
    fn run_cdb(&mut self) {
        if self.bus.cmdbuf.is_empty() {
            return;
        }
        if scsi_cmd_len(self.bus.cmdbuf[0]).is_none() {
            warn!(
                "unknown length for command ${:02X}, taken as {} bytes from its group",
                self.bus.cmdbuf[0],
                cdb_len(self.bus.cmdbuf[0])
            );
        }

        self.seq = SEQ_CMD_DONE;
        self.dma_xfer = false;
        match self.bus.cmd_run(None) {
            Ok(CmdOutcome::Status) => self.set_phase(PHASE_STATUS),
            Ok(CmdOutcome::DataIn) => self.set_phase(PHASE_DATA_IN),
            Ok(CmdOutcome::DataOut(_)) => self.set_phase(PHASE_DATA_OUT),
            Err(e) => {
                error!("SCSI command error: {:#}", e);
                self.intr |= INTR_DC;
                self.raise_irq();
                return;
            }
        }

        // Function complete for a selection sequence, the target now asks for
        // the next phase (p.17)
        self.intr |= INTR_BS;
        if self.selecting {
            self.intr |= INTR_FC;
            self.selecting = false;
        }
        self.raise_irq();
    }

    /// The initiator has delivered every byte the target asked for: run the
    /// command with the collected data.
    fn finish_dataout(&mut self) {
        if self.bus.cmdbuf.is_empty() {
            // Nothing to run the data against
            self.dataout_buf.clear();
            return;
        }

        self.dma_xfer = false;
        let data = std::mem::take(&mut self.dataout_buf);
        match self.bus.cmd_run(Some(&data)) {
            Ok(_) => self.set_phase(PHASE_STATUS),
            Err(e) => {
                error!("SCSI command error: {:#}", e);
                self.intr |= INTR_DC;
            }
        }
        self.intr |= INTR_BS;
        self.raise_irq();
    }

    /// Transfer Information: moves data between the FIFO and the target for the
    /// current phase.
    fn transfer_info(&mut self) {
        if self.dma && !matches!(self.phase(), PHASE_COMMAND | PHASE_DATA_IN | PHASE_DATA_OUT) {
            error!(
                "ESP CHECK: DMA Transfer Information in phase {}: {}",
                self.phase(),
                self.dbg()
            );
        }
        match self.phase() {
            PHASE_MSG_OUT => {
                // Identify message arriving through the FIFO instead of with
                // the select command
                self.fifo_pop();
                self.fifo.clear();
                self.set_phase(PHASE_COMMAND);
                self.intr |= INTR_BS;
                self.raise_irq();
            }
            PHASE_COMMAND => {
                // The CDB may be transferred over several commands
                while let Some(b) = self.fifo.pop_front() {
                    self.bus.cmdbuf.push(b);
                }
                if self.cdb_complete() {
                    self.run_cdb();
                } else if self.dma && self.tc > 0 {
                    // The rest comes by DMA (write_dma)
                    self.dma_xfer = true;
                } else {
                    self.intr |= INTR_BS;
                    self.raise_irq();
                }
            }
            PHASE_DATA_IN if self.dma => {
                self.dma_xfer = true;
                self.dma_in = true;
                self.dma_in_receive();
            }
            PHASE_DATA_IN => {
                // One byte per command without DMA (data sheet p.33)
                if self.fifo.len() < FIFO_SIZE
                    && let Some(b) = self.bus.responsebuf.pop_front()
                {
                    self.fifo.push_back(b);
                }

                if self.bus.responsebuf.is_empty() {
                    // Command complete
                    self.set_phase(PHASE_STATUS);
                }
                self.intr |= INTR_BS;
                self.raise_irq();
            }
            PHASE_DATA_OUT => {
                let n = self.fifo.len().min(self.bus.dataout_len);
                if n > 0 {
                    for _ in 0..n {
                        let b = self.fifo_pop();
                        self.dataout_buf.push(b);
                    }
                    // Bytes already in the FIFO don't count: in data out the
                    // counter counts the DMA bytes (data sheet p.13)
                    self.bus.dataout_len -= n;

                    if self.bus.dataout_len == 0 {
                        self.finish_dataout();
                        return;
                    }
                }
                if self.dma && self.tc > 0 {
                    // The rest comes by DMA (write_dma)
                    self.dma_xfer = true;
                } else {
                    self.intr |= INTR_BS;
                    self.raise_irq();
                }
            }
            _ => {
                self.intr |= INTR_BS;
                self.raise_irq();
            }
        }
    }

    /// Transfer Pad: discards what the target sends, or sends it what is in
    /// the FIFO and then null bytes, for as long as the transfer counter says
    /// (data sheet p.33). The sheet says that the FIFO may contain pad bytes
    /// after a phase change; they are not modelled, as nothing reads them.
    fn transfer_pad(&mut self) {
        let n = self.tc;
        match self.phase() {
            PHASE_DATA_IN => {
                let discard = n.min(self.bus.responsebuf.len() as u32);
                self.bus.responsebuf.drain(..discard as usize);
                self.advance_tc(discard);
                if self.bus.responsebuf.is_empty() {
                    self.set_phase(PHASE_STATUS);
                }
            }
            PHASE_DATA_OUT => {
                let from_fifo = self.fifo.len().min(self.bus.dataout_len);
                self.dataout_buf.extend(self.fifo.drain(..from_fifo));
                self.bus.dataout_len -= from_fifo;

                let pad = self.bus.dataout_len.min(n as usize);
                self.dataout_buf.resize(self.dataout_buf.len() + pad, 0);
                self.bus.dataout_len -= pad;
                self.advance_tc(pad as u32);

                if self.bus.dataout_len == 0 {
                    self.finish_dataout();
                    return;
                }
            }
            _ => self.advance_tc(n),
        }
        self.intr |= INTR_BS;
        self.raise_irq();
    }

    /// Initiator Command Complete: reads the status and message bytes off the
    /// bus into the FIFO.
    fn command_complete(&mut self) {
        if self.dma {
            error!("ESP CHECK: DMA Initiator Command Complete: {}", self.dbg());
        }
        if self.phase() != PHASE_STATUS {
            error!(
                "ESP CHECK: Initiator Command Complete out of the status phase: {}",
                self.dbg()
            );
            self.intr |= INTR_IL;
            self.raise_irq();
            return;
        }

        self.fifo_push(self.bus.status);
        // Command complete message
        self.fifo_push(0);
        self.set_phase(PHASE_MSG_IN);
        self.intr |= INTR_FC;
        self.raise_irq();
    }
}

impl BusMember<Address> for Esp {
    fn read(&mut self, addr: Address) -> Option<u8> {
        let reg = (addr >> 4) & 0xF;

        Some(match reg {
            0x0 => self.tc as u8,
            0x1 => (self.tc >> 8) as u8,
            0x2 => self.fifo_pop(),
            0x3 => self.cmd,
            0x4 => self.stat | if self.get_irq() { STAT_INT } else { 0 },
            0x5 => self.read_interrupt(),
            0x6 => self.visible_seq(),
            // FIFO flags, with a copy of the sequence step on top (p.23)
            0x7 => self.fifo.len() as u8 | (self.visible_seq() << 5),
            0x8 => self.cfg1,
            0xB => self.cfg2,
            0xC => self.cfg3,
            _ => 0,
        })
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        let reg = (addr >> 4) & 0xF;

        match reg {
            0x0 => {
                self.stc = (self.stc & 0xFF00) | u32::from(val);
            }
            0x1 => {
                self.stc = (self.stc & 0x00FF) | (u32::from(val) << 8);
            }
            0x2 => {
                if self.dma_xfer {
                    error!(
                        "ESP CHECK: FIFO write during a DMA transfer: {}",
                        self.dbg()
                    );
                }
                if self.selected && self.phase() == PHASE_COMMAND {
                    self.command_byte(val);
                } else {
                    self.fifo_push(val);
                }
            }
            0x3 => {
                // Reset chip and reset bus execute at once, the others should
                // wait for the command before them (p.14)
                if self.dma_xfer && !matches!(val & !CMD_DMA, 0x02 | 0x03) {
                    error!(
                        "ESP CHECK: command ${:02X} written during a DMA transfer: {}",
                        val,
                        self.dbg()
                    );
                }
                self.stack_interrupt();
                self.cmd = val;
                self.run_command(val);
            }
            0x4 => self.bus_id = val,
            0x5 => self.sel_timeout = val,
            0x6 => self.sync_period = val,
            0x7 => self.sync_offset = val,
            0x8 => self.cfg1 = val,
            0x9 => self.clkconv = val,
            0xA => (),
            0xB => self.cfg2 = val,
            0xC => self.cfg3 = val,
            _ => (),
        }

        Some(())
    }
}

impl Tickable<&dyn EmuContext> for Esp {
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.bus.tick_targets(ticks, ctx)?;

        Ok(ticks)
    }
}

impl Debuggable for Esp {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_group, dbgprop_header, dbgprop_nest};

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
                    dbgprop_byte!("Command", self.cmd),
                    dbgprop_byte!("Status", self.stat),
                    dbgprop_byte!("Interrupt", self.intr),
                    dbgprop_byte!("Sequence step", self.seq),
                    dbgprop_byte!("Bus ID", self.bus_id),
                    dbgprop_byte!("Config 1", self.cfg1),
                    dbgprop_byte!("Config 2", self.cfg2),
                    dbgprop_byte!("Config 3", self.cfg3),
                    dbgprop_byte!("Clock conversion", self.clkconv),
                    dbgprop_byte!("Select timeout", self.sel_timeout),
                    dbgprop_byte!("Sync period", self.sync_period),
                    dbgprop_byte!("Sync offset", self.sync_offset),
                ]
            ),
            dbgprop_header!("State"),
            dbgprop_bool!("Interrupt", self.get_irq()),
            dbgprop_udec!("Bus phase", self.phase()),
            dbgprop_udec!("Transfer counter", self.tc),
            dbgprop_udec!("FIFO len", self.fifo.len()),
            dbgprop_udec!("Selected ID", self.bus.sel_id),
            dbgprop_udec!("SCSI status", self.bus.status),
            dbgprop_udec!("Response buffer len", self.bus.responsebuf.len()),
            dbgprop_udec!("Data out len", self.bus.dataout_len),
        ]
    }
}

impl Esp {
    /// State summary for the temporary checks
    pub fn dbg(&self) -> String {
        format!(
            "cmd {:02X} phase {} tc {} stc {} fifo {} selected {} selecting {} dma {} xfer {} in {} cfg3 {:02X} cdb {:02X?} out {} resp {}",
            self.cmd,
            self.phase(),
            self.tc,
            self.stc,
            self.fifo.len(),
            self.selected,
            self.selecting,
            self.dma,
            self.dma_xfer,
            self.dma_in,
            self.cfg3,
            self.bus.cmdbuf,
            self.bus.dataout_len,
            self.bus.responsebuf.len()
        )
    }

    pub fn fifo_len(&self) -> usize {
        self.fifo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::scsi::disk_image::DiskImage;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    const REG_TC_LOW: Address = 0x00;
    const REG_TC_HIGH: Address = 0x10;
    const REG_FIFO: Address = 0x20;
    const REG_CMD: Address = 0x30;
    const REG_STATUS: Address = 0x40;
    const REG_INTR: Address = 0x50;

    struct MemDisk(Arc<Mutex<Vec<u8>>>);

    impl DiskImage for MemDisk {
        fn byte_len(&self) -> usize {
            self.0.lock().unwrap().len()
        }
        fn read_bytes(&self, offset: usize, length: usize) -> Vec<u8> {
            self.0.lock().unwrap()[offset..offset + length].to_vec()
        }
        fn write_bytes(&mut self, offset: usize, data: &[u8]) {
            self.0.lock().unwrap()[offset..offset + data.len()].copy_from_slice(data);
        }
        fn media_bytes(&self) -> Option<&[u8]> {
            None
        }
        fn image_path(&self) -> Option<&Path> {
            None
        }
    }

    /// A 53C96 with a disk of 16 blocks full of $AA at ID 0
    fn esp_with_disk() -> (Esp, Arc<Mutex<Vec<u8>>>) {
        let image = Arc::new(Mutex::new(vec![0xAA; 16 * 512]));
        let mut esp = Esp::new(true);
        esp.bus_mut()
            .attach_disk_image_at(Box::new(MemDisk(image.clone())), 0)
            .unwrap();
        (esp, image)
    }

    fn command(esp: &mut Esp, cmd: u8) {
        esp.write(REG_CMD, cmd).unwrap();
    }

    /// Selects the disk and sends a CDB, through the FIFO
    fn select(esp: &mut Esp, cdb: &[u8]) {
        for b in cdb {
            esp.write(REG_FIFO, *b).unwrap();
        }
        command(esp, 0x41);
        assert!(esp.get_irq());
        esp.read(REG_INTR).unwrap();
    }

    fn set_count(esp: &mut Esp, count: u16) {
        esp.write(REG_TC_LOW, count as u8).unwrap();
        esp.write(REG_TC_HIGH, (count >> 8) as u8).unwrap();
    }

    fn phase(esp: &mut Esp) -> u8 {
        esp.read(REG_STATUS).unwrap() & STAT_PHASE
    }

    #[test]
    fn second_interrupt_stacks_behind_the_first() {
        let mut esp = Esp::new(true);
        // Disable selection: function complete
        command(&mut esp, 0x45);
        // Reset SCSI bus, without servicing the first interrupt
        command(&mut esp, 0x03);
        assert_ne!(esp.read(REG_STATUS).unwrap() & STAT_INT, 0);
        assert_eq!(esp.read(REG_INTR).unwrap(), INTR_FC);
        assert!(esp.get_irq());
        assert_ne!(esp.read(REG_STATUS).unwrap() & STAT_INT, 0);
        assert_eq!(esp.read(REG_INTR).unwrap(), INTR_RST);
        assert!(!esp.get_irq());
        assert_eq!(esp.read(REG_STATUS).unwrap() & STAT_INT, 0);
    }

    #[test]
    fn command_without_interrupt_leaves_the_pending_one() {
        let mut esp = Esp::new(true);
        command(&mut esp, 0x45);
        // Flush FIFO, no interrupt of its own
        command(&mut esp, 0x01);
        assert!(esp.get_irq());
        assert_eq!(esp.read(REG_INTR).unwrap(), INTR_FC);
        assert!(!esp.get_irq());
        assert_eq!(esp.read(REG_INTR).unwrap(), 0);
    }

    #[test]
    fn chip_reset_clears_a_pending_interrupt() {
        let mut esp = Esp::new(true);
        command(&mut esp, 0x45);
        command(&mut esp, 0x02);
        assert!(!esp.get_irq());
        assert_eq!(esp.read(REG_INTR).unwrap(), 0);
    }

    #[test]
    fn pad_completes_a_short_write() {
        let (mut esp, image) = esp_with_disk();
        // WRITE(10) of block 1
        select(&mut esp, &[0x2A, 0, 0, 0, 0, 1, 0, 0, 1, 0]);
        assert_eq!(phase(&mut esp), PHASE_DATA_OUT);

        for b in [1, 2, 3, 4] {
            esp.write(REG_FIFO, b).unwrap();
        }
        set_count(&mut esp, 508);
        command(&mut esp, 0x98);

        assert_eq!(phase(&mut esp), PHASE_STATUS);
        assert!(esp.get_irq());
        assert_eq!(esp.read(REG_INTR).unwrap(), INTR_BS);
        let image = image.lock().unwrap();
        assert_eq!(image[512..516], [1, 2, 3, 4]);
        assert!(image[516..1024].iter().all(|b| *b == 0));
        assert!(image[..512].iter().all(|b| *b == 0xAA));
        assert_eq!(image[1024], 0xAA);
    }

    #[test]
    fn pad_stops_at_the_transfer_count() {
        let (mut esp, image) = esp_with_disk();
        select(&mut esp, &[0x2A, 0, 0, 0, 0, 1, 0, 0, 1, 0]);

        set_count(&mut esp, 100);
        command(&mut esp, 0x98);
        assert_eq!(phase(&mut esp), PHASE_DATA_OUT);
        assert_ne!(esp.read(REG_STATUS).unwrap() & STAT_TC, 0);
        assert_eq!(esp.read(REG_INTR).unwrap(), INTR_BS);
        assert_eq!(image.lock().unwrap()[512], 0xAA);

        set_count(&mut esp, 412);
        command(&mut esp, 0x98);
        assert_eq!(phase(&mut esp), PHASE_STATUS);
        assert_eq!(image.lock().unwrap()[512], 0);
    }

    #[test]
    fn pad_discards_received_data() {
        let (mut esp, _image) = esp_with_disk();
        // READ(10) of 1 block
        select(&mut esp, &[0x28, 0, 0, 0, 0, 1, 0, 0, 1, 0]);
        assert_eq!(phase(&mut esp), PHASE_DATA_IN);

        set_count(&mut esp, 100);
        command(&mut esp, 0x98);
        assert_eq!(phase(&mut esp), PHASE_DATA_IN);
        assert_eq!(esp.read(REG_INTR).unwrap(), INTR_BS);

        // A count beyond the data: the target goes on to the status phase
        // before the count runs out
        set_count(&mut esp, 1000);
        command(&mut esp, 0x98);
        assert_eq!(phase(&mut esp), PHASE_STATUS);
        assert_eq!(esp.read(REG_STATUS).unwrap() & STAT_TC, 0);
        assert_eq!(esp.fifo_len(), 0);
    }
}
