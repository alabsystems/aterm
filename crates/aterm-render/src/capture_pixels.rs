// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A presented destination's mapped texels decoded to straight sRGB RGBA8, for a `video` take
//! and a presented snapshot: pixel code, built optimized with the rest of this crate even in a
//! debug build, where at no optimization a debug build's take converted about one frame a
//! second and kept almost none.

use std::sync::LazyLock;

use crate::{linear_to_srgb, srgb8_to_linear};

/// How a mapped destination's texels decode to sRGB RGBA8: their size, layout and colour space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexelDecode {
    /// Bytes per texel.
    pub bpp: usize,
    /// Half-float extended-linear (scRGB) channels, tone-mapped to SDR.
    pub f16: bool,
    /// 8-bit channels in BGRA order.
    pub bgra: bool,
    /// 8-bit Display-P3 coordinates, converted to sRGB.
    pub display_p3: bool,
    /// The scRGB reference-white multiplier an f16 texel is divided by.
    pub sdr_white_scale: f32,
}

impl TexelDecode {
    /// The texel at byte `base` of `data` as straight, non-linear sRGB RGBA8 channels.
    #[inline]
    pub fn fetch(&self, data: &[u8], base: usize) -> [u32; 4] {
        if self.f16 {
            let half = |channel: usize| {
                u16::from_le_bytes([data[base + channel * 2], data[base + channel * 2 + 1]])
            };
            let linear = tone_map_scrgb_to_sdr(
                [
                    f16_to_f32(half(0)),
                    f16_to_f32(half(1)),
                    f16_to_f32(half(2)),
                ],
                self.sdr_white_scale,
            );
            let alpha = (f16_to_f32(half(3)).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
            [
                srgb8_from_linear(linear[0]),
                srgb8_from_linear(linear[1]),
                srgb8_from_linear(linear[2]),
                alpha,
            ]
        } else {
            let raw = if self.bgra {
                [data[base + 2], data[base + 1], data[base], data[base + 3]]
            } else {
                [data[base], data[base + 1], data[base + 2], data[base + 3]]
            };
            if self.display_p3 {
                let srgb = display_p3_to_srgb8([raw[0], raw[1], raw[2]]);
                [
                    u32::from(srgb[0]),
                    u32::from(srgb[1]),
                    u32::from(srgb[2]),
                    u32::from(raw[3]),
                ]
            } else {
                raw.map(u32::from)
            }
        }
    }
}

/// Validated `(width, height)` for a mapped `src_w`×`src_h` buffer with `padded_row` stride.
pub fn mapped_geometry(
    data_len: usize,
    src_w: u32,
    src_h: u32,
    padded_row: usize,
    bpp: usize,
) -> Result<(usize, usize), String> {
    let sw = usize::try_from(src_w)
        .map_err(|_| "presented-frame width does not fit memory".to_string())?;
    let sh = usize::try_from(src_h)
        .map_err(|_| "presented-frame height does not fit memory".to_string())?;
    let row_bytes = sw
        .checked_mul(bpp)
        .ok_or_else(|| "presented-frame row size overflow".to_string())?;
    if padded_row < row_bytes {
        return Err(format!(
            "presented-frame row stride {padded_row} is smaller than {row_bytes}"
        ));
    }
    let required = if sh == 0 {
        0
    } else {
        (sh - 1)
            .checked_mul(padded_row)
            .and_then(|n| n.checked_add(row_bytes))
            .ok_or_else(|| "presented-frame buffer size overflow".to_string())?
    };
    if data_len < required {
        return Err(format!(
            "presented-frame buffer has {data_len} bytes, expected at least {required}"
        ));
    }
    Ok((sw, sh))
}

/// Strip row padding, swizzle or decode the source format, and optionally box downsample 2×2.
pub fn mapped_to_rgba8(
    data: &[u8],
    src_w: u32,
    src_h: u32,
    padded_row: usize,
    texel: TexelDecode,
    half_res: bool,
) -> Result<(Vec<u8>, u32, u32), String> {
    let bpp = texel.bpp;
    let (sw, sh) = mapped_geometry(data.len(), src_w, src_h, padded_row, bpp)?;

    let (dw, dh) = if half_res {
        (sw.div_ceil(2), sh.div_ceil(2))
    } else {
        (sw, sh)
    };
    let out_len = dw
        .checked_mul(dh)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "presented-frame output size overflow".to_string())?;
    let mut rgba = vec![0u8; out_len];
    let fetch = |x: usize, y: usize| texel.fetch(data, y * padded_row + x * bpp);

