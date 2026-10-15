//! CD-XA ADPCM decoder
//!
//! The data comes in blocks of 28 samples, each preceded by a parameter
//! byte: the filter (0-3) in bits 4-7 and the range in bits 0-3. Samples
//! are 8, 4 or 2 bits, signed, packed from the lowest bits of a byte up. A
//! sample is shifted right by the range and added to a prediction from the
//! two previous ones, (K0 * s0 + K1 * s1) / 64, with the coefficients of
//! the filter.

use serde::{Deserialize, Serialize};

const BLOCK_SAMPLES: u32 = 28;

#[derive(Default, Serialize, Deserialize)]
pub struct CdxaDecoder {
    /// Last two samples decoded
    s0: i16,
    s1: i16,

    /// Parameter byte of the block
    param: u8,

    /// Sample number in the block
    pos: u32,

    /// Byte the samples are unpacked from
    byte: u8,
}

impl CdxaDecoder {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Bytes the next sample of `bits` bits takes from the input
    pub fn bytes_needed(&self, bits: u32) -> usize {
        usize::from(self.pos == 0) + usize::from((self.pos * bits).is_multiple_of(8))
    }

    /// Decodes a sample of `bits` bits, taking the bytes it needs with
    /// `next`. The coefficients are K1, K0 pairs, one for each filter.
    pub fn decode(&mut self, bits: u32, coefs: &[u8; 8], mut next: impl FnMut() -> u8) -> i16 {
        if self.pos == 0 {
            self.param = next();
        }
        let offset = self.pos * bits % 8;
        if offset == 0 {
            self.byte = next();
        }
        let raw = i16::from(((self.byte >> offset) << (8 - bits)) as i8) << 8;

        let filter = usize::from(self.param >> 4 & 3);
        let range = (self.param & 0xF).min(12);
        let k1 = i32::from(coefs[filter * 2] as i8);
        let k0 = i32::from(coefs[filter * 2 + 1] as i8);
        let prediction = (k0 * i32::from(self.s0) + k1 * i32::from(self.s1) + 32) >> 6;
        let sample = (i32::from(raw >> range) + prediction).clamp(-0x8000, 0x7FFF) as i16;

        self.s1 = self.s0;
        self.s0 = sample;
        self.pos = (self.pos + 1) % BLOCK_SAMPLES;
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The coefficients of the CD-XA standard
    const COEFS: [u8; 8] = [0x00, 0x00, 0x00, 0x3C, 0xCC, 0x73, 0xC9, 0x62];

    fn decode(bits: u32, data: &[u8], samples: usize) -> Vec<i16> {
        let mut decoder = CdxaDecoder::default();
        let mut input = data.iter().copied();
        (0..samples)
            .map(|_| decoder.decode(bits, &COEFS, || input.next().unwrap()))
            .collect()
    }

    #[test]
    fn unpack() {
        // Filter 0, range 0: the samples as they are, lowest bits first
        assert_eq!(decode(8, &[0x00, 0x12, 0x80], 2), [0x1200, -0x8000]);
        assert_eq!(decode(4, &[0x00, 0x8F], 2), [-0x1000, -0x8000]);
        assert_eq!(
            decode(2, &[0x00, 0b00_10_11_01], 4),
            [0x4000, -0x4000, -0x8000, 0]
        );

        // Range 12 at most
        assert_eq!(decode(4, &[0x0F, 0x07], 1), [7]);
        assert_eq!(decode(4, &[0x04, 0x07], 1), [0x700]);
    }

    #[test]
    fn prediction() {
        // Filter 1: s0 * 60 / 64
        assert_eq!(
            decode(8, &[0x10, 0x40, 0x00, 0x00], 3),
            [0x4000, 0x3C00, 0x3840]
        );
        // Filter 2: s0 * 115 / 64 - s1 * 52 / 64, clipped
        assert_eq!(
            decode(8, &[0x20, 0x7F, 0x7F, 0x7F], 3),
            [0x7F00, 0x7FFF, 0x7FFF]
        );
    }

    #[test]
    fn blocks() {
        // A parameter byte for every 28 samples, 14 bytes of 4-bit samples
        let mut data = vec![0x00, 0x11];
        data.extend([0; 13]);
        data.extend([0x01, 0x22]);
        let samples = decode(4, &data, 30);
        assert_eq!(samples[0], 0x1000);
        assert_eq!(samples[28], 0x1000);
        assert_eq!(samples[29], 0x1000);

        let mut decoder = CdxaDecoder::default();
        assert_eq!(decoder.bytes_needed(4), 2);
        decoder.decode(4, &COEFS, || 0);
        assert_eq!(decoder.bytes_needed(4), 0);
        decoder.decode(4, &COEFS, || 0);
        assert_eq!(decoder.bytes_needed(4), 1);
    }
}
