//! Test helpers: a SCSI target whose reads complete in the background, when
//! the test says so.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::emulator::comm::EmulatorSpeed;
use crate::mac::scsi::disk_image::DiskImage;
use crate::mac::scsi::target::{ScsiTarget, ScsiTargetCommon, ScsiTargetType};
use crate::mac::scsi::{STATUS_GOOD, ScsiCmdResult};
use crate::tickable::Ticks;

pub const SLOW_BLOCKSIZE: usize = 512;

/// What the test sees of a `SlowTarget`
#[derive(Default)]
pub struct SlowState {
    /// A read is waiting for the test
    pub reading: bool,
    /// Data to complete the read with, set by the test
    pub release: Option<Vec<u8>>,
    /// Times a pending read was aborted
    pub aborts: usize,
}

/// A 16-block disk whose reads return Pending until the test sets `release`
#[derive(Default, Serialize, Deserialize)]
pub struct SlowTarget {
    common: ScsiTargetCommon,
    #[serde(skip)]
    pub state: Arc<Mutex<SlowState>>,
}

impl SlowTarget {
    /// The target, and the state the test drives it with
    pub fn new() -> (Box<Self>, Arc<Mutex<SlowState>>) {
        let target = Box::new(Self::default());
        let state = target.state.clone();
        (target, state)
    }
}

impl Debuggable for SlowTarget {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        vec![]
    }
}

#[typetag::serde]
impl ScsiTarget for SlowTarget {
    fn common(&mut self) -> &mut ScsiTargetCommon {
        &mut self.common
    }

    #[cfg(feature = "savestates")]
    fn after_deserialize(&mut self, _imgfn: &Path) -> Result<()> {
        Ok(())
    }

    fn set_blocksize(&mut self, _blocksize: usize) -> bool {
        false
    }

    fn take_event(&mut self) -> Option<super::target::ScsiTargetEvent> {
        None
    }

    fn target_type(&self) -> ScsiTargetType {
        ScsiTargetType::Disk
    }

    fn unit_ready(&mut self) -> Result<ScsiCmdResult> {
        Ok(ScsiCmdResult::Status(STATUS_GOOD))
    }

    fn inquiry(&mut self, _cmd: &[u8]) -> Result<ScsiCmdResult> {
        Ok(ScsiCmdResult::DataIn(vec![0; 36]))
    }

    fn mode_sense(&mut self, _page: u8) -> Option<Vec<u8>> {
        None
    }

    fn ms_density(&self) -> u8 {
        0
    }

    fn ms_media_type(&self) -> u8 {
        0
    }

    fn ms_device_specific(&self) -> u8 {
        0
    }

    #[cfg(feature = "ethernet")]
    fn eth_set_link(&mut self, _link: super::ethernet::EthernetLinkType) -> Result<()> {
        unreachable!()
    }

    #[cfg(feature = "ethernet")]
    fn eth_link(&self) -> Option<super::ethernet::EthernetLinkType> {
        None
    }

    fn blocksize(&self) -> Option<usize> {
        Some(SLOW_BLOCKSIZE)
    }

    fn blocks(&self) -> Option<usize> {
        Some(16)
    }

    fn read(&mut self, _block_offset: usize, _block_count: usize) -> Result<ScsiCmdResult> {
        let mut state = self.state.lock().unwrap();
        assert!(!state.reading, "read while another one is pending");
        state.reading = true;
        drop(state);
        Ok(ScsiCmdResult::Pending)
    }

    fn poll_pending(&mut self) -> Option<Result<ScsiCmdResult>> {
        let mut state = self.state.lock().unwrap();
        assert!(state.reading, "poll without a pending read");
        let data = state.release.take()?;
        state.reading = false;
        drop(state);
        Some(Ok(ScsiCmdResult::DataIn(data)))
    }

    fn abort_pending(&mut self) {
        let mut state = self.state.lock().unwrap();
        assert!(state.reading, "abort without a pending read");
        state.reading = false;
        state.release = None;
        state.aborts += 1;
    }

    fn write(&mut self, _block_offset: usize, _data: &[u8]) {}

    fn image_fn(&self) -> Option<&Path> {
        None
    }

    fn load_media(&mut self, _path: &Path) -> Result<()> {
        bail!("not removable")
    }

    fn load_image(&mut self, _image: Box<dyn DiskImage>) -> Result<()> {
        bail!("not removable")
    }

    fn branch_media(&mut self, _path: &Path) -> Result<()> {
        bail!("not supported")
    }

    fn specific_cmd(&mut self, _cmd: &[u8], _outdata: Option<&[u8]>) -> Result<ScsiCmdResult> {
        bail!("not supported")
    }
}

/// Enough of an emulator for ticking SCSI controllers
pub struct TestContext;

impl EmuContext for TestContext {
    fn speed(&self) -> EmulatorSpeed {
        EmulatorSpeed::Accurate
    }

    fn bus_frequency(&self) -> Ticks {
        16_000_000
    }
}
