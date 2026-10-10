//! The client display recombine on the CPU: main (plus an optional aux)
//! NV12 pair to BGRA, with the YUV->RGB conversion folded in.
//!
//! This is the reference the Metal shader in `shaders/recombine.metal`
//! (and `gliff-vk`'s `recombine.comp` on Linux) must reproduce; it is
//! checked against the `gliff-proto` colour oracle in its tests. It is
//! not a shipping path.
//!
//! Layout, from `gliff-proto::chroma`: the main stream carries the full
//! luma and the even-position chroma; the aux stream carries the rest —
//! U on odd rows in the top half of its luma plane, V on odd rows in the
//! bottom half, and the odd-column chroma of even rows in its UV plane.
//! Without an aux frame the main chroma is duplicated over each 2x2
//! block (the Single420 path). Each stream pixel is written as a
//! `zoom` x `zoom` block, so the display shows the frame at an integer
//! scale with no resampling.

use gliff_proto::chroma::Nv12;

/// Recombine into BGRA8 of `zoom * width` by `zoom * height` pixels.
pub fn to_bgra(main: &Nv12, aux: Option<&Nv12>, zoom: u32) -> Vec<u8> {
    let (w, h) = (main.width, main.height);
    assert!(w % 2 == 0 && h % 2 == 0, "dimensions must be even");
    let zoom = zoom.max(1) as usize;
    let ow = w * zoom;
    let mut out = vec![0u8; ow * h * zoom * 4];
    let half = h / 2;
    for by in 0..half {
        for bx in 0..w / 2 {
            let (x, y) = (2 * bx, 2 * by);
            let m = by * w + 2 * bx;
            let c00 = (main.uv[m] as f32, main.uv[m + 1] as f32);
            let (c10, c01, c11) = match aux {
                Some(aux) => (
                    (aux.uv[m] as f32, aux.uv[m + 1] as f32),
                    (aux.y[by * w + x] as f32, aux.y[(half + by) * w + x] as f32),
                    (
                        aux.y[by * w + x + 1] as f32,
                        aux.y[(half + by) * w + x + 1] as f32,
                    ),
                ),
                None => (c00, c00, c00),
            };
            put(&mut out, ow, zoom, x, y, px(main.y[y * w + x], c00));
            put(&mut out, ow, zoom, x + 1, y, px(main.y[y * w + x + 1], c10));
            put(
                &mut out,
                ow,
                zoom,
                x,
                y + 1,
                px(main.y[(y + 1) * w + x], c01),
            );
            put(
                &mut out,
                ow,
                zoom,
                x + 1,
                y + 1,
                px(main.y[(y + 1) * w + x + 1], c11),
            );
        }
    }
    out
}

/// Write one pixel as a `zoom` x `zoom` block at `(x * zoom, y * zoom)`.
fn put(out: &mut [u8], ow: usize, zoom: usize, x: usize, y: usize, p: [u8; 4]) {
    for j in 0..zoom {
        let row = (y * zoom + j) * ow;
        for i in 0..zoom {
            let at = (row + x * zoom + i) * 4;
            out[at..at + 4].copy_from_slice(&p);
        }
    }
}

/// BT.709 limited range, the exact expression of `to_rgb` in
/// `gliff-vk/shaders/common.glsl` (and `gliff-proto::color`), so the CPU
/// and GPU paths agree sample for sample.
fn px(y8: u8, c: (f32, f32)) -> [u8; 4] {
    let y = y8 as f32 - 16.0;
    let cb = c.0 - 128.0;
    let cr = c.1 - 128.0;
    let r = 1.1644 * y + 1.7927 * cr;
    let g = 1.1644 * y - 0.2132 * cb - 0.5329 * cr;
    let b = 1.1644 * y + 2.1124 * cb;
    [round_u8(b), round_u8(g), round_u8(r), 255]
}

fn round_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use gliff_proto::chroma::{self, Yuv444};
    use gliff_proto::color;

    fn lcg_u8(seed: &mut u64) -> u8 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*seed >> 33) as u8
    }

    fn random_yuv444(w: usize, h: usize, mut seed: u64) -> Yuv444 {
        let mut src = Yuv444::new(w, h);
        for plane in [&mut src.y, &mut src.u, &mut src.v] {
            for b in plane.iter_mut() {
                *b = lcg_u8(&mut seed);
            }
        }
        src
    }

    fn max_delta(a: &[u8], b: &[u8]) -> i32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (*x as i32 - *y as i32).abs())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn dual_matches_the_proto_oracle() {
        let src = random_yuv444(64, 64, 1);
        let (main, aux) = chroma::split_yuv444(&src);
        let got = to_bgra(&main, Some(&aux), 1);
        let want = color::yuv444_to_bgra(&chroma::recombine_yuv444(&main, &aux));
        assert_eq!(got.len(), want.len());
        assert!(
            max_delta(&got, &want) <= 1,
            "delta {}",
            max_delta(&got, &want)
        );
    }

    #[test]
    fn single_matches_the_proto_oracle() {
        let src = random_yuv444(64, 64, 2);
        let main = chroma::yuv444_to_nv12(&src);
        let got = to_bgra(&main, None, 1);
        let want = color::yuv444_to_bgra(&chroma::nv12_to_yuv444(&main));
        assert_eq!(got.len(), want.len());
        assert!(
            max_delta(&got, &want) <= 1,
            "delta {}",
            max_delta(&got, &want)
        );
    }

    #[test]
    fn zoom_replicates_pixels() {
        let src = random_yuv444(64, 32, 3);
        let (main, aux) = chroma::split_yuv444(&src);
        let one = to_bgra(&main, Some(&aux), 1);
        let two = to_bgra(&main, Some(&aux), 2);
        assert_eq!(two.len(), one.len() * 4);
        for y in 0..32 {
            for x in 0..64 {
                let px = &one[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
                for (dy, dx) in [(0usize, 0usize), (0, 1), (1, 0), (1, 1)] {
                    let at = ((y * 2 + dy) * 128 + x * 2 + dx) * 4;
                    assert_eq!(&two[at..at + 4], px, "block at ({x},{y})");
                }
            }
        }
    }
}
