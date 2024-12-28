//! Loader to load an image through Fluxfox
//! https://github.com/dbalsom/fluxfox

use super::FloppyImageLoader;
use crate::{FloppyImage, FloppyType, OriginalTrackType};

use anyhow::Result;
use binrw::io::Cursor;
use fluxfox::DiskImage;

/// Fluxfox loader
pub struct Fluxfox {}

impl Fluxfox {
    pub fn detect(data: &[u8]) -> bool {
        let mut cursor = Cursor::new(data);
        DiskImage::detect_format(&mut cursor, None).is_ok()
    }
}

impl FloppyImageLoader for Fluxfox {
    fn load(data: &[u8], filename: Option<&str>) -> Result<FloppyImage> {
        let mut cursor = Cursor::new(data);
        let mut image = DiskImage::load(&mut cursor, None, None, None)?;

        let mut img = FloppyImage::new_empty(FloppyType::Mfm144M, filename.unwrap_or_default());

        // Fill tracks
        // Below needs collect() because read_track_raw() borrows the DiskImage mutably
        #[allow(clippy::needless_collect)]
        for tch in image.track_ch_iter().collect::<Vec<_>>() {
            if tch.h() > 1 || tch.c() >= 80 {
                log::warn!("Ignoring out of range tch: {:?}", tch);
                continue;
            }
            let side = tch.h() as usize;
            let track = tch.c() as usize;
            let trackdata = image.read_track_raw(tch, None)?;

            img.origtracktype[side][track] = OriginalTrackType::Bitstream;
            img.set_actual_track_length(side, track, trackdata.read_len_bits);

            img.trackdata[side][track][0..trackdata.read_buf.len()]
                .copy_from_slice(&trackdata.read_buf);
        }

        Ok(img)
    }
}
