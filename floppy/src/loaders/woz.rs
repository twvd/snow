//! Applesauce WOZ format: Apple II 5.25" and 3.5" disks as bit streams
//! WOZ 2: https://applesaucefdc.com/woz/reference2/
//! WOZ 1: https://applesaucefdc.com/woz/reference/
//! The Macintosh drives take the 3.5" disks, which the Apple IIe Card can
//! use. The card's 5.25" drives take the image apart with [`Woz::parse`].

use std::collections::HashMap;

use super::FloppyImageLoader;
use crate::flux::FluxTicks;
use crate::{FLOPPY_MAX_TRACKS, FloppyImage, FloppyType, OriginalTrackType};

use anyhow::{Context, Result, bail};

/// Entries in the track maps: 5.25" quarter-tracks, or 3.5" tracks and sides
const MAP_ENTRIES: usize = 160;
/// Size of a WOZ 1 track in the TRKS chunk
const WOZ1_TRACK_SIZE: usize = 6656;
/// Room for the bits in a WOZ 1 track
const WOZ1_BITS_SIZE: usize = 6646;

pub struct Woz {}

/// One track of a WOZ image
pub struct WozTrack<'a> {
    /// The bits, most significant first, or the bytes of a flux track
    pub data: &'a [u8],
    /// The number of bits, or of bytes for a flux track
    pub count: usize,
}

/// The parts of a WOZ file
pub struct WozData<'a> {
    /// Disk type (`Woz::DISK_525`, `Woz::DISK_35`)
    pub disk_type: u8,
    /// The disk is write-protected
    pub write_protected: bool,
    /// Sides of the disk
    pub sides: usize,
    /// The rate the drive delivers the bits at, in 125 ns units
    pub bit_timing: u8,
    /// The entry in `tracks` under each 5.25" quarter-track, or under each
    /// (track << 1) + side of a 3.5" disk. `Woz::NO_TRACK` where there is
    /// none.
    pub tmap: [u8; MAP_ENTRIES],
    /// The flux tracks (WOZ 2.1), in the same layout as `tmap`
    pub flux_map: Option<[u8; MAP_ENTRIES]>,
    /// The tracks of the TRKS chunk
    pub tracks: Vec<WozTrack<'a>>,
    /// The META chunk
    pub metadata: HashMap<String, String>,
}

impl<'a> WozData<'a> {
    /// The bit stream track at map entry `entry`
    pub fn track(&self, entry: usize) -> Option<&WozTrack<'a>> {
        self.trks_entry(self.tmap[entry])
    }

    /// The flux track at map entry `entry`
    pub fn flux_track(&self, entry: usize) -> Option<&WozTrack<'a>> {
        self.trks_entry(self.flux_map?[entry])
    }

    /// The track at `index` in `tracks`, none for `Woz::NO_TRACK`
    fn trks_entry(&self, index: u8) -> Option<&WozTrack<'a>> {
        if index == Woz::NO_TRACK {
            return None;
        }
        self.tracks.get(usize::from(index))
    }
}

impl Woz {
    /// Disk type: 5.25"
    pub const DISK_525: u8 = 1;
    /// Disk type: 3.5"
    pub const DISK_35: u8 = 2;
    /// Map entry with no track
    pub const NO_TRACK: u8 = 0xFF;

    const CHECKSUM: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

    pub fn detect(data: &[u8]) -> bool {
        data.len() >= 12 && (data[0..8] == *b"WOZ1\xFF\n\r\n" || data[0..8] == *b"WOZ2\xFF\n\r\n")
    }

    pub fn parse(data: &[u8]) -> Result<WozData<'_>> {
        if !Self::detect(data) {
            bail!("Not a WOZ image");
        }
        let woz1 = data[3] == b'1';

        // A CRC of 0 was not computed
        let crc = le32(data, 8);
        if crc != 0 && crc != Self::CHECKSUM.checksum(&data[12..]) {
            bail!("WOZ checksum verification failed");
        }

