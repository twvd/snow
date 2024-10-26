pub mod loaders;
mod macformat;

use std::collections::HashMap;

use log::*;
use strum::EnumIter;

/// Key/value collection of floppy metadata.
/// Loaders should convert keys to lowercase.
pub type FloppyMetadata = HashMap<String, String>;

/// Types of emulated floppies - 3.5" only
#[derive(Copy, Clone, EnumIter)]
pub enum FloppyType {
    /// Macintosh CLV 3.5", single sided
    Mac400K,
    /// Macintosh CLV 3.5", double sided
    Mac800K,
}

impl FloppyType {
    /// Gets the (approximate) track length in bits
    pub fn get_approx_track_length(self, track: usize) -> usize {
        match self {
            Self::Mac400K | Self::Mac800K => match track {
                0..=15 => 74640,
                16..=31 => 68240,
                32..=47 => 62200,
                48..=63 => 55980,
                64..=79 => 49760,
                _ => unreachable!(),
            },
        }
    }

    /// Gets the sector size for this format
    pub fn get_sector_size(self) -> usize {
        512
    }

    /// Gets the amount of sectors for this format
    pub fn get_sector_count(self) -> usize {
        self.get_logical_size() / self.get_sector_size()
    }

    /// Gets the logical size of the total image
    pub fn get_logical_size(self) -> usize {
        match self {
            Self::Mac400K => 409600,
            Self::Mac800K => 819200,
        }
    }
}

impl std::fmt::Display for FloppyType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Mac400K => "Macintosh GCR 400KB",
                Self::Mac800K => "Macintosh GCR 800KB",
            }
        )
    }
}

const FLOPPY_MAX_SIDES: usize = 2;
const FLOPPY_MAX_TRACKS: usize = 80;

/// A read/writable floppy interface that can be used with Snow
/// This exposes the physical disk bits, as they are outputted by the IWM
pub trait Floppy {
    /// Gets the type of the emulated floppy
    fn get_type(&self) -> FloppyType;

    /// Gets the amount of tracks per side
    fn get_track_count(&self) -> usize;

    /// Gets a specific bit on a track and side
    fn get_track_bit(&self, side: usize, track: usize, position: usize) -> bool;

    /// Sets a specific bit on a track and side
    fn set_track_bit(&mut self, side: usize, track: usize, position: usize, value: bool);

    /// Gets the length of a specific track, in bits
    fn get_track_length(&self, side: usize, track: usize) -> usize;

    /// Gets the amount of sides on the floppy
    fn get_side_count(&self) -> usize;

    /// A generic, user readable identification of the image
    /// For example: the title, label or filename
    fn get_title(&self) -> &str;

    /// Gets the metadata of the image as key/value.
    /// May be empty if the initial format did not support it or there is no metadata.
    fn get_metadata(&self) -> FloppyMetadata;
}

/// An in-memory loaded floppy image
pub struct FloppyImage {
    floppy_type: FloppyType,
    pub(crate) trackdata: [[Vec<u8>; FLOPPY_MAX_TRACKS]; FLOPPY_MAX_SIDES],
    bitlen: [[usize; FLOPPY_MAX_TRACKS]; FLOPPY_MAX_SIDES],
    title: String,
    metadata: FloppyMetadata,
}

impl FloppyImage {
    /// Creates a new, empty image for the specified type
    /// Tracks are sized to their approximate size
    pub fn new(floppy_type: FloppyType, title: &str) -> Self {
        let mut img = Self::new_empty(floppy_type, title);
        for side in 0..img.get_side_count() {
            for track in 0..img.get_track_count() {
                img.set_actual_track_length(
                    side,
                    track,
                    floppy_type.get_approx_track_length(track),
                );
            }
        }
        img
    }

    /// Creates a new, empty image for the specified type
    /// Tracks are sized to empty so they can be filled
    pub fn new_empty(floppy_type: FloppyType, title: &str) -> Self {
        Self {
            floppy_type,
            trackdata: core::array::from_fn(|_| core::array::from_fn(|_| vec![])),
            bitlen: [[0; FLOPPY_MAX_TRACKS]; FLOPPY_MAX_SIDES],
            title: title.to_owned(),
            metadata: FloppyMetadata::from([("title".to_string(), title.to_string())]),
        }
    }

    /// Resizes the length of a track to the actual size used in the image
    pub(crate) fn set_actual_track_length(&mut self, side: usize, track: usize, sz: usize) {
        let old_sz = self.get_track_length(side, track);
        let perc_inc = (100
            - (std::cmp::min(sz as isize, old_sz as isize) * 100)
                / std::cmp::max(sz as isize, old_sz as isize))
        .wrapping_abs();

        if old_sz != 0 && perc_inc >= 10 {
            warn!(
                "Side {} track {}: length changed by {}% ({} -> {})",
                side, track, perc_inc, old_sz, sz
            );
        }
        self.bitlen[side][track] = sz;
        self.trackdata[side][track].resize(sz / 8 + 1, 0);
    }

    pub(crate) fn push_byte(&mut self, side: usize, track: usize, byte: u8) {
        self.bitlen[side][track] += 8;
        self.trackdata[side][track].push(byte);
    }

    pub(crate) fn set_metadata(&mut self, key: &str, val: &str) {
        self.metadata.insert(key.to_lowercase(), val.to_string());
    }
}

impl Floppy for FloppyImage {
    fn get_type(&self) -> FloppyType {
        self.floppy_type
    }

    fn get_track_count(&self) -> usize {
        80
    }

    fn get_track_bit(&self, side: usize, track: usize, position: usize) -> bool {
        let byte = position / 8;
        let bit = 7 - position % 8;
        self.trackdata[side][track][byte] & (1 << bit) != 0
    }

    fn set_track_bit(&mut self, side: usize, track: usize, position: usize, value: bool) {
        let byte = position / 8;
        let bit = 7 - position % 8;

        self.trackdata[side][track][byte] &= !(1 << bit);
        if value {
            self.trackdata[side][track][byte] |= 1 << bit;
        }
    }

    fn get_track_length(&self, side: usize, track: usize) -> usize {
        self.bitlen[side][track]
    }

    fn get_side_count(&self) -> usize {
        match self.floppy_type {
            FloppyType::Mac400K => 1,
            FloppyType::Mac800K => 2,
        }
    }

    fn get_title(&self) -> &str {
        &self.title
    }

    fn get_metadata(&self) -> FloppyMetadata {
        self.metadata.clone()
    }
}
