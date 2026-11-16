//! SCSI bus: target management and command execution
//!
//! This models a SCSI bus independently from a controller.

use std::collections::VecDeque;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use log::*;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::devices::{BusOps, DeviceOps, FunctionInfo, MadeDevice};
use crate::emulator::EmuContext;
use crate::mac::scsi::target::ScsiTarget;
use crate::mac::scsi::toolbox::BlueSCSI;
use crate::mac::scsi::{
    ASC_LOGICAL_UNIT_NOT_SUPPORTED, CC_KEY_ILLEGAL_REQUEST, STATUS_CHECK_CONDITION, ScsiCmdResult,
};
use crate::tickable::Ticks;

/// What phase the target wants after a command was executed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CmdOutcome {
    /// Command finished, go to the Status phase
    Status,
    /// The target has data for the initiator in the response buffer
    DataIn,
    /// The target expects this many bytes from the initiator
    DataOut(usize),
    /// The target is working on the command in the background: poll() it
    Busy,
}

/// The SCSI bus and the targets attached to it
#[derive(Serialize, Deserialize)]
pub struct ScsiBus {
    /// Selected SCSI ID
    pub(crate) sel_id: usize,

    /// The logical unit of the IDENTIFY message of this selection, when the
    /// initiator sent one
    #[serde(default)]
    pub(crate) sel_lun: Option<u8>,

    /// Command buffer
    pub(crate) cmdbuf: Vec<u8>,

    /// Active command length
    pub(crate) cmdlen: usize,

    /// DataOut phase length
    pub(crate) dataout_len: usize,

    /// Response buffer
    pub(crate) responsebuf: VecDeque<u8>,

    /// Status of the last executed command
    pub(crate) status: u8,

    /// The selected target is running a command in the background. A busy
    /// target holds the bus, so only the selected target can be pending.
    // TODO: investigate whether several SCSI devices can have commands in
    // flight at once (a target disconnecting from the bus during a long
    // command, then reselecting the initiator), and how to support it.
    // TODO: guest time keeps running while a target holds the bus. This is a
    // risk while running in fast-forward: guest time then runs faster than a
    // physical drive, so a slow command (such as a spin-up) can exceed the
    // guest's SCSI timeout, which aborts it with a bus reset. Consider slowing
    // guest time while a command is pending.
    pending: bool,

    /// Attached targets
    #[serde(with = "BigArray")]
    pub(crate) targets: [Option<Box<dyn ScsiTarget>>; Self::MAX_TARGETS],

    /// Whether any target needs ticks, None when to be found out again
    #[serde(skip)]
    ticking: Option<bool>,

    #[serde(skip)]
    toolbox: BlueSCSI,
    scsi_debug: bool,

    #[serde(skip)]
    scsi_trace_cdb: bool,
}

impl ScsiBus {
    pub const MAX_TARGETS: usize = 7;

    /// The ids of the slots of the bus: the SCSI IDs, in decimal
    pub const SLOT_IDS: [&str; Self::MAX_TARGETS] = ["0", "1", "2", "3", "4", "5", "6"];

    pub fn new() -> Self {
        let scsi_trace_cdb = std::env::var("SNOW_SCSI_TRACE_CDB")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false);

