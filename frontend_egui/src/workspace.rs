use std::{fs::File, io::BufReader, path::Path};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A workspace representation which contains:
/// * (Paths to) loaded assets
/// * View configuration of the egui frontend
/// * ..probably some hardware configuration
///
/// It does not contain the state of the running emulator, but mirrors
/// part of it. It can be used to re-construct a previously running
/// emulator.
#[derive(Default, Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Workspace {
    pub log_open: bool,
    pub disassembly_open: bool,
    pub registers_open: bool,
    pub breakpoints_open: bool,
    pub center_viewport_v: bool,
    pub viewport_scale: f32,
}

impl Workspace {
    pub fn from_file(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        Ok(serde_json::from_reader(reader)?)
    }

    pub fn to_file(&self, path: &Path) -> Result<()> {
        let file = File::create(path)?;
        Ok(serde_json::to_writer_pretty(file, self)?)
    }
}
