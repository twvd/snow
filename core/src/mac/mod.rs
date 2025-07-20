use std::fmt::Display;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use swim::drive::DriveType;

use crate::bus::Address;
use crate::cpu_m68k::{CpuM68kType, M68000, M68020};
use crate::keymap::Keymap;
use crate::tickable::Ticks;

pub mod adb;
pub mod asc;
pub mod compact;
pub mod macii;
pub mod nubus;
pub mod pluskbd;
pub mod rtc;
pub mod scc;
pub mod scsi;
pub mod swim;
pub mod via;

/// Differentiation of Macintosh models and their features
#[derive(
    Debug, Copy, Clone, PartialOrd, Ord, PartialEq, Eq, strum::EnumIter, Serialize, Deserialize,
)]
pub enum MacModel {
    /// Macintosh 128K
    Early128K,
    /// Macintosh 512K
    Early512K,
    /// Macintosh 512Ke
    Early512Ke,
    /// Macintosh Plus
    Plus,
    /// Macintosh SE
    SE,
    /// Macintosh SE (FDHD)
    SeFdhd,
    /// Macintosh Classic
    Classic,
    /// Macintosh II
    MacII,
    /// Macintosh II FDHD
    MacIIFDHD,
}

#[allow(clippy::match_like_matches_macro)]
impl MacModel {
    #[rustfmt::skip]
    const ROMS: &[(&str, &[Self])] = &[
        // Macintosh 128K
        ("13fe8312cf6167a2bb4351297b48cc1ee29c523b788e58270434742bfeda864c", &[Self::Early128K]),
        // Macintosh 512K
        ("fe6a1ceff5b3eefe32f20efea967cdf8cd4cada291ede040600e7f6c9e2dfc0e", &[Self::Early512K]),
        // Macintosh Plus v1
        ("c5d862605867381af6200dd52f5004cc00304a36ab996531f15e0b1f8a80bc01", &[Self::Plus, Self::Early512Ke]),
        // Macintosh Plus v2
        ("06f598ff0f64c944e7c347ba55ae60c792824c09c74f4a55a32c0141bf91b8b3", &[Self::Plus, Self::Early512Ke]),
        // Macintosh Plus v3
        ("dd908e2b65772a6b1f0c859c24e9a0d3dcde17b1c6a24f4abd8955846d7895e7", &[Self::Plus, Self::Early512Ke]),
        // Macintosh Plus Japanese ROM
        ("969269ced56dcb76402f2bc32e4d41343b5af00e5ad828e6f08098d5e4b1ad05", &[Self::Plus, Self::Early512Ke]),
        // Macintosh SE
        ("0dea05180e66fddb5f5577c89418de31b97e2d9dc6affe84871b031df8245487", &[Self::SE]),
        // Macintosh SE FDHD
        ("bb0cb4786e2e004b701dda9bec475598bc82a4f27eb7b11e6b78dfcee1434f71", &[Self::SeFdhd]),
        // Macintosh Classic
        ("c1c47260bacac2473e21849925fbfdf48e5ab584aaef7c6d54569d0cb6b41cce", &[Self::Classic]),
        // Macintosh II v1
        ("cc6d754cfa7841644971718ada1121bc5f94ff954918f502a75abb0e6fd90540", &[Self::MacII]),
        // Macintosh II v2
        ("97f2a22bdb8972bfcc1f16aff1ebbe157887c26787a1c81747a9842fa7b97a06", &[Self::MacII]),
        // Macintosh II FDHD
        ("79fae48e2d5cfde68520e46616503963f8c16430903f410514b62c1379af20cb", &[Self::MacIIFDHD]),
    ];

    pub const fn has_adb(self) -> bool {
        match self {
            Self::Early128K | Self::Early512K | Self::Early512Ke | Self::Plus => false,
            _ => true,
        }
    }

    pub const fn ram_size(self) -> usize {
        match self {
            Self::Early128K => 128 * 1024,
            Self::Early512K | Self::Early512Ke => 512 * 1024,
            Self::Plus | Self::SE | Self::SeFdhd | Self::Classic => 4096 * 1024,
            Self::MacII | Self::MacIIFDHD => 8 * 1024 * 1024,
        }
    }

    /// Supports high-density floppies, implying SWIM controller
    pub const fn fdd_hd(self) -> bool {
        match self {
            Self::Early128K | Self::Early512K | Self::Early512Ke | Self::Plus | Self::SE => false,
            Self::SeFdhd | Self::Classic => true,
            Self::MacII => false,
            Self::MacIIFDHD => true,
        }
    }