        let mut info = None;
        let mut tmap = None;
        let mut flux_map = None;
        let mut trks = None;
        let mut metadata = HashMap::new();
        let mut pos = 12;
        while pos + 8 <= data.len() {
            let id = &data[pos..pos + 4];
            let size = le32(data, pos + 4) as usize;
            let body = data
                .get(pos + 8..pos + 8 + size)
                .context("WOZ chunk out of bounds")?;
            match id {
                b"INFO" => info = Some(body),
                b"TMAP" => tmap = Some(map(body)?),
                b"FLUX" => flux_map = Some(map(body)?),
                b"TRKS" => trks = Some(body),
                b"META" => {
                    for line in String::from_utf8_lossy(body).lines() {
                        if let Some((k, v)) = line.split_once('\t') {
                            metadata.insert(k.to_string(), v.to_string());
                        }
                    }
                }
                _ => (),
            }
            pos += 8 + size;
        }

        let info = info.context("No INFO chunk in WOZ image")?;
        let mut tmap = tmap.context("No TMAP chunk in WOZ image")?;
        let trks = trks.context("No TRKS chunk in WOZ image")?;
        if info.len() < 37 {
            bail!("WOZ INFO chunk too short");
        }
        let version = info[0];
        let disk_type = info[1];
        if disk_type != Self::DISK_525 && disk_type != Self::DISK_35 {
            bail!("Unsupported WOZ disk type {}", disk_type);
        }

        let tracks = if woz1 {
            trks.chunks_exact(WOZ1_TRACK_SIZE)
                .map(|t| WozTrack {
                    data: &t[..WOZ1_BITS_SIZE],
                    count: usize::from(le16(t, WOZ1_BITS_SIZE + 2)),
                })
                .collect()
        } else {
            let mut tracks = vec![];
            for entry in trks.chunks_exact(8).take(MAP_ENTRIES) {
                let start = usize::from(le16(entry, 0)) * 512;
                let len = usize::from(le16(entry, 2)) * 512;
                tracks.push(WozTrack {
                    data: data
                        .get(start..start + len)
                        .context("WOZ track out of bounds")?,
                    count: le32(entry, 4) as usize,
                });
            }
            tracks
        };

        // WOZ 1 lists 3.5" side 0, then side 1
        if woz1 && disk_type == Self::DISK_35 {
            let side_first = tmap;
            for (entry, index) in tmap.iter_mut().enumerate() {
                *index = side_first[(entry & 1) * 80 + entry / 2];
            }
        }

        let sides = if disk_type == Self::DISK_525 {
            1
        } else if version >= 2 && info.len() > 37 {
            usize::from(info[37])
        } else if tmap.iter().skip(1).step_by(2).any(|&i| i != Self::NO_TRACK) {
            2
        } else {
            1
        };
        if !(1..=2).contains(&sides) {
            bail!("Unsupported WOZ side count {}", sides);
        }
        let standard_timing = if disk_type == Self::DISK_525 { 32 } else { 16 };
        let bit_timing = match info.get(39) {
            Some(&timing) if version >= 2 && timing != 0 => timing,
            _ => standard_timing,
        };

        let woz = WozData {
            disk_type,
            write_protected: info[2] == 1,
            sides,
            bit_timing,
            tmap,
            flux_map,
            tracks,
            metadata,
        };
        for entry in 0..MAP_ENTRIES {
            let bits = woz.tmap[entry] != Self::NO_TRACK;
            if bits && woz.track(entry).is_none_or(|t| t.count == 0) {
                bail!("WOZ map entry {} names a missing track", entry);
            }
            if woz
                .track(entry)
                .is_some_and(|t| t.count.div_ceil(8) > t.data.len())
            {
                bail!("WOZ track at map entry {} too short", entry);
            }
            let flux = woz.flux_map.is_some_and(|m| m[entry] != Self::NO_TRACK);
            if flux && woz.flux_track(entry).is_none_or(|t| t.count > t.data.len()) {
                bail!("WOZ flux map entry {} names a bad track", entry);
            }
        }
        Ok(woz)
    }
}

fn map(body: &[u8]) -> Result<[u8; MAP_ENTRIES]> {
    body.get(..MAP_ENTRIES)
        .context("WOZ map chunk too short")?
        .try_into()
        .context("WOZ map chunk too short")
}

