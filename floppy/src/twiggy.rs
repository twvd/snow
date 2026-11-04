//! The geometry of a Twiggy disk
//!
//! The zones are those of the Lisa 1's floppy controller ROM ($40, 341-0138-f, at $114E
//! and $1129) and of the Twiggy Mac's disk driver (Twiggy.ROM, at $18E6).
//!
//! Images hold side 0, then side 1. The Twiggy Mac's driver numbers its blocks from side 1
//! (blocks 0-850 there, Twiggy.ROM $401920): the Mac disks in circulation have their boot
//! blocks in the second half of the image.

pub const TRACKS: usize = 46;
pub const SECTORS_PER_SIDE: usize = 851;
pub const SECTORS: usize = 2 * SECTORS_PER_SIDE;

/// Tracks in each zone, and their sectors
const ZONES: [(usize, usize); 8] = [
    (4, 22),
    (7, 21),
    (6, 20),
    (6, 19),
    (6, 18),
    (6, 17),
    (7, 16),
    (4, 15),
];

/// Bits a sector takes on a track, gaps included
///
/// The drive turns faster on the inner tracks to keep the bit rate. 22 sectors make a
/// track that, at about 2 us a bit, turns in the time the Twiggy Mac's driver checks for
/// (Twiggy.ROM $402488: $35732 counts of the VIA timer).
pub const BITS_PER_SECTOR: usize = 6220;

/// Sectors on a track. Past the last track, those of the last zone.
pub fn sectors_on_track(track: usize) -> usize {
    let mut first = 0;
    for (tracks, sectors) in ZONES {
        first += tracks;
        if track < first {
            return sectors;
        }
    }
    ZONES[ZONES.len() - 1].1
}

pub fn track_bits(track: usize) -> usize {
    sectors_on_track(track) * BITS_PER_SECTOR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones() {
        assert_eq!(
            (0..TRACKS).map(sectors_on_track).sum::<usize>(),
            SECTORS_PER_SIDE
        );
        assert_eq!(sectors_on_track(3), 22);
        assert_eq!(sectors_on_track(4), 21);
        assert_eq!(sectors_on_track(45), 15);
    }
}