    for dy in 0..dh {
        for dx in 0..dw {
            let pixel = if half_res {
                let (x, y) = (dx * 2, dy * 2);
                let (a, b, c, d) = (
                    fetch(x, y),
                    fetch((x + 1).min(sw - 1), y),
                    fetch(x, (y + 1).min(sh - 1)),
                    fetch((x + 1).min(sw - 1), (y + 1).min(sh - 1)),
                );
                let samples = [a, b, c, d];
                let alpha_sum: u32 = samples.iter().map(|sample| sample[3]).sum();
                let rgb = if alpha_sum == 0 {
                    [0, 0, 0]
                } else {
                    std::array::from_fn(|channel| {
                        let premul_linear: f32 = samples
                            .iter()
                            .map(|sample| {
                                let linear = srgb8_to_linear(sample[channel] as u8);
                                linear * sample[3] as f32
                            })
                            .sum();
                        srgb8_from_linear(premul_linear / alpha_sum as f32)
                    })
                };
                [rgb[0], rgb[1], rgb[2], (alpha_sum + 2) / 4]
            } else {
                fetch(dx, dy)
            };
            let out = (dy * dw + dx) * 4;
            rgba[out] = pixel[0] as u8;
            rgba[out + 1] = pixel[1] as u8;
            rgba[out + 2] = pixel[2] as u8;
            rgba[out + 3] = pixel[3] as u8;
        }
    }
    Ok((rgba, dw as u32, dh as u32))
}

/// IEEE 754 half → f32.
#[must_use]
pub const fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let frac = (bits & 0x3ff) as u32;
    let f = match (exp, frac) {
        (0, 0) => sign << 31,
        (0, _) => {
            let mut e = 127 - 15 + 1;
            let mut m = frac;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            (sign << 31) | ((e as u32) << 23) | ((m & 0x3ff) << 13)
        }
        (0x1f, 0) => (sign << 31) | 0x7f80_0000,
        (0x1f, _) => (sign << 31) | 0x7fc0_0000,
        _ => (sign << 31) | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(f)
}

/// The 8-bit sRGB encode of a linear channel: clamp to `[0, 1]`, apply the OETF, and round half up.
#[must_use]
pub fn srgb8_from_linear_powf(linear: f32) -> u32 {
    (linear_to_srgb(linear.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32
}

/// Entry `k - 1` is the least `f32` whose [`srgb8_from_linear_powf`] is at least `k`, for `k` in `1..=255`.
pub static SRGB8_ENCODE_THRESHOLDS: LazyLock<[f32; 255]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        let byte = index as u32 + 1;
        let (mut lo, mut hi) = (0.0_f32.to_bits(), 1.0_f32.to_bits());
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if srgb8_from_linear_powf(f32::from_bits(mid)) >= byte {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        f32::from_bits(hi)
    })
});

/// [`srgb8_from_linear_powf`] read from [`SRGB8_ENCODE_THRESHOLDS`].
#[inline]
#[must_use]
pub fn srgb8_from_linear(linear: f32) -> u32 {
    srgb8_by_thresholds(&SRGB8_ENCODE_THRESHOLDS, linear)
}

/// Number of `thresholds` at or below `linear`, by an eight-step binary search of the sorted table.
#[inline]
#[must_use]
pub fn srgb8_by_thresholds(thresholds: &[f32; 255], linear: f32) -> u32 {
    let (mut below, mut step) = (0_usize, 128_usize);
    while step > 0 {
        below += step * usize::from(linear >= thresholds[below + step - 1]);
        step /= 2;
    }
    below as u32
}