    /// List of FDD drive types and amount
    pub const fn fdd_drives(self) -> &'static [DriveType] {
        match self {
            Self::Early128K | Self::Early512K => &[DriveType::GCR400K, DriveType::GCR400K],
            Self::Early512Ke | Self::Plus => &[DriveType::GCR800K, DriveType::GCR800K],
            Self::SE => &[DriveType::GCR800K, DriveType::GCR800K, DriveType::GCR800K],
            Self::SeFdhd => &[
                DriveType::SuperDrive,
                DriveType::SuperDrive,
                DriveType::SuperDrive,
            ],
            Self::Classic => &[DriveType::SuperDrive, DriveType::SuperDrive],
            Self::MacII => &[DriveType::GCR800K, DriveType::GCR800K],
            Self::MacIIFDHD => &[DriveType::SuperDrive, DriveType::SuperDrive],
        }
    }

    pub const fn has_scsi(self) -> bool {
        match self {
            Self::Early128K | Self::Early512K | Self::Early512Ke => false,
            _ => true,
        }
    }

    pub const fn keymap(self) -> Keymap {
        match self {
            Self::Early128K | Self::Early512K | Self::Early512Ke | Self::Plus => Keymap::AkM0110,
            _ => Keymap::AekM0115,
        }
    }

    /// Memory controller interleave timing for shared access between CPU and video circuit
    /// Returns true if the CPU has access
    pub const fn ram_interleave_cpu(self, cycles: Ticks) -> bool {
        match self {
            // 50/50 ratio for early macs
            Self::Early128K | Self::Early512K | Self::Early512Ke | Self::Plus => cycles % 8 >= 4,
            // 75/25 for SE and onwards
            Self::SE | Self::SeFdhd | Self::Classic => cycles % 16 >= 4,
            // No interleave for MacII
            Self::MacII | Self::MacIIFDHD => true,
        }
    }

    pub const fn disable_memtest(self) -> Option<(Address, u32)> {
        match self {
            Self::Early128K | Self::Early512K => None,
            Self::Early512Ke | Self::Plus => Some((0x0002AE, 0x0040_0000)),
            Self::SE | Self::SeFdhd | Self::Classic | Self::MacII | Self::MacIIFDHD => {
                Some((0x000CFC, 0x574C5343))
            }
        }
    }

    pub const fn cpu_type(self) -> CpuM68kType {
        match self {
            Self::Early128K
            | Self::Early512K
            | Self::Early512Ke
            | Self::Plus
            | Self::SE
            | Self::SeFdhd
            | Self::Classic => M68000,
            Self::MacII | Self::MacIIFDHD => M68020,
        }
    }
}

impl MacModel {
    fn rom_digest(rom: &[u8]) -> String {
        let mut hash = Sha256::new();
        hash.update(rom);
        let digest = hash.finalize();
        hex::encode(digest)
    }

    /// Detects the Mac model from a given ROM file
    pub fn detect_from_rom(rom: &[u8]) -> Option<Self> {
        let digest = Self::rom_digest(rom);

        // For auto-detect, only return the first model in the list so e.g.
        // the Plus is chosen for the Plus ROMs and not the 512Ke
        Self::ROMS
            .iter()
            .find(|(h, _)| h == &digest)
            .map(|(_, models)| models[0])
    }

    /// Checks whether the provided ROM is valid for this model
    pub fn is_valid_rom(&self, rom: &[u8]) -> bool {
        let digest = Self::rom_digest(rom);

        Self::ROMS
            .iter()
            .any(|(h, models)| h == &digest && models.contains(self))
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
                Self::Early512Ke => "Macintosh 512Ke",
                Self::Plus => "Macintosh Plus",
                Self::SE => "Macintosh SE",
                Self::SeFdhd => "Macintosh SE (FDHD)",
                Self::Classic => "Macintosh Classic",
                Self::MacII => "Macintosh II",
                Self::MacIIFDHD => "Macintosh II (FDHD)",
            }
        )
    }
}

/// Extra ROMs required/optional for some models
pub enum ExtraROMs<'a> {
    /// Macintosh Display Card 8-24
    MDC12(&'a [u8]),
    /// Extension ROM
    ExtensionROM(&'a [u8]),
}

/// Definitions of Macintosh monitors
#[derive(
    Clone,
    Copy,
    strum::IntoStaticStr,
    Default,
    Serialize,
    Deserialize,
    Debug,
    strum::EnumIter,
    Eq,
    PartialEq,
)]
pub enum MacMonitor {
    /// Macintosh 12" RGB monitor
    RGB12,
    /// Macintosh 14" high-res
    #[default]
    HiRes14,
    /// Macintosh 21" RGB monitor (1152x870)
    RGB21,
}

impl Display for MacMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}x{})",
            match self {
                Self::RGB12 => "Macintosh 12\" RGB monitor",
                Self::HiRes14 => "Macintosh 14\" high-resolution",
                Self::RGB21 => "Macintosh 21\" RGB monitor",
            },
            self.width(),
            self.height()
        )
    }
}

impl MacMonitor {
    pub fn sense(self) -> [u8; 4] {
        match self {
            Self::RGB12 => [2, 2, 0, 2],
            Self::HiRes14 => [6, 2, 4, 6],
            Self::RGB21 => [0, 0, 0, 0],
        }
    }

    pub fn width(self) -> usize {
        match self {
            Self::RGB12 => 512,
            Self::HiRes14 => 640,
            Self::RGB21 => 1152,
        }
    }

    pub fn height(self) -> usize {
        match self {
            Self::RGB12 => 384,
            Self::HiRes14 => 480,
            Self::RGB21 => 870,
        }
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
