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
/// Sequence step: command transferred completely
const SEQ_CMD_DONE: u8 = 4;

/// Command register: perform the transfer through DMA
const CMD_DMA: u8 = 0x80;

/// Configuration 1: do not report a SCSI bus reset
const CFG1_RESREPT: u8 = 1 << 6;

/// NCR 53C96 SCSI controller
#[derive(Serialize, Deserialize)]
pub struct Esp {
    /// The bus and its targets
    pub(crate) bus: ScsiBus,

    /// Data FIFO
    fifo: VecDeque<u8>,

    /// Transfer counter (24-bit), counts down during a transfer
    /// Used for DMA mode only
    tc: u32,

    /// Transfer counter, reloaded into tc
    /// Used for DMA mode only
    stc: u32,

    /// Last command written to the command register
    cmd: u8,

    /// Status register
    stat: u8,

    /// Interrupt status register
    intr: u8,

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
}

impl Esp {
    pub fn new() -> Self {
        Self {
            bus: ScsiBus::new(),
            fifo: VecDeque::with_capacity(FIFO_SIZE),
            tc: 0,
            stc: 0,
            cmd: 0,
            stat: 0,
            intr: 0,
            seq: 0,
            bus_id: 0,
            // Host ID 7 after reset
            cfg1: 7,
            cfg2: 0,
            cfg3: 0,
            clkconv: 0,
            sel_timeout: 0,
            sync_period: 0,
            sync_offset: 0,
            dataout_buf: vec![],
            selected: false,
            dma: false,
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
        self.stat & STAT_INT != 0
    }

    pub fn get_drq(&self) -> bool {
        // Just shove bytes out as fast as we can, the driver doesn't
        // seem to mind.
        match self.phase() {
            PHASE_DATA_IN => !self.bus.responsebuf.is_empty() || !self.fifo.is_empty(),
            PHASE_DATA_OUT => self.bus.dataout_len > 0,
            _ => false,
        }
    }

    pub fn read_dma(&mut self) -> u8 {
        if self.fifo.is_empty()
            && self.phase() == PHASE_DATA_IN
            && let Some(b) = self.bus.responsebuf.pop_front()
        {
            self.fifo.push_back(b);
        }

        let val = self.fifo_pop();
        if self.dma {
            self.advance_tc(1);
        }

        if self.phase() == PHASE_DATA_IN && self.bus.responsebuf.is_empty() && self.fifo.is_empty()
        {
            // Done, status phase
            self.set_phase(PHASE_STATUS);
            self.intr |= INTR_BS;
            self.raise_irq();
        }

        val
    }

    pub fn write_dma(&mut self, val: u8) {
        if !self.selected {
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
                self.command_byte(val);
            }
            PHASE_DATA_OUT => {
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
                }
            }
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

    fn reset_chip(&mut self) {
        self.fifo.clear();
        self.tc = 0;
        self.stc = 0;
        self.cmd = 0;
        self.stat = 0;
        self.intr = 0;
        self.seq = 0;
        self.bus_id = 0;
        self.cfg1 = 7;
        self.dataout_buf.clear();
        self.selected = false;
        self.dma = false;
    }

    fn fifo_push(&mut self, val: u8) {
        if self.fifo.len() < FIFO_SIZE {
            self.fifo.push_back(val);
        }
    }

    fn fifo_pop(&mut self) -> u8 {
        self.fifo.pop_front().unwrap_or(0)
    }

    /// Decrements the transfer counter and updates the TC status bit
    fn advance_tc(&mut self, n: u32) {
        self.tc = self.tc.saturating_sub(n);
        if self.tc == 0 {
            self.stat |= STAT_TC;
        }
    }

    /// Executes a command written to the command register
    fn run_command(&mut self, cmd: u8) {
        self.dma = cmd & CMD_DMA != 0;
        if self.dma {
            // A transfer count of zero means the maximum
            self.tc = if self.stc == 0 { 65536 } else { self.stc };
        }

        match cmd & !CMD_DMA {
            // Miscellaneous
            0x00 => (), // NOP
            0x01 => self.fifo.clear(),
            0x02 => self.reset_chip(),
            0x03 => {
                // Reset SCSI bus
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
            0x1A | 0x1B => (), // Set/reset ATN

            // Disconnected state
            0x41 => self.select(false, false),
            0x42 => self.select(true, false),
            0x43 => self.select(true, true),
            // Target mode commands
            0x44 | 0x45 => unreachable!(),

            _ => {
                warn!("illegal command ${:02X}", cmd);
                self.intr |= INTR_IL;
                self.raise_irq();
            }
        }
    }

    /// Arbitrates and selects the target in the bus ID register
    fn select(&mut self, atn: bool, stop: bool) {
        let id = usize::from(self.bus_id & 0b111);
        self.seq = SEQ_0;
        self.bus.cmdbuf.clear();
        self.dataout_buf.clear();

        if id >= ScsiBus::MAX_TARGETS || self.bus.targets[id].is_none() {
            // Nothing at this ID: report a disconnect
            self.stat &= !(STAT_PHASE | STAT_TC);
            self.intr |= INTR_DC;
            self.raise_irq();
            return;
        }
        self.bus.sel_id = id;

        // The identify message (if any) is consumed here; only LUN 0 is
        // supported by the targets.
        if atn && !self.fifo.is_empty() {
            self.fifo_pop();
        }

        if stop {
            // Select with ATN and stop: stay in the message out phase
            self.set_phase(PHASE_MSG_OUT);
            self.seq = SEQ_MSG_OUT;
            self.intr |= INTR_BS | INTR_FC;
            self.raise_irq();
            return;
        }

        self.selected = true;

        // Whatever is in the FIFO is the start of the CDB
        self.set_phase(PHASE_COMMAND);
        while let Some(b) = self.fifo.pop_front() {
            self.bus.cmdbuf.push(b);
        }

        if self.cdb_complete() {
            // Command already here
            self.run_cdb();
        } else {
            // Wait for command
            self.intr |= INTR_BS | INTR_FC;
            self.seq = SEQ_CMD_DONE;
            self.raise_irq();
        }
    }

    /// Whether the command buffer holds a complete CDB
    fn cdb_complete(&self) -> bool {
        match self.bus.cmdbuf.first() {
            Some(op) => self.bus.cmdbuf.len() >= scsi_cmd_len(*op).unwrap_or(6),
            None => false,
        }
    }

    fn command_byte(&mut self, val: u8) {
        self.bus.cmdbuf.push(val);
        if self.dma {
            self.advance_tc(1);
        }
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
                "unknown length for command ${:02X}, assumed 6",
                self.bus.cmdbuf[0]
            );
        }

        self.seq = SEQ_CMD_DONE;
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

        self.intr |= INTR_BS | INTR_FC;
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

    /// A transfer command issued with no target connected
    fn disconnected_transfer(&mut self) {
        self.fifo.clear();
        self.intr |= INTR_DC;
        self.seq = SEQ_0;
        self.raise_irq();
    }

    /// Transfer Information: moves data between the FIFO and the target for the
    /// current phase.
    fn transfer_info(&mut self) {
        if !self.selected {
            self.disconnected_transfer();
            return;
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
                } else {
                    self.intr |= INTR_BS;
                    self.raise_irq();
                }
            }
            PHASE_DATA_IN => {
                let free = (FIFO_SIZE - self.fifo.len()) as u32;
                let avail = self.bus.responsebuf.len() as u32;
                let n = if self.dma {
                    self.tc.min(free).min(avail)
                } else {
                    free.min(avail)
                };
                for _ in 0..n {
                    let b = self.bus.responsebuf.pop_front().unwrap();
                    self.fifo.push_back(b);
                }
                if self.dma {
                    self.advance_tc(n);
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
                    self.bus.dataout_len -= n;
                    if self.dma {
                        self.advance_tc(n as u32);
                    }

                    if self.bus.dataout_len == 0 {
                        self.finish_dataout();
                    }
                }
                self.intr |= INTR_BS;
                self.raise_irq();
            }
            _ => {
                self.intr |= INTR_BS;
                self.raise_irq();
            }
        }
    }

    /// Transfer Pad: like Transfer Information, but without moving real data
    fn transfer_pad(&mut self) {
        if !self.selected {
            self.disconnected_transfer();
            return;
        }

        let n = self.tc;
        match self.phase() {
            PHASE_DATA_IN => {
                for _ in 0..n.min(self.bus.responsebuf.len() as u32) {
                    self.bus.responsebuf.pop_front();
                }
                if self.bus.responsebuf.is_empty() {
                    self.set_phase(PHASE_STATUS);
                }
            }
            PHASE_DATA_OUT => {
                let pad = self.bus.dataout_len.min(n as usize);
                self.bus.dataout_len -= pad;
            }
            _ => (),
        }
        self.advance_tc(n);
        self.intr |= INTR_BS;
        self.raise_irq();
    }

    /// Initiator Command Complete: reads the status and message bytes off the
    /// bus into the FIFO.
    fn command_complete(&mut self) {
        if self.phase() != PHASE_STATUS {
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

impl Default for Esp {
    fn default() -> Self {
        Self::new()
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
            0x4 => self.stat,
            0x5 => {
                // Reading the interrupt register clears it, the interrupt and
                // every status bit except the phase and TC.
                let val = self.intr;
                self.intr = 0;
                self.stat &= STAT_TC | STAT_PHASE;
                val
            }
            0x6 => self.seq,
            0x7 => self.fifo.len() as u8,
            0x8 => self.cfg1,
            0xB => self.cfg2,
            0xC => self.cfg3,
            0xE => (self.tc >> 16) as u8,
            _ => 0,
        })
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        let reg = (addr >> 4) & 0xF;

        match reg {
            0x0 => {
                self.stc = (self.stc & 0xFF_FF00) | u32::from(val);
                self.stat &= !STAT_TC;
            }
            0x1 => {
                self.stc = (self.stc & 0xFF_00FF) | (u32::from(val) << 8);
                self.stat &= !STAT_TC;
            }
            0x2 => {
                if self.selected && self.phase() == PHASE_COMMAND {
                    self.command_byte(val);
                } else {
                    self.fifo_push(val);
                }
            }
            0x3 => {
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
            0xE => {
                self.stc = (self.stc & 0x00_FFFF) | (u32::from(val) << 16);
                self.stat &= !STAT_TC;
            }
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
