pub mod loaders;

/// Types of emulated floppies - 3.5" only
#[derive(Copy, Clone)]
pub enum FloppyType {
    /// Macintosh CLV 3.5", single sided
    Mac400K,
    /// Macintosh CLV 3.5", double sided
    Mac800K,
}

impl FloppyType {
    /// Gets the track length in bits
    pub fn get_track_length(self, track: usize) -> usize {
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
    fn get_track_length(&self, track: usize) -> usize;

    /// Gets the amount of sides on the floppy
    fn get_side_count(&self) -> usize;
}

/// An in-memory loaded floppy image
pub struct FloppyImage {
    floppy_type: FloppyType,
    pub(crate) trackdata: [[Vec<u8>; FLOPPY_MAX_TRACKS]; FLOPPY_MAX_SIDES],
}

impl FloppyImage {
    /// Creates a new, empty image for the specified type
    pub fn new(floppy_type: FloppyType) -> Self {
        Self {
            floppy_type,
            trackdata: core::array::from_fn(|_| {
                core::array::from_fn(|t| vec![0; floppy_type.get_track_length(t)])
            }),
        }
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

    fn get_track_length(&self, track: usize) -> usize {
        self.get_type().get_track_length(track)
    }

    fn get_side_count(&self) -> usize {
        match self.floppy_type {
            FloppyType::Mac400K => 1,
            FloppyType::Mac800K => 2,
        }
    }
}
