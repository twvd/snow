use anyhow::{bail, Result};

use crate::{Floppy, FloppyImage, FloppyType};

pub struct MacFormatEncoder<'a> {
    data: &'a [u8],
    tags: Option<&'a [u8]>,
    image: FloppyImage,

    enc_track: usize,
    enc_side: usize,
    enc_zeroes: i16,
}

impl<'a> MacFormatEncoder<'a> {
    /// 6-and-2 encoding table for 6 logical bits to one on-disk byte
    #[rustfmt::skip]
    const GCR_ENCTABLE: [u8; 64] = [
        // 0x00 - 0x0F
        0x96, 0x97, 0x9A, 0x9B, 0x9D, 0x9E, 0x9F, 0xA6, 0xA7, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF, 0xB2, 0xB3,
        // 0x10 - 0x1F
        0xB4, 0xB5, 0xB6, 0xB7, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xCB, 0xCD, 0xCE, 0xCF, 0xD3,
        // 0x20 - 0x2F
        0xD6, 0xD7, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF, 0xE5, 0xE6, 0xE7, 0xE9, 0xEA, 0xEB, 0xEC,
        // 0x30 - 0x3F
        0xED, 0xEE, 0xEF, 0xF2, 0xF3, 0xF4, 0xf5, 0xF6, 0xF7, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF,
    ];

    /// Sectors/track per speed group
    const SECTORS_PER_TRACK: [usize; 5] = [12, 11, 10, 9, 8];

    /// Sector interleaving per speed group
    const SECTOR_INTERLEAVE: [&'static [usize]; 5] = [
        &[0, 6, 1, 7, 2, 8, 3, 9, 4, 10, 5, 11],
        &[0, 6, 1, 7, 2, 8, 3, 9, 4, 10, 5],
        &[0, 5, 1, 6, 2, 7, 3, 8, 4, 9],
        &[0, 5, 1, 6, 2, 7, 3, 8, 4],
        &[0, 4, 1, 5, 2, 6, 3, 7],
    ];

    /// Sector tag size, in bytes.
    const SECTOR_TAG_SIZE: usize = 12;

    /// Sector data size, in bytes.
    const SECTOR_DATA_SIZE: usize = 512;

    /// Full sector size (tag + data), in bytes.
    const SECTOR_SIZE: usize = Self::SECTOR_TAG_SIZE + Self::SECTOR_DATA_SIZE;

    /// Auto sync group
    const AUTO_SYNC_GROUP: &'static [u8] = &[0xFF, 0x3F, 0xCF, 0xF3, 0xFC, 0xFF];

    /// Address mark
    const ADDRESS_MARK: &'static [u8] = &[0xD5, 0xAA, 0x96];

    /// Data mark
    const DATA_MARK: &'static [u8] = &[0xD5, 0xAA, 0xAD];

    /// Bit slip sequence
    const BIT_SLIP_SEQ: &'static [u8] = &[0xDE, 0xAA, 0xFF, 0xFF];

    fn push_physical(&mut self, data: &[u8]) {
        for mut byte in data.iter().copied() {
            for _ in 0..8 {
                if byte & (1 << 7) != 0 {
                    self.image
                        .push(self.enc_side, self.enc_track, (self.enc_zeroes + 1) * 16);
                    self.enc_zeroes = 0;
                } else {
                    self.enc_zeroes += 1;
                }
                byte <<= 1;
            }
        }
    }

    fn push_physical_enc(&mut self, data: &[u8]) {
        for b in data {
            self.push_physical(&[Self::GCR_ENCTABLE[*b as usize]]);
        }
    }

    /// Encodes/mangles logical data into pre-GCR encoded sector data and
    /// appends the checksum calculated using the GCR checksum algorithm.
    fn encode_sector_data(data: &[u8]) -> Vec<u8> {
        // This function has been adapted from Greaseweazle, which in turn
        // incorporates code from FluxEngine by David Given, which in turn is
        // extremely inspired from MESS by Nathan Woods and R. Belmont.

        const LOOKUP_LEN: usize = MacFormatEncoder::SECTOR_SIZE / 3;

        let mut b1: [u8; LOOKUP_LEN + 1] = [0; LOOKUP_LEN + 1];
        let mut b2: [u8; LOOKUP_LEN + 1] = [0; LOOKUP_LEN + 1];
        let mut b3: [u8; LOOKUP_LEN + 1] = [0; LOOKUP_LEN + 1];
        let mut c1: u32 = 0;
        let mut c2: u32 = 0;
        let mut c3: u32 = 0;

        let mut din = data.iter().peekable();

        for j in 0.. {
            c1 = (c1 & 0xff) << 1;
            if (c1 & 0x0100) != 0 {
                c1 += 1;
            }

            let val = *din.next().unwrap();
            c3 += val as u32;
            if (c1 & 0x0100) != 0 {
                c3 += 1;
                c1 &= 0xff;
            }
            b1[j] = val ^ (c1 as u8);

            let val = *din.next().unwrap();
            c2 += val as u32;
            if c3 > 0xff {
                c2 += 1;
                c3 &= 0xff;
            }
            b2[j] = val ^ (c3 as u8);

            if din.peek().is_none() {
                // End of input
                break;
            }

            let val = *din.next().unwrap();
            c1 += val as u32;
            if c2 > 0xff {
                c1 += 1;
                c2 &= 0xff;
            }
            b3[j] = val ^ (c2 as u8);
        }
        let c4 = ((c1 & 0xc0) >> 6) | ((c2 & 0xc0) >> 4) | ((c3 & 0xc0) >> 2);
        b3[LOOKUP_LEN] = 0;

        let mut out = vec![];
        for i in 0..=LOOKUP_LEN {
            let w1 = b1[i] & 0x3f;
            let w2 = b2[i] & 0x3f;
            let w3 = b3[i] & 0x3f;
            let w4 = ((b1[i] & 0xc0) >> 2) | ((b2[i] & 0xc0) >> 4) | ((b3[i] & 0xc0) >> 6);

            out.push(w4);
            out.push(w1);
            out.push(w2);

            if i != LOOKUP_LEN {
                out.push(w3);
            }
        }

        assert_eq!(din.count(), 0);

        // Append GCR checksum
        out.push(c4 as u8 & 0x3f);
        out.push(c3 as u8 & 0x3f);
        out.push(c2 as u8 & 0x3f);
        out.push(c1 as u8 & 0x3f);
        out
    }

