use std::fmt::Display;

use crate::{bus::Address, keymap::Keymap, tickable::Ticks};

pub mod adb;
pub mod audio;
pub mod bus;
pub mod iwm;
pub mod pluskbd;
pub mod rtc;
pub mod scc;
pub mod scsi;
pub mod via;
pub mod video;

/// Differentiation of Macintosh models and their features
#[derive(Debug, Copy, Clone, PartialOrd, Ord, PartialEq, Eq)]
pub enum MacModel {
    /// Macintosh 128K
    Early128K,
    /// Macintosh 512K
    Early512K,
    /// Macintosh Plus
    Plus,
    /// Macintosh SE
    SE,
    /// Macintosh Classic
    Classic,
}

#[allow(clippy::match_like_matches_macro)]
impl MacModel {
    pub const fn has_adb(self) -> bool {
        match self {
            Self::Early128K | Self::Early512K | Self::Plus => false,
            _ => true,
        }
    }

    pub const fn ram_size(self) -> usize {
        match self {
            Self::Early128K => 128 * 1024,
            Self::Early512K => 512 * 1024,
            Self::Plus | Self::SE | Self::Classic => 4096 * 1024,
        }
    }

    pub const fn fdd_double_sided(self) -> bool {
        match self {
            Self::Early128K | Self::Early512K => false,
            _ => true,
        }
    }

    pub const fn fdd_count(self) -> usize {
        match self {
            Self::SE => 3,
            _ => 2,
        }
    }

    pub const fn has_scsi(self) -> bool {
        match self {
            Self::Early128K | Self::Early512K => false,
            _ => true,
        }
    }

    pub const fn keymap(self) -> Keymap {
        match self {
            Self::Early128K | Self::Early512K | Self::Plus => Keymap::AkM0110,
            _ => Keymap::AekM0115,
        }
    }

    /// Memory controller interleave timing for shared access between CPU and video circuit
    /// Returns true if the CPU has access
    pub const fn ram_interleave_cpu(self, cycles: Ticks) -> bool {
        match self {
            // 50/50 ratio for early macs
            Self::Early128K | Self::Early512K | Self::Plus => cycles % 8 >= 4,
            // 75/25 for SE and onwards
            Self::SE | Self::Classic => cycles % 16 >= 4,
        }
    }

    pub const fn disable_memtest(self) -> Option<(Address, u32)> {
        match self {
            Self::Early128K | Self::Early512K => None,
            Self::Plus => Some((0x0002AE, 0x0040_0000)),
            Self::SE | Self::Classic => Some((0x000CFC, 0x574C5343)),
        }
    }
}

impl Display for MacModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Early128K => "Macintosh 128K",
                Self::Early512K => "Macintosh 512K",
                Self::Plus => "Macintosh Plus",
                Self::SE => "Macintosh SE",
                Self::Classic => "Macintosh Classic",
            }
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_early_plus() {
        for m in &[MacModel::Early128K, MacModel::Early512K, MacModel::Plus] {
            assert!(!m.ram_interleave_cpu(0));
            assert!(!m.ram_interleave_cpu(1));
            assert!(!m.ram_interleave_cpu(2));
            assert!(!m.ram_interleave_cpu(3));
            assert!(m.ram_interleave_cpu(4));
            assert!(m.ram_interleave_cpu(5));
            assert!(m.ram_interleave_cpu(6));
            assert!(m.ram_interleave_cpu(7));
            assert!(!m.ram_interleave_cpu(8));
        }
    }

    #[test]
    fn interleave_se_classic() {
        for m in &[MacModel::SE, MacModel::Classic] {
            assert!(!m.ram_interleave_cpu(0));
            assert!(!m.ram_interleave_cpu(1));
            assert!(!m.ram_interleave_cpu(2));
            assert!(!m.ram_interleave_cpu(3));
            assert!(m.ram_interleave_cpu(4));
            assert!(m.ram_interleave_cpu(5));
            assert!(m.ram_interleave_cpu(6));
            assert!(m.ram_interleave_cpu(7));
            assert!(m.ram_interleave_cpu(8));
            assert!(m.ram_interleave_cpu(9));
            assert!(m.ram_interleave_cpu(10));
            assert!(m.ram_interleave_cpu(11));
            assert!(m.ram_interleave_cpu(12));
            assert!(m.ram_interleave_cpu(13));
            assert!(m.ram_interleave_cpu(14));
            assert!(m.ram_interleave_cpu(15));
            assert!(!m.ram_interleave_cpu(16));
        }
    }
}
