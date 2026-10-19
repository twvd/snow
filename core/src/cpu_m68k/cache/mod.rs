//! M68k CPU - On-chip caches

use serde::{Deserialize, Serialize};

/// The on-chip caches of a CPU model
pub trait CpuCaches: Default {}

/// No on-chip caches
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NoCaches;

impl CpuCaches for NoCaches {}
