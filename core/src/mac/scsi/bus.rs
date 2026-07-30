//! SCSI bus: target management and command execution
//!
//! This models a SCSI bus independently from a controller.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use log::*;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::emulator::EmuContext;
use crate::mac::scsi::ScsiCmdResult;
use crate::mac::scsi::cdrom::ScsiTargetCdrom;
use crate::mac::scsi::disk::ScsiTargetDisk;
use crate::mac::scsi::disk_image::DiskImage;
#[cfg(feature = "ethernet")]
use crate::mac::scsi::ethernet::ScsiTargetEthernet;
#[cfg(feature = "printer")]
use crate::mac::scsi::printer::ScsiTargetPrinter;
use crate::mac::scsi::target::ScsiTarget;
use crate::mac::scsi::target::ScsiTargetType;
use crate::mac::scsi::toolbox::BlueSCSI;
use crate::renderer::AudioProvider;
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
}

/// The SCSI bus and the targets attached to it
#[derive(Serialize, Deserialize)]
pub struct ScsiBus {
    /// Selected SCSI ID
    pub(crate) sel_id: usize,

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

    /// Attached targets
    #[serde(with = "BigArray")]
    pub(crate) targets: [Option<Box<dyn ScsiTarget>>; Self::MAX_TARGETS],

    #[serde(skip)]
    toolbox: BlueSCSI,
    scsi_debug: bool,

    #[serde(skip)]
    scsi_trace_cdb: bool,
}

impl ScsiBus {
    pub const MAX_TARGETS: usize = 7;

    pub fn new() -> Self {
        let scsi_trace_cdb = std::env::var("SNOW_SCSI_TRACE_CDB")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false);

        Self {
            sel_id: 0,
            cmdbuf: vec![],
            cmdlen: 0,
            dataout_len: 0,
            responsebuf: VecDeque::default(),
            status: 0,
            targets: Default::default(),
            toolbox: BlueSCSI::default(),
            scsi_debug: false,
            scsi_trace_cdb,
        }
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

    /// Returns the Toolbox-managed image folder of a folder-backed CD-ROM at the
    /// given ID, or None if the ID has no folder-backed CD-ROM attached.
    pub fn get_cdrom_folder(&self, id: usize) -> Option<&Path> {
        self.targets[id].as_ref().and_then(|t| t.cdrom_image_dir())
    }

    /// Builds the BlueSCSI Toolbox device-type map (one byte per SCSI ID) returned
    /// by the LIST_DEVICES metadata subcommand. 0xFF means no device on that ID.
    fn toolbox_device_map(&self) -> [u8; 8] {
        let mut map = [0xFFu8; 8];
        for (id, slot) in map.iter_mut().enumerate().take(Self::MAX_TARGETS) {
            if let Some(target) = self.targets[id].as_ref() {
                *slot = match target.target_type() {
                    ScsiTargetType::Disk => 0x00,  // fixed disk
                    ScsiTargetType::Cdrom => 0x02, // optical / CD-ROM
                    #[cfg(feature = "ethernet")]
                    ScsiTargetType::Ethernet => 0x06, // DaynaPORT
                    #[cfg(feature = "printer")]
                    ScsiTargetType::Printer => 0xFF, // not a Toolbox device type
                };
            }
        }
        map
    }

    pub fn set_shared_dir(&mut self, path: Option<PathBuf>) {
        self.toolbox = BlueSCSI::new(path);
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

    /// Attaches a folder-backed ("Toolbox managed") CD-ROM drive at the given
    /// SCSI ID. The folder's images are exposed to the guest through the BlueSCSI
    /// Toolbox CD commands (LIST_CDS / SET_NEXT_CD / COUNT_CDS) and the first
    /// image found is mounted automatically.
    pub fn attach_cdrom_folder_at(
        &mut self,
        scsi_id: usize,
        dir: PathBuf,
        audio_provider: Option<&mut (dyn AudioProvider + '_)>,
    ) -> Result<()> {
        if scsi_id >= Self::MAX_TARGETS {
            bail!("SCSI ID out of range: {}", scsi_id);
        }
        let mut cdrom = ScsiTargetCdrom::new(audio_provider);
        cdrom.set_image_dir(Some(dir));
        if let Err(e) = cdrom.mount_first_image() {
            log::warn!("Folder CD-ROM at SCSI ID {}: {:#}", scsi_id, e);
        }
        self.targets[scsi_id] = Some(Box::new(cdrom));
        Ok(())
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

    /// Attaches a LaserWriter IISC printer at the given SCSI ID
    #[cfg(feature = "printer")]
    pub fn attach_printer_at(&mut self, scsi_id: usize, output_dir: std::path::PathBuf) {
        self.targets[scsi_id] = Some(Box::new(ScsiTargetPrinter::new(output_dir)));
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

    /// Ticks every attached target
    pub(crate) fn tick_targets(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<()> {
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
            0xD0..=0xD6 | 0xD9 => {
                let toolbox_devices = self.toolbox_device_map();
                Ok(self.toolbox.handle_command(
                    cmd,
                    outdata,
                    &mut self.scsi_debug,
                    &toolbox_devices,
                ))
            }
            _ => {
                let Some(target) = self.targets[self.sel_id].as_mut() else {
                    bail!("SCSI command to disconnected target ID {}", self.sel_id);
                };
                target.cmd(cmd, outdata)
            }
        };

        if self.scsi_trace_cdb {
            match &result {
                Ok(ScsiCmdResult::Status(s)) => {
                    debug!("SCSI CDB {:02X} -> Status({:02X})", cmd_op, s);
                }
                Ok(ScsiCmdResult::DataIn(d)) => {
                    debug!("SCSI CDB {:02X} -> DataIn {}B", cmd_op, d.len());
                }
                Ok(ScsiCmdResult::DataOut(n)) => {
                    debug!("SCSI CDB {:02X} -> DataOut {}B", cmd_op, n);
                }
                Err(e) => debug!("SCSI CDB {:02X} -> Err: {:#}", cmd_op, e),
            }
        }

        match result? {
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
        }
    }
}

impl Default for ScsiBus {
    fn default() -> Self {
        Self::new()
    }
}
