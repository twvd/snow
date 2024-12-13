pub mod drive;
pub mod iwm;

use snow_floppy::flux::FluxTicks;

enum FluxTransitionTime {
    /// 1
    Short,
    /// 01
    Medium,
    /// 001
    Long,
    /// Something else, out of spec.
    /// Contains the amount of bit cells
    OutOfSpec(usize),
}

impl FluxTransitionTime {
    pub fn from_ticks_ex(ticks: FluxTicks, _fast: bool, _highf: bool) -> Option<Self> {
        // Below is from Integrated Woz Machine (IWM) Specification, 1982, rev 19, page 4.
        // TODO fast/low frequency mode.. The Mac SE sets mode to 0x17, which makes things not work?
        match (true, true) {
            (false, false) | (true, false) => match ticks {
                7..=20 => Some(Self::Short),
                21..=34 => Some(Self::Medium),
                35..=48 => Some(Self::Long),
                56.. => Some(Self::OutOfSpec(ticks as usize / 14)),
                _ => None,
            },
            (true, true) | (false, true) => match ticks {
                8..=23 => Some(Self::Short),
                24..=39 => Some(Self::Medium),
                40..=55 => Some(Self::Long),
                56.. => Some(Self::OutOfSpec(ticks as usize / 16)),
                _ => None,
            },
        }
    }

    #[allow(dead_code)]
    pub fn from_ticks(ticks: FluxTicks) -> Option<Self> {
        Self::from_ticks_ex(ticks, true, true)
    }

    pub fn get_zeroes(self) -> usize {
        match self {
            Self::Short => 0,
            Self::Medium => 1,
            Self::Long => 2,
            Self::OutOfSpec(bc) => bc - 1,
        }
    }
}