fn le16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap())
}

fn le32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

impl FloppyImageLoader for Woz {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let woz = Self::parse(data)?;
        if woz.disk_type != Self::DISK_35 {
            bail!("A 5.25\" WOZ image only goes in the Apple IIe Card's drives");
        }
        let floppy_type = if woz.sides == 2 {
            FloppyType::Mac800K
        } else {
            FloppyType::Mac400K
        };
        let title = woz
            .metadata
            .get("title")
            .map(String::as_str)
            .unwrap_or_else(|| filename.unwrap_or_default());

        // The tracks not in the image keep the random bits of a new image
        let mut img = FloppyImage::new_empty(floppy_type, title);
        for (k, v) in &woz.metadata {
            img.set_metadata(k, v);
        }
        for track in 0..FLOPPY_MAX_TRACKS {
            for side in 0..woz.sides {
                let entry = (track << 1) | side;
                if let Some(trk) = woz.flux_track(entry) {
                    img.origtracktype[side][track] = OriginalTrackType::Flux;
                    // Each byte is a time in 125 ns ticks, 255 adds the next
                    let mut ticks: FluxTicks = 0;
                    for &b in &trk.data[..trk.count] {
                        ticks = ticks.saturating_add(FluxTicks::from(b));
                        if b == 255 {
                            continue;
                        }
                        img.push_flux(side, track, ticks);
                        ticks = 0;
                    }
                    if ticks > 0 {
                        img.push_flux(side, track, ticks);
                    }
                } else if let Some(trk) = woz.track(entry) {
                    img.set_actual_track_length(side, track, trk.count);
                    for i in 0..trk.count {
                        let bit = trk.data[i / 8] & (0x80 >> (i % 8)) != 0;
                        img.push_track_bit(side, track, i, bit);
                    }
                    img.origtracktype[side][track] = OriginalTrackType::Bitstream;
                }
            }
        }
        if woz.write_protected {
            img.set_force_wp();
        }
        Ok(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Floppy, TrackLength};

    /// A WOZ 2 image with the bit stream tracks `tracks` (bytes, all bits
    /// used) at the map entries `entries`
    fn woz2(disk_type: u8, sides: u8, entries: &[(usize, u8)], tracks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = b"WOZ2\xFF\n\r\n\0\0\0\0".to_vec();

        let mut info = vec![0; 60];
        info[0] = 2;
        info[1] = disk_type;
        info[37] = sides;
        info[39] = if disk_type == Woz::DISK_525 { 32 } else { 16 };
        v.extend(b"INFO\x3C\0\0\0");
        v.extend(info);

        let mut tmap = [Woz::NO_TRACK; MAP_ENTRIES];
        for &(entry, index) in entries {
            tmap[entry] = index;
        }
        v.extend(b"TMAP\xA0\0\0\0");
        v.extend(tmap);

        let mut trks = vec![0; 1280];
        let mut bits = vec![];
        for (i, track) in tracks.iter().enumerate() {
            let blocks = track.len().div_ceil(512);
            let start = 3 + bits.len() / 512;
            trks[i * 8..i * 8 + 2].copy_from_slice(&(start as u16).to_le_bytes());
            trks[i * 8 + 2..i * 8 + 4].copy_from_slice(&(blocks as u16).to_le_bytes());
            trks[i * 8 + 4..i * 8 + 8].copy_from_slice(&(track.len() as u32 * 8).to_le_bytes());
            bits.extend(track);
            bits.resize(bits.len().next_multiple_of(512), 0);
        }
        v.extend(b"TRKS");
        v.extend(((1280 + bits.len()) as u32).to_le_bytes());
        v.extend(trks);
        assert_eq!(v.len(), 1536);
        v.extend(bits);

        let crc = Woz::CHECKSUM.checksum(&v[12..]);
        v[8..12].copy_from_slice(&crc.to_le_bytes());
        v
    }