/// The CSS Color 4 Display-P3-to-linear-sRGB matrix, on linear channels.
#[must_use]
pub fn display_p3_linear_to_srgb_linear([r, g, b]: [f32; 3]) -> [f32; 3] {
    [
        1.224_940_2_f32.mul_add(r, -0.224_940_18 * g),
        (-0.042_056_955_f32).mul_add(r, 1.042_057 * g),
        (-0.019_637_555_f32).mul_add(r, (-0.078_636_05_f32).mul_add(g, 1.098_273_6 * b)),
    ]
}

/// Convert 8-bit Display-P3 coordinates to sRGB via the Display-P3-to-linear-sRGB matrix.
#[must_use]
pub fn display_p3_to_srgb8(p3: [u8; 3]) -> [u8; 3] {
    display_p3_linear_to_srgb_linear(p3.map(srgb8_to_linear))
        .map(|channel| srgb8_from_linear(channel) as u8)
}

/// Tone-map scRGB/EDR to SDR-linear by removing `sdr_white_scale` and compressing over-white channels by peak.
#[must_use]
pub fn tone_map_scrgb_to_sdr(scrgb: [f32; 3], sdr_white_scale: f32) -> [f32; 3] {
    let mut linear = scrgb.map(|channel| {
        let bounded = if channel.is_nan() || channel <= 0.0 {
            0.0
        } else if channel.is_infinite() {
            f32::MAX
        } else {
            channel
        };
        bounded / sdr_white_scale
    });
    let peak = linear[0].max(linear[1]).max(linear[2]);
    if peak > 1.0 {
        linear = linear.map(|channel| channel / peak);
    }
    linear
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srgb_to_linear;

    /// The ulps on each side of a threshold that are walked one by one.
    const THRESHOLD_WINDOW_ULPS: u32 = 4096;

    /// Where `thresholds` departs from the encode's definition near its own entries, if it does.
    fn threshold_table_error(thresholds: &[f32; 255]) -> Option<String> {
        for (index, &threshold) in thresholds.iter().enumerate() {
            let byte = index as u32 + 1;
            let bits = threshold.to_bits();
            let below = f32::from_bits(bits - 1);
            if srgb8_from_linear_powf(below) != byte - 1
                || srgb8_from_linear_powf(threshold) != byte
            {
                return Some(format!(
                    "threshold {byte} at {threshold:e} is not the first f32 of its byte"
                ));
            }
            let mut previous = 0;
            for pattern in bits - THRESHOLD_WINDOW_ULPS..=bits + THRESHOLD_WINDOW_ULPS {
                let linear = f32::from_bits(pattern);
                let exact = srgb8_from_linear_powf(linear);
                if exact < previous {
                    return Some(format!("the encode decreases at {linear:e}"));
                }
                let read = srgb8_by_thresholds(thresholds, linear);
                if read != exact {
                    return Some(format!(
                        "the table reads {read} at {linear:e}, the encode {exact}"
                    ));
                }
                previous = exact;
            }
        }
        None
    }

    /// `srgb8_from_linear_powf` before its floor, in both `f32` and `f64` over the same `f32` constants.
    fn srgb8_unrounded(linear: f32) -> (f64, f64) {
        let clamped = linear.clamp(0.0, 1.0);
        let in_f32 = f64::from(linear_to_srgb(clamped) * 255.0 + 0.5);
        let x = f64::from(clamped);
        let encoded = if clamped <= 0.003_130_8 {
            f64::from(12.92_f32) * x
        } else {
            f64::from(1.055_f32) * x.powf(f64::from(1.0_f32 / 2.4)) - f64::from(0.055_f32)
        };
        (in_f32, encoded * 255.0 + 0.5)
    }

    /// The table matches the definition for every `f32` across each threshold window.
    #[test]
    fn encode_thresholds_are_exact_and_locally_monotone() {
        let thresholds = &*SRGB8_ENCODE_THRESHOLDS;
        assert_eq!(threshold_table_error(thresholds), None);
        assert!(
            thresholds.windows(2).all(|pair| pair[0] < pair[1]),
            "the thresholds ascend"
        );
        for (index, &threshold) in thresholds.iter().enumerate() {
            let byte = f64::from(index as u32 + 1);
            let bits = threshold.to_bits();
            let edge = |pattern: u32| srgb8_unrounded(f32::from_bits(pattern)).1;
            let margin = (byte - edge(bits - THRESHOLD_WINDOW_ULPS))
                .min(edge(bits + THRESHOLD_WINDOW_ULPS) - byte);
            let stray = (bits - THRESHOLD_WINDOW_ULPS..=bits + THRESHOLD_WINDOW_ULPS)
                .map(|pattern| {
                    let (in_f32, exact) = srgb8_unrounded(f32::from_bits(pattern));
                    (in_f32 - exact).abs()
                })
                .fold(0.0, f64::max);
            assert!(
                margin > 16.0 * stray,
                "byte {byte}: margin {margin:e} against an f32 stray of {stray:e}"
            );
        }
        assert_eq!(
            srgb8_from_linear(f32::NAN),
            srgb8_from_linear_powf(f32::NAN)
        );
        for linear in [f32::NEG_INFINITY, -1.0, -0.0, 0.0, 1.0, 2.0, f32::INFINITY] {
            assert_eq!(
                srgb8_from_linear(linear),
                srgb8_from_linear_powf(linear),
                "{linear}"
            );
        }

        let mut late = *thresholds;
        late[127] = f32::from_bits(late[127].to_bits() + 1);
        assert!(
            threshold_table_error(&late).is_some(),
            "negative control: an entry one ulp late is caught"
        );
        let mut early = *thresholds;
        early[0] = f32::from_bits(early[0].to_bits() - 1);
        assert!(
            threshold_table_error(&early).is_some(),
            "negative control: an entry one ulp early is caught"
        );
    }

    /// The decode table holds `srgb_to_linear` bit for bit and every P3 triple encodes alike by table and `powf`.
    #[test]
    fn display_p3_lut_matches_powf_over_every_input() {
        let decoded: [f32; 256] = std::array::from_fn(|byte| {
            let by_powf = srgb_to_linear(byte as f32 / 255.0);
            assert_eq!(
                srgb8_to_linear(byte as u8).to_bits(),
                by_powf.to_bits(),
                "decode of {byte}"
            );
            by_powf
        });
        let decoded = &decoded;
        std::thread::scope(|scope| {
            for eighth in 0..8_u8 {
                scope.spawn(move || {
                    for r in eighth * 32..=eighth * 32 + 31 {
                        for g in 0..=u8::MAX {
                            for b in 0..=u8::MAX {
                                let p3 = [r, g, b];
                                let linear = display_p3_linear_to_srgb_linear(
                                    p3.map(|channel| decoded[usize::from(channel)]),
                                );
                                let by_powf =
                                    linear.map(|channel| srgb8_from_linear_powf(channel) as u8);
                                assert_eq!(display_p3_to_srgb8(p3), by_powf, "{p3:?}");
                            }
                        }
                    }
                });
            }
        });
    }

    #[test]
    fn f16_lut_matches_powf() {
        let agree = |linear: [f32; 3]| {
            for channel in linear {
                assert_eq!(
                    srgb8_from_linear(channel),
                    srgb8_from_linear_powf(channel),
                    "{channel:e}"
                );
            }
        };
        for bits in 0..=u16::MAX {
            agree(tone_map_scrgb_to_sdr([f16_to_f32(bits); 3], 1.0));
        }
        let mut state = 0x5eed_u64;
        for _ in 0..1_000_000 {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut word = state;
            word = (word ^ (word >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            word = (word ^ (word >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            word ^= word >> 31;
            let scrgb = [0, 16, 32].map(|shift| f16_to_f32((word >> shift) as u16));
            let sdr_white_scale = 1.0 + f32::from((word >> 48) as u16) / 8192.0;
            agree(tone_map_scrgb_to_sdr(scrgb, sdr_white_scale));
        }
    }
}
