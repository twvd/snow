//! Fluxfox helpers

use crate::{Floppy, FloppyImage, FloppyType, TrackLength, TrackType};

use anyhow::{Result, anyhow, bail};
use fluxfox::DiskImage;
use fluxfox::prelude::{TrackDataEncoding, TrackDataResolution};
use fluxfox::types::{BitStreamTrackParams, DiskCh, TrackDataRate};

/// Builds a Fluxfox `DiskImage` from a Snow `FloppyImage`.
pub fn floppy_image_to_fluxfox(img: &FloppyImage) -> Result<DiskImage> {
    let (encoding, data_rate) = match img.get_type() {
        FloppyType::Mac400K | FloppyType::Mac800K => {
            (TrackDataEncoding::Gcr, TrackDataRate::Rate250Kbps(1.0))
        }
        FloppyType::Mfm144M => (TrackDataEncoding::Mfm, TrackDataRate::Rate500Kbps(1.0)),
    };

    let mut disk = DiskImage::default();
    disk.set_resolution(TrackDataResolution::BitStream);

    let sides = img.get_side_count();
    let tracks = img.get_track_count();
    let mut added = 0usize;

    for track in 0..tracks {
        for side in 0..sides {
            if img.get_track_type(side, track) != TrackType::Bitstream {
                // TODO flux support
                continue;
            }
            let TrackLength::Bits(bits) = img.get_track_length(side, track) else {
                continue;
            };
            if bits == 0 {
                continue;
            }
            let bytes_needed = bits.div_ceil(8);
            let raw = img.track_bytes(side, track);
            if raw.len() < bytes_needed {
                continue;
            }
            let data = &raw[..bytes_needed];

            let params = BitStreamTrackParams {
                schema: None,
                ch: DiskCh::new(track as u16, side as u8),
                encoding,
                data_rate,
                rpm: None,
                bitcell_ct: Some(bits),
                data,
                weak: None,
                hole: None,
                detect_weak: false,
            };
            disk.add_track_bitstream(&params)
                .map_err(|e| anyhow!("fluxfox add_track_bitstream failed: {e:?}"))?;
            added += 1;
        }
    }

    if added == 0 {
        bail!("Image has no bitstream tracks to visualize");
    }

    Ok(disk)
}