        Self {
            sel_id: 0,
            sel_lun: None,
            cmdbuf: vec![],
            cmdlen: 0,
            dataout_len: 0,
            responsebuf: VecDeque::default(),
            status: 0,
            pending: false,
            targets: Default::default(),
            ticking: None,
            toolbox: BlueSCSI::default(),
            scsi_debug: false,
            scsi_trace_cdb,
        }
    }

    /// Builds the BlueSCSI Toolbox device-type map (one byte per SCSI ID) returned
    /// by the LIST_DEVICES metadata subcommand. 0xFF means no device on that ID.
    fn toolbox_device_map(&self) -> [u8; 8] {
        let mut map = [0xFFu8; 8];
        for (id, slot) in map.iter_mut().enumerate().take(Self::MAX_TARGETS) {
            if let Some(target) = self.targets[id].as_ref() {
                *slot = match target.info().function {
                    FunctionInfo::HardDisk => 0x00,    // fixed disk
                    FunctionInfo::Cdrom => 0x02,       // optical / CD-ROM
                    FunctionInfo::Ethernet(_) => 0x06, // DaynaPORT
                    _ => 0xFF,                         // not a Toolbox device type
                };
            }
        }
        map
    }

    pub fn set_shared_dir(&mut self, path: Option<PathBuf>) {
        self.toolbox = BlueSCSI::new(path);
    }

    /// Ticks the attached targets which use ticks
    pub(crate) fn tick_targets(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<()> {
        let targets = &self.targets;
        if !*self
            .ticking
            .get_or_insert_with(|| targets.iter().flatten().any(|t| t.ticks()))
        {
            return Ok(());
        }
        for target in self.targets.iter_mut().flatten() {
            target.tick(ticks, ctx)?;
        }

        Ok(())
    }

    /// Executes the command in the command buffer against the selected target
    /// and reports what should happen on the bus next.
    pub(crate) fn cmd_run(&mut self, outdata: Option<&[u8]>) -> Result<CmdOutcome> {
        let cmd = &self.cmdbuf;
        let Some(cmd_op) = cmd.first().copied() else {
            bail!("SCSI command run with an empty command buffer");
        };

        if self.scsi_trace_cdb {
            debug!(
                "SCSI CDB id={} {:02X?} dataout={}B",
                self.sel_id,
                cmd.as_slice(),
                outdata.map(|d| d.len()).unwrap_or(0)
            );
        }

        // File-sharing/metadata toolbox commands (0xD0-0xD6, 0xD9) are bus-global
        // and handled by the shared BlueSCSI helper. The per-device CD switching
        // commands (LIST_CDS 0xD7 / SET_NEXT_CD 0xD8 / COUNT_CDS 0xDA) fall through
        // to the addressed target, which owns the folder of images and its media.
        let result = match cmd_op {
            _ if self.logical_unit(cmd) != 0 => Ok(Self::unsupported_lun(cmd)),
            0xD0..=0xD6 | 0xD9 => {
                let toolbox_devices = self.toolbox_device_map();
                let r = self.toolbox.handle_command(
                    cmd,
                    outdata,
                    &mut self.scsi_debug,
                    &toolbox_devices,
                );
                // Toolbox commands are bus-global here, but the firmware
                // reports through scsiDev.target->sense, which is where a
                // client's REQUEST SENSE looks.
                if let Some((key, asc)) = self.toolbox.take_pending_sense()
                    && let Some(target) = self.targets[self.sel_id].as_mut()
                {
                    target.common().set_cc(key, asc);
                }
                Ok(r)
            }
            _ => {
                let Some(target) = self.targets[self.sel_id].as_mut() else {
                    bail!("SCSI command to disconnected target ID {}", self.sel_id);
                };
                target.cmd(cmd, outdata)
            }
        };

        self.trace_result(cmd_op, &result);
        self.finish(result?)
    }

    /// The logical unit a command is for. [SCSI-2] 7.2.2: the one of the
    /// IDENTIFY message, else the LUN field of the CDB (bits 7-5 of byte 1),
    /// which SCSI-1 initiators use. Byte 1 of a vendor-specific command
    /// (C0h-FFh) is the vendor's, and the BlueSCSI toolbox commands use it
    /// so: those are for logical unit 0 unless an IDENTIFY says otherwise.
    fn logical_unit(&self, cmd: &[u8]) -> u8 {
        match (self.sel_lun, cmd.first()) {
            (Some(lun), _) => lun,
            (None, Some(0xC0..=0xFF)) => 0,
            (None, _) => cmd.get(1).map_or(0, |b| b >> 5),
        }
    }

    /// The answer to a command for a logical unit other than 0, which no
    /// target has. [SCSI-2] 7.5.3 (a): INQUIRY returns its data with the
    /// peripheral qualifier of 8.2.5.1 (7Fh: no device can be on this logical
    /// unit), REQUEST SENSE returns ILLEGAL REQUEST, LOGICAL UNIT NOT
    /// SUPPORTED, and any other command ends with CHECK CONDITION.
    fn unsupported_lun(cmd: &[u8]) -> ScsiCmdResult {
        let alloc_len = usize::from(cmd.get(4).copied().unwrap_or(0));
        match cmd[0] {
            0x12 => {
                // [SCSI-2] 8.2.5.1: the standard INQUIRY data, 36 bytes
                let mut data = vec![0; 36];
                data[0] = 0x7F;
                data[4] = 31;
                data.truncate(alloc_len);
                ScsiCmdResult::DataIn(data)
            }
            0x03 => {
                // [SCSI-2] 8.2.14.3: current error, additional length 10
                let mut data = vec![0; 18];
                data[0] = 0x70;
                data[2] = CC_KEY_ILLEGAL_REQUEST;
                data[7] = 10;
                data[12..14].copy_from_slice(&ASC_LOGICAL_UNIT_NOT_SUPPORTED.to_be_bytes());
                data.truncate(alloc_len);
                ScsiCmdResult::DataIn(data)
            }
            _ => ScsiCmdResult::Status(STATUS_CHECK_CONDITION),
        }
    }

    /// Whether the selected target is running a command in the background
    pub(crate) fn is_pending(&self) -> bool {
        self.pending
    }

    /// Checks on a command running in the background. Returns None while it
    /// is still running (or if none is), or what should happen on the bus
    /// next. Never blocks.
    pub(crate) fn poll(&mut self) -> Option<Result<CmdOutcome>> {
        if !self.pending {
            return None;
        }
        let result = match self.targets[self.sel_id].as_mut() {
            Some(target) => target.poll_pending()?,
            None => Err(anyhow!(
                "SCSI target ID {} detached during a command",
                self.sel_id
            )),
        };
        Some(self.pending_done(result))
    }

    /// Waits for a command running in the background to finish. Returns None
    /// if none is running, or what should happen on the bus next. Blocks: only
    /// for saving a state.
    pub(crate) fn wait(&mut self) -> Option<Result<CmdOutcome>> {
        if !self.pending {
            return None;
        }
        let result = match self.targets[self.sel_id].as_mut() {
            Some(target) => target.wait_pending(),
            None => Err(anyhow!(
                "SCSI target ID {} detached during a command",
                self.sel_id
            )),
        };
        Some(self.pending_done(result))
    }

    /// Drops a command running in the background, as on a bus reset
    pub(crate) fn abort_pending(&mut self) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        if let Some(target) = self.targets[self.sel_id].as_mut() {
            target.abort_pending();
        }
    }

    /// Takes in the result of a command that was running in the background
    fn pending_done(&mut self, result: Result<ScsiCmdResult>) -> Result<CmdOutcome> {
        self.pending = false;
        let cmd_op = self.cmdbuf.first().copied().unwrap_or(0);
        self.trace_result(cmd_op, &result);
        self.finish(result?)
    }

    fn trace_result(&self, cmd_op: u8, result: &Result<ScsiCmdResult>) {
        if !self.scsi_trace_cdb {
            return;
        }
        match result {
            Ok(r) => debug!("SCSI CDB {:02X} -> {:?}", cmd_op, r),
            Err(e) => debug!("SCSI CDB {:02X} -> Err: {:#}", cmd_op, e),
        }
    }

    /// Takes in what the target answered and reports what should happen on
    /// the bus next
    fn finish(&mut self, result: ScsiCmdResult) -> Result<CmdOutcome> {
        match result {
            ScsiCmdResult::Status(s) => {
                self.status = s;
                Ok(CmdOutcome::Status)
            }
            ScsiCmdResult::DataIn(data) => {
                self.status = crate::mac::scsi::STATUS_GOOD;
                self.responsebuf = VecDeque::from(data);
                Ok(CmdOutcome::DataIn)
            }
            ScsiCmdResult::DataOut(len) => {
                self.dataout_len = len;
                self.responsebuf.clear();
                if len == 0 {
                    // [SPC-3] 6.7:
                    // "A parameter list length of zero specifies that the Data-Out Buffer shall be
                    // empty. This condition shall not be considered as an error."
                    self.cmd_run(Some(&[]))
                } else {
                    Ok(CmdOutcome::DataOut(len))
                }
            }
            ScsiCmdResult::Pending => {
                self.pending = true;
                Ok(CmdOutcome::Busy)
            }
        }
    }
}

