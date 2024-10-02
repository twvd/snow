use std::fmt::Display;

use crate::keymap::Keymap;

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

    pub const fn keymap(self) -> Keymap {
        match self {
            Self::Early128K | Self::Early512K | Self::Plus => Keymap::AkM0110,
            _ => Keymap::AekM0115,
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