    fn push_sector_header(&mut self, side: usize, track: usize, tsector: usize) {
        let mut checksum = 0u8;
        // Auto sync groups
        for _ in 0..6 {
            self.push_physical(Self::AUTO_SYNC_GROUP);
        }

        // Address mark
        self.push_physical(Self::ADDRESS_MARK);

        // Track number low
        self.push_physical_enc(&[(track as u8) & 0x3F]);
        checksum ^= (track as u8) & 0x3F;
        // Sector number
        let tsector = tsector & 0x1F;
        self.push_physical_enc(&[tsector as u8]);
        checksum ^= tsector as u8;
        // Head and track number high
        let hthigh = ((side as u8) << 5) | (((track as u8) >> 6) & 1);
        self.push_physical_enc(&[hthigh]);
        checksum ^= hthigh;
        // Format
        let format = match self.image.floppy_type {
            FloppyType::Mac400K => 0x02,
            FloppyType::Mac800K => 0x22,
        };
        self.push_physical_enc(&[format]);
        checksum ^= format;

        // Checksum
        self.push_physical_enc(&[checksum]);

        // Bit slip sequence
        self.push_physical(Self::BIT_SLIP_SEQ);
    }

    fn push_sector_data(&mut self, id: u8, tag: &[u8], data: &[u8]) {
        // Auto sync group
        self.push_physical(Self::AUTO_SYNC_GROUP);

        // Data mark
        self.push_physical(Self::DATA_MARK);

        // Sector identifier
        self.push_physical_enc(&[id & 0x1F]);

        // Encode tag + data
        let mut sectordata = Vec::with_capacity(Self::SECTOR_SIZE);
        sectordata.extend(tag);
        sectordata.extend(data);
        assert_eq!(sectordata.len(), Self::SECTOR_SIZE);
        let encoded = Self::encode_sector_data(&sectordata);
        self.push_physical_enc(&encoded);

        // Bit slip sequence
        self.push_physical(Self::BIT_SLIP_SEQ);
    }

    /// Encodes logical sectors into a GCR, CLV Macintosh format bitstream
    pub fn encode(
        format: FloppyType,
        data: &'a [u8],
        tags: Option<&'a [u8]>,
        name: &str,
    ) -> Result<FloppyImage> {
        let mut encoder = Self::new(format, data, tags, name)?;
        encoder.run()?;
        Ok(encoder.image)
    }

    fn run(&mut self) -> Result<()> {
        let mut sector_offset = 0usize;

        for track in 0..self.image.get_track_count() {
            self.enc_track = track;
            for side in 0..self.image.get_side_count() {
                self.enc_side = side;

                let speedgroup = track / 16;
                for &tsector in Self::SECTOR_INTERLEAVE[speedgroup] {
                    let sector = sector_offset + tsector;
                    let data = &self.data[(sector * Self::SECTOR_DATA_SIZE)
                        ..((sector + 1) * Self::SECTOR_DATA_SIZE)];
                    let tag = if let Some(t) = self.tags.as_ref() {
                        &t[(sector * Self::SECTOR_TAG_SIZE)..((sector + 1) * Self::SECTOR_TAG_SIZE)]
                    } else {
                        &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
                    };
                    self.push_sector_header(side, track, tsector);
                    self.push_sector_data(tsector as u8, tag, data);
                }
                sector_offset += Self::SECTORS_PER_TRACK[speedgroup];

                if self.enc_zeroes > 0 {
                    self.image
                        .stitch(self.enc_side, self.enc_track, self.enc_zeroes * 16);
                    self.enc_zeroes = 0;
                }
            }
        }
        Ok(())
    }

    fn new(
        format: FloppyType,
        data: &'a [u8],
        tags: Option<&'a [u8]>,
        title: &str,
    ) -> Result<Self> {
        if data.len() != format.get_logical_size() {
            bail!(
                "Invalid data length: {} (expected {})",
                data.len(),
                format.get_logical_size()
            );
        }
        if let Some(t) = tags.as_ref() {
            if t.len() != format.get_sector_count() * Self::SECTOR_TAG_SIZE {
                bail!(
                    "Invalid tags length: {} (expected {})",
                    t.len(),
                    format.get_sector_count() * Self::SECTOR_TAG_SIZE
                );
            }
        }

        Ok(Self {
            data,
            tags,
            image: FloppyImage::new_empty(format, title),
            enc_track: 0,
            enc_side: 0,
            enc_zeroes: 0,
        })
    }
}
