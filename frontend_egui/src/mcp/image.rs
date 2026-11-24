//! Pictures for the MCP tools: crop, scale, PNG, base64, and comparing frames

use super::hub::Frame;

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn whole(f: &Frame) -> Self {
        Self {
            x: 0,
            y: 0,
            w: f.width,
            h: f.height,
        }
    }

    /// Clips the rectangle to the frame; None if nothing is left
    pub fn clip(self, f: &Frame) -> Option<Self> {
        let x = self.x.min(f.width);
        let y = self.y.min(f.height);
        let w = self.w.min(f.width - x);
        let h = self.h.min(f.height - y);
        (w > 0 && h > 0).then_some(Self { x, y, w, h })
    }

    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// An RGB picture
pub struct Rgb {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

pub fn crop(f: &Frame, r: Rect) -> Rgb {
    let mut px = Vec::with_capacity(r.w * r.h * 3);
    for y in r.y..r.y + r.h {
        let row = &f.rgba[(y * f.width + r.x) * 4..(y * f.width + r.x + r.w) * 4];
        for p in row.chunks_exact(4) {
            px.extend_from_slice(&p[..3]);
        }
    }
    Rgb { w: r.w, h: r.h, px }
}

/// Scales by a factor: nearest neighbour up, box filter down
pub fn scale(img: Rgb, factor: f64) -> Rgb {
    if (factor - 1.0).abs() < 1e-6 {
        return img;
    }
    let w = ((img.w as f64 * factor).round() as usize).max(1);
    let h = ((img.h as f64 * factor).round() as usize).max(1);
    let mut px = vec![0; w * h * 3];
    if factor > 1.0 {
        for y in 0..h {
            let sy = (y * img.h / h).min(img.h - 1);
            for x in 0..w {
                let sx = (x * img.w / w).min(img.w - 1);
                px[(y * w + x) * 3..][..3].copy_from_slice(&img.px[(sy * img.w + sx) * 3..][..3]);
            }
        }
    } else {
        for y in 0..h {
            let y0 = y * img.h / h;
            let y1 = ((y + 1) * img.h / h).max(y0 + 1).min(img.h);
            for x in 0..w {
                let x0 = x * img.w / w;
                let x1 = ((x + 1) * img.w / w).max(x0 + 1).min(img.w);
                let mut sum = [0u32; 3];
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let src = &img.px[(sy * img.w + sx) * 3..][..3];
                        for (acc, &v) in sum.iter_mut().zip(src) {
                            *acc += u32::from(v);
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as u32;
                for (dst, acc) in px[(y * w + x) * 3..][..3].iter_mut().zip(sum) {
                    *dst = (acc / n) as u8;
                }
            }
        }
    }
    Rgb { w, h, px }
}

/// Draws a small red cross at (x, y)
pub fn mark(img: &mut Rgb, x: isize, y: isize) {
    for d in -6isize..=6 {
        for (px, py) in [(x + d, y), (x, y + d)] {
            if px >= 0 && py >= 0 && (px as usize) < img.w && (py as usize) < img.h {
                img.px[(py as usize * img.w + px as usize) * 3..][..3]
                    .copy_from_slice(&[0xFF, 0x00, 0x00]);
            }
        }
    }
}

pub fn png(img: &Rgb) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.w as u32, img.h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&img.px).expect("png data");
    }
    out
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

/// How many pixels of `area` differ between two frames, not counting the
/// pixels in `ignore`. Frames of another size differ everywhere.
pub fn diff(a: &Frame, b: &Frame, area: Rect, ignore: &[Rect]) -> usize {
    if a.width != b.width || a.height != b.height {
        return usize::MAX;
    }
    if a.seq == b.seq {
        return 0;
    }
    let mut n = 0;
    for y in area.y..area.y + area.h {
        let start = (y * a.width + area.x) * 4;
        let end = start + area.w * 4;
        let (ra, rb) = (&a.rgba[start..end], &b.rgba[start..end]);
        if ra == rb {
            continue;
        }
        for (x, (pa, pb)) in ra.chunks_exact(4).zip(rb.chunks_exact(4)).enumerate() {
            if pa != pb && !ignore.iter().any(|r| r.contains(area.x + x, y)) {
                n += 1;
            }
        }
    }
    n
}
