//! The disk under the heads of a floppy drive

use serde::{Deserialize, Serialize};
use snow_floppy::flux::FluxTicks;
use snow_floppy::{Floppy, FloppyImage, FloppyType, TrackLength, TrackType};

use super::WeakBitRng;
use crate::tickable::Ticks;

/// The disk under the heads of a floppy drive, which the controller reads and writes bit by bit.
///
/// This is a struct with ordinary methods, and not a trait, on purpose. The controller reads
/// the bits of a disk one by one for as long as the drive turns: a method of this struct is a
/// direct call, which the compiler can inline.
///
/// The fields are for the drive the stream belongs to, which moves the head and changes the
/// disk. The controller only calls the methods.
#[derive(Serialize, Deserialize)]
pub struct FloppyStream {
    /// The disk in the drive
    pub(super) floppy: FloppyImage,

    /// The track under the heads
    pub(super) track: usize,

    /// The position on the track: a bit of a bitstream track, a transition of a flux track
    pub(super) track_position: usize,

    /// Amount of ticks a physical bit is under the drive head
    pub(super) ticks_per_bit: Ticks,

    /// The disk spins under the heads: the controller reads and writes it only then
    pub(super) spinning: bool,

    /// The drive is not ready (e.g. stepping, spindle motor changing speed): its heads read
    /// garbage
    pub(super) not_ready: bool,

    /// The drive has two heads
    double_sided: bool,

    /// Amount of flux ticks for current transition (for flux tracks)
    pub(super) flux_ticks: FluxTicks,

    /// Amount of flux ticks left for current transition (for flux tracks)
    pub(super) flux_ticks_left: FluxTicks,

    head_bit: [bool; 2],

    /// Noise source for garbage read while the drive is not ready, and for the weak bits the
    /// controller reads
    weak_bit_rng: WeakBitRng,
}

impl FloppyStream {
    pub(super) fn new(double_sided: bool) -> Self {
        Self {
            floppy: FloppyImage::new(FloppyType::Mac400K, ""),
            track: 4,
            track_position: 0,
            ticks_per_bit: Ticks::MAX,
            spinning: false,
            not_ready: false,
            double_sided,
            flux_ticks: 0,
            flux_ticks_left: 0,
            head_bit: [false; 2],
            weak_bit_rng: WeakBitRng::default(),
        }
    }

    /// Seeds the noise source, for runs that must repeat. `None` reverts to non-deterministic
    /// randomness.
    pub(super) fn seed_weak_bits(&self, seed: Option<u64>) {
        self.weak_bit_rng.seed(seed);
    }

    /// Gets the amount of ticks a physical bit is under the drive head
    #[inline]
    pub fn ticks_per_bit(&self) -> Ticks {
        self.ticks_per_bit
    }

    /// Gets the head that reads and writes, from the level of the SEL line
    #[inline]
    pub fn head(&self, sel: bool) -> usize {
        if !self.double_sided || self.floppy.get_side_count() == 1 || !sel {
            0
        } else {
            1
        }
    }

    /// Gets the track under the heads
    pub fn track(&self) -> usize {
        self.track
    }

    /// Gets the type of the track under a head
    #[inline]
    pub fn track_type(&self, head: usize) -> TrackType {
        self.floppy.get_track_type(head, self.track)
    }

    /// Gets a bit of noise, as a head reads when there is nothing to read
    #[inline]
    pub fn weak_bit(&self) -> bool {
        self.weak_bit_rng.random_bool()
    }

    /// Gets the physical disk bit currently under a head
    pub(super) fn head_bit(&self, head: usize) -> bool {
        match self.floppy.get_track_type(head, self.track) {
            TrackType::Bitstream => {
                let TrackLength::Bits(tracklen) = self.floppy.get_track_length(head, self.track)
                else {
                    unreachable!()
                };
                // Extra modulus here because RDDATAx can be read while the controller is currently
                // has the other side selected or we just changed tracks.
                self.floppy
                    .get_track_bit(head, self.track, self.track_position % tracklen)
            }
            TrackType::Flux => self.head_bit[head],
        }
    }

    /// Advances to the next bit on the track (bitstream tracks)
    #[inline]
    pub fn next_bit(&mut self, head: usize) -> bool {
        let TrackLength::Bits(tracklen) = self.floppy.get_track_length(head, self.track) else {
            unreachable!()
        };
        self.track_position = (self.track_position + 1) % tracklen;

        // Garbage if drive is not yet ready
        if self.not_ready {
            return self.weak_bit();
        }
        self.head_bit(head)
    }

    /// Writes a bit to the current track position
    #[inline]
    pub fn write_bit(&mut self, head: usize, bit: bool) {
        self.floppy
            .set_track_bit(head, self.track, self.track_position, bit);
    }

    /// Gets the amount of ticks to the next flux transition, which is at least one tick away
    /// (flux tracks)
    #[inline]
    pub fn ticks_to_transition(&self) -> Ticks {
        self.flux_ticks_left.max(1) as Ticks
    }

    /// Advances towards the next flux transition. Returns true when it passes under the head.
    #[inline]
    pub fn advance_flux(&mut self, head: usize, ticks: Ticks) -> bool {
        self.flux_ticks_left -= ticks as i16;

        // Not sure how long this should be?
        if self.flux_ticks_left < self.flux_ticks - 20 {
            self.head_bit[head] = false;
        }

        self.flux_ticks_left <= 0
    }

    /// Gets the amount of ticks from the last flux transition to the current one
    #[inline]
    pub fn transition_ticks(&self) -> FluxTicks {
        self.flux_ticks
    }

    /// Marks the current flux transition as read by a head
    #[inline]
    pub fn transition_read(&mut self, head: usize) {
        self.head_bit[head] = true;
    }

    /// Advances to the next flux transition on the track (flux tracks)
    pub fn next_transition(&mut self, head: usize) {
        let TrackLength::Transitions(tlen) = self.floppy.get_track_length(head, self.track) else {
            unreachable!()
        };
        self.track_position = (self.track_position + 1) % tlen;
        self.flux_ticks = self
            .floppy
            .get_track_transition(head, self.track, self.track_position);
        self.flux_ticks_left = self.flux_ticks;
    }
}