impl Default for ScsiBus {
    fn default() -> Self {
        Self::new()
    }
}

/// The SCSI ID of a slot
fn slot_number(slot: &str) -> Option<usize> {
    ScsiBus::SLOT_IDS.iter().position(|&id| id == slot)
}

impl BusOps for ScsiBus {
    fn device(&mut self, slot: &str) -> Option<&mut dyn DeviceOps> {
        let target = self.targets[slot_number(slot)?].as_mut()?;
        // A SCSI target is a DeviceOps too
        Some(&mut **target)
    }

    fn attach(&mut self, slot: &str, device: MadeDevice) -> Result<()> {
        let id = slot_number(slot).context("No such SCSI ID")?;
        let MadeDevice::Scsi(target) = device;
        if self.targets[id].is_some() {
            bail!("SCSI ID {} already has a device", id);
        }
        self.targets[id] = Some(target);
        self.ticking = None;
        Ok(())
    }

    fn detach(&mut self, slot: &str) -> Result<()> {
        let id = slot_number(slot).context("No such SCSI ID")?;
        if self.targets[id].take().is_none() {
            bail!("SCSI ID {} has no device", id);
        }
        self.ticking = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::scsi::test_target::{SLOW_BLOCKSIZE, SlowState, SlowTarget};
    use std::sync::{Arc, Mutex};

    /// A bus with a target at ID 0 whose reads wait for the test, selected
    /// with a READ(6) of one block in the command buffer
    fn bus_with_slow_read() -> (ScsiBus, Arc<Mutex<SlowState>>) {
        let mut bus = ScsiBus::new();
        let (target, state) = SlowTarget::new();
        bus.targets[0] = Some(target);
        bus.sel_id = 0;
        bus.cmdbuf = vec![0x08, 0, 0, 0, 1, 0];
        (bus, state)
    }

    fn release(state: &Mutex<SlowState>) {
        state.lock().unwrap().release = Some(vec![0x55; SLOW_BLOCKSIZE]);
    }

    #[test]
    fn pending_command_completes_through_poll() {
        let (mut bus, state) = bus_with_slow_read();
        assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::Busy);
        assert!(bus.is_pending());
        assert!(bus.poll().is_none());

        release(&state);
        assert_eq!(bus.poll().unwrap().unwrap(), CmdOutcome::DataIn);
        assert!(!bus.is_pending());
        assert_eq!(bus.responsebuf.len(), SLOW_BLOCKSIZE);
        assert!(bus.poll().is_none());
    }

