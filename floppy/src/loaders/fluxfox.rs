//! Loader to load an image through Fluxfox
//! https://github.com/dbalsom/fluxfox

use super::FloppyImageLoader;
use crate::{FloppyImage, FloppyType, OriginalTrackType};

use anyhow::{bail, Context, Result};
use binrw::io::Cursor;
use fluxfox::prelude::TrackDataEncoding;
use fluxfox::types::DiskCh;
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
        let image = DiskImage::load(&mut cursor, None, None, None)?;

        // We use track 0's type to determine the disk image type, as Snow still needs this information
        // (FloppyType) internally. This does possibly prevent images with tracks of different formats
        // from working, but I haven't encountered these yet (for Mac/PC).
        let t0info = image
            .track(DiskCh::new(0, 0))
            .as_ref()
            .context("Image has no track 0?")?
            .info();
        let floppytype = match (t0info.encoding, image.heads()) {
            (TrackDataEncoding::Mfm, _) => FloppyType::Mfm144M,
            (TrackDataEncoding::Gcr, 1) => FloppyType::Mac400K,
            (TrackDataEncoding::Gcr, 2) => FloppyType::Mac800K,
            _ => bail!(
                "Unrecognized image encoding: {} ({} sides)",
                t0info.encoding,
                image.heads()
            ),
        };

        let mut img = FloppyImage::new_empty(floppytype, filename.unwrap_or_default());

        // Fill tracks
        for tch in image.track_ch_iter() {
            if tch.h() > 1 || tch.c() >= 80 {
                log::warn!("Ignoring out of range tch: {:?}", tch);
                continue;
            }
            let side = tch.h() as usize;
            let track = tch.c() as usize;
            let trackdata = image.track(tch).as_ref().unwrap().read_raw(None)?;

            img.origtracktype[side][track] = OriginalTrackType::Bitstream;
            img.set_actual_track_length(side, track, trackdata.read_len_bits);

            img.trackdata[side][track][0..trackdata.read_buf.len()]
                .copy_from_slice(&trackdata.read_buf);
        }

        Ok(img)
    }
}
