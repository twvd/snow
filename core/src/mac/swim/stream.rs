//! The disk under the heads of a floppy drive

use serde::{Deserialize, Serialize};
use snow_floppy::flux::FluxTicks;
use snow_floppy::{Floppy, FloppyImage, FloppyType, TrackLength, TrackType};

use super::WeakBitRng;
use crate::mac::maclc::pds_iie::disk::{CELLS, DskImage};
use crate::tickable::Ticks;

/// The disk of a stream
// A Mac disk image is much the larger of the two. It stays in line: a Box would add a pointer
// to follow for each bit the Mac's controller reads.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize)]
enum Disk {
    /// The disk in a Mac drive. A drive with none has a blank disk here.
    Mac(FloppyImage),
    /// The disk in a 5.25" drive of an Apple IIe Card, if it has one. It isn't in save
    /// states: the frontend inserts it again.
    Apple525 {
        #[serde(skip)]
        image: Option<DskImage>,
    },
}

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
    disk: Disk,

    /// The track under the heads: a track of a Mac disk, a quarter-track of a 5.25" disk
    pub(crate) track: usize,

    /// The position on the track: a bit of a bitstream track, a transition of a flux track
    pub(crate) track_position: usize,

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
    /// The stream of a Mac drive, with a blank disk
    pub(super) fn new(double_sided: bool) -> Self {
        Self::with_disk(
            Disk::Mac(FloppyImage::new(FloppyType::Mac400K, "")),
            4,
            double_sided,
        )
    }

    /// The stream of a 5.25" drive, with no disk. It always says it spins: the Disk II
    /// controller of the Apple IIe Card runs the motor itself, and doesn't read `spinning`.
    pub(crate) fn new_525() -> Self {
        let mut stream = Self::with_disk(Disk::Apple525 { image: None }, 0, false);
        stream.spinning = true;
        stream
    }

    fn with_disk(disk: Disk, track: usize, double_sided: bool) -> Self {
        Self {
            disk,
            track,
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

    /// Gets the disk of a Mac drive
    pub(super) fn floppy(&self) -> &FloppyImage {
        match &self.disk {
            Disk::Mac(floppy) => floppy,
            Disk::Apple525 { .. } => unreachable!("Not the stream of a Mac drive"),
        }
    }

    /// Gets the disk of a Mac drive, to change it
    pub(super) fn floppy_mut(&mut self) -> &mut FloppyImage {
        match &mut self.disk {
            Disk::Mac(floppy) => floppy,
            Disk::Apple525 { .. } => unreachable!("Not the stream of a Mac drive"),
        }
    }

    /// Gets the disk of a 5.25" drive, if it has one
    pub(crate) fn disk_525(&self) -> Option<&DskImage> {
        match &self.disk {
            Disk::Apple525 { image } => image.as_ref(),
            Disk::Mac(_) => None,
        }
    }

    /// Puts a disk in a 5.25" drive, or takes it out with `None`
    pub(crate) fn set_disk_525(&mut self, disk: Option<DskImage>) {
        self.disk = Disk::Apple525 { image: disk };
    }

    /// Gets the amount of cells in one turn of the track under the head of a 5.25" drive
    pub(crate) fn track_len_525(&self) -> usize {
        self.disk_525()
            .map_or(CELLS, |disk| disk.track_len(self.track))
    }

    /// Gets the time a bit of a 5.25" disk is under the head, in units of 125 ns
    pub fn bit_timing_525(&self) -> Option<u8> {
        self.disk_525().map(DskImage::bit_timing)
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
        if !self.double_sided || self.floppy().get_side_count() == 1 || !sel {
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
        match &self.disk {
            Disk::Mac(floppy) => floppy.get_track_type(head, self.track),
            Disk::Apple525 { .. } => TrackType::Bitstream,
        }
    }

    /// Gets a bit of noise, as a head reads when there is nothing to read
    #[inline]
    pub fn weak_bit(&self) -> bool {
        self.weak_bit_rng.random_bool()
    }

    /// Gets the physical disk bit currently under a head
    pub(crate) fn head_bit(&self, head: usize) -> bool {
        let floppy = match &self.disk {
            Disk::Mac(floppy) => floppy,
            // No bit with no disk, or no track there
            Disk::Apple525 { image } => {
                return image
                    .as_ref()
                    .is_some_and(|disk| disk.bit(self.track, self.track_position));
            }
        };
        match floppy.get_track_type(head, self.track) {
            TrackType::Bitstream => {
                let TrackLength::Bits(tracklen) = floppy.get_track_length(head, self.track) else {
                    unreachable!()
                };
                // Extra modulus here because RDDATAx can be read while the controller is currently
                // has the other side selected or we just changed tracks.
                floppy.get_track_bit(head, self.track, self.track_position % tracklen)
            }
            TrackType::Flux => self.head_bit[head],
        }
    }

    /// Advances to the next bit on the track (bitstream tracks)
    #[inline]
    pub fn next_bit(&mut self, head: usize) -> bool {
        let floppy = match &self.disk {
            Disk::Mac(floppy) => floppy,
            // A 5.25" drive gives the bit the head leaves
            Disk::Apple525 { .. } => {
                let bit = self.head_bit(head);
                self.track_position += 1;
                if self.track_position >= self.track_len_525() {
                    self.track_position = 0;
                }
                return bit;
            }
        };
        let TrackLength::Bits(tracklen) = floppy.get_track_length(head, self.track) else {
            unreachable!()
        };
        self.track_position = (self.track_position + 1) % tracklen;

        // Garbage if drive is not yet ready
        if self.not_ready {
            return self.weak_bit();
        }
        self.head_bit(head)
    }

    /// Writes a bit to the current track position. A 5.25" drive doesn't write.
    #[inline]
    pub fn write_bit(&mut self, head: usize, bit: bool) {
        if let Disk::Mac(floppy) = &mut self.disk {
            floppy.set_track_bit(head, self.track, self.track_position, bit);
        }
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
        let floppy = self.floppy();
        let TrackLength::Transitions(tlen) = floppy.get_track_length(head, self.track) else {
            unreachable!()
        };
        let position = (self.track_position + 1) % tlen;
        self.flux_ticks = floppy.get_track_transition(head, self.track, position);
        self.track_position = position;
        self.flux_ticks_left = self.flux_ticks;
    }
}