    /// A WOZ 1 image with the tracks `tracks` at the map entries `entries`
    fn woz1(disk_type: u8, entries: &[(usize, u8)], tracks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = b"WOZ1\xFF\n\r\n\0\0\0\0".to_vec();
        let mut info = vec![0; 60];
        info[0] = 1;
        info[1] = disk_type;
        info[2] = 1;
        v.extend(b"INFO\x3C\0\0\0");
        v.extend(info);
        let mut tmap = [Woz::NO_TRACK; MAP_ENTRIES];
        for &(entry, index) in entries {
            tmap[entry] = index;
        }
        v.extend(b"TMAP\xA0\0\0\0");
        v.extend(tmap);
        v.extend(b"TRKS");
        v.extend(((tracks.len() * WOZ1_TRACK_SIZE) as u32).to_le_bytes());
        for track in tracks {
            let mut t = track.clone();
            t.resize(WOZ1_BITS_SIZE, 0);
            t.extend((track.len() as u16).to_le_bytes());
            t.extend((track.len() as u16 * 8).to_le_bytes());
            t.resize(WOZ1_TRACK_SIZE, 0);
            v.extend(t);
        }
        v
    }

    #[test]
    fn woz2_525() {
        let data = woz2(
            Woz::DISK_525,
            1,
            &[(0, 0), (1, 0), (4, 1)],
            &[vec![0xD5, 0xAA], vec![0x96; 700]],
        );
        let woz = Woz::parse(&data).unwrap();
        assert_eq!(woz.disk_type, Woz::DISK_525);
        assert_eq!(woz.bit_timing, 32);
        assert!(!woz.write_protected);
        let t0 = woz.track(1).unwrap();
        assert_eq!(t0.count, 16);
        assert_eq!(&t0.data[..2], &[0xD5, 0xAA]);
        assert!(woz.track(2).is_none());
        assert_eq!(woz.track(4).unwrap().count, 5600);
    }

    #[test]
    fn checksum() {
        let mut data = woz2(Woz::DISK_525, 1, &[(0, 0)], &[vec![0xFF]]);
        data[20 + 5] ^= 1;
        assert!(Woz::parse(&data).is_err());
        // A CRC of 0 is not checked
        data[8..12].fill(0);
        assert!(Woz::parse(&data).is_ok());
    }

    #[test]
    fn missing_track() {
        let data = woz2(Woz::DISK_525, 1, &[(0, 0), (4, 1)], &[vec![0xFF]]);
        assert!(Woz::parse(&data).is_err());
    }

    #[test]
    fn woz1_35_sides() {
        let data = woz1(
            Woz::DISK_35,
            &[(0, 0), (80, 1)],
            &[vec![0x55], vec![0xAA; 3]],
        );
        let woz = Woz::parse(&data).unwrap();
        assert_eq!(woz.sides, 2);
        assert_eq!(woz.bit_timing, 16);
        assert!(woz.write_protected);
        assert_eq!(woz.track(0).unwrap().data[0], 0x55);
        assert_eq!(woz.track(1).unwrap().count, 24);
        assert!(woz.track(2).is_none());
    }

    #[test]
    fn load_35() {
        let data = woz2(
            Woz::DISK_35,
            2,
            &[(0, 0), (3, 1)],
            &[vec![0xA5; 2], vec![0xC3; 4]],
        );
        let img = Woz::load(&data, Some("disk.woz")).unwrap();
        assert_eq!(img.get_type(), FloppyType::Mac800K);
        assert_eq!(img.get_track_length(0, 0), TrackLength::Bits(16));
        assert_eq!(img.get_track_length(1, 1), TrackLength::Bits(32));
        let bits: Vec<bool> = (0..8).map(|i| img.get_track_bit(1, 1, i)).collect();
        assert_eq!(bits, [true, true, false, false, false, false, true, true]);
        assert!(!img.get_write_protect());
    }

    #[test]
    fn load_35_single_sided() {
        let data = woz2(Woz::DISK_35, 1, &[(0, 0)], &[vec![0xA5; 2]]);
        assert_eq!(
            Woz::load(&data, None).unwrap().get_type(),
            FloppyType::Mac400K
        );
    }

    #[test]
    fn load_525_rejected() {
        let data = woz2(Woz::DISK_525, 1, &[(0, 0)], &[vec![0xFF]]);
        assert!(Woz::load(&data, None).is_err());
    }
}