    #[test]
    fn pending_command_completes_through_wait() {
        let (mut bus, state) = bus_with_slow_read();
        assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::Busy);

        release(&state);
        assert_eq!(bus.wait().unwrap().unwrap(), CmdOutcome::DataIn);
        assert!(!bus.is_pending());
        assert_eq!(bus.responsebuf.len(), SLOW_BLOCKSIZE);
        assert!(bus.wait().is_none());
    }

    #[test]
    fn aborted_command_is_dropped() {
        let (mut bus, state) = bus_with_slow_read();
        assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::Busy);

        bus.abort_pending();
        assert_eq!(state.lock().unwrap().aborts, 1);
        assert!(!bus.is_pending());
        assert!(bus.poll().is_none());

        // Nothing to abort any more
        bus.abort_pending();
        assert_eq!(state.lock().unwrap().aborts, 1);
    }

    #[test]
    fn detached_target_ends_the_pending_command() {
        let (mut bus, _state) = bus_with_slow_read();
        assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::Busy);

        bus.targets[0] = None;
        assert!(bus.poll().unwrap().is_err());
        assert!(!bus.is_pending());
    }

    #[test]
    fn command_that_does_not_pend_completes_at_once() {
        let mut bus = ScsiBus::new();
        let cdrom = Box::new(crate::mac::scsi::cdrom::ScsiTargetCdrom::default());
        bus.attach("0", MadeDevice::Scsi(cdrom)).unwrap();
        bus.sel_id = 0;
        // INQUIRY of 36 bytes
        bus.cmdbuf = vec![0x12, 0, 0, 0, 36, 0];
        assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::DataIn);
        assert!(!bus.is_pending());
        assert!(bus.poll().is_none());
    }

    /// A bus with a CD-ROM at ID 0, selected with this command
    fn bus_with_command(cmd: &[u8], lun: Option<u8>) -> ScsiBus {
        let mut bus = ScsiBus::new();
        let cdrom = Box::new(crate::mac::scsi::cdrom::ScsiTargetCdrom::default());
        bus.attach("0", MadeDevice::Scsi(cdrom)).unwrap();
        bus.sel_id = 0;
        bus.sel_lun = lun;
        bus.cmdbuf = cmd.to_vec();
        bus
    }

    #[test]
    fn inquiry_of_another_lun_finds_no_device() {
        // LUN 1 in the CDB, and in an IDENTIFY
        for (cdb, lun) in [
            ([0x12, 0x20, 0, 0, 36, 0], None),
            ([0x12, 0, 0, 0, 36, 0], Some(1)),
        ] {
            let mut bus = bus_with_command(&cdb, lun);
            assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::DataIn);
            assert_eq!(bus.responsebuf.len(), 36);
            assert_eq!(bus.responsebuf[0], 0x7F);
        }
        // LUN 0 is the target's
        let mut bus = bus_with_command(&[0x12, 0, 0, 0, 36, 0], Some(0));
        assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::DataIn);
        assert_eq!(bus.responsebuf[0], 0x05);
    }

    #[test]
    fn the_identify_lun_wins_over_the_cdb_lun() {
        let mut bus = bus_with_command(&[0x12, 0x20, 0, 0, 36, 0], Some(0));
        assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::DataIn);
        assert_eq!(bus.responsebuf[0], 0x05);
    }

    #[test]
    fn other_commands_to_another_lun_fail() {
        let mut bus = bus_with_command(&[0x00, 0x40, 0, 0, 0, 0], None);
        assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::Status);
        assert_eq!(bus.status, STATUS_CHECK_CONDITION);

        let mut bus = bus_with_command(&[0x03, 0x40, 0, 0, 18, 0], None);
        assert_eq!(bus.cmd_run(None).unwrap(), CmdOutcome::DataIn);
        let sense: Vec<u8> = bus.responsebuf.iter().copied().collect();
        assert_eq!(sense[2], CC_KEY_ILLEGAL_REQUEST);
        assert_eq!(sense[12..14], ASC_LOGICAL_UNIT_NOT_SUPPORTED.to_be_bytes());
    }

    #[test]
    fn vendor_commands_keep_byte_1() {
        // A BlueSCSI toolbox command with 1 in byte 1 is for LUN 0
        let bus = bus_with_command(&[0xD0, 0x20, 0, 0, 0, 0, 0, 0, 0, 0], None);
        assert_eq!(bus.logical_unit(&bus.cmdbuf), 0);
    }
}
