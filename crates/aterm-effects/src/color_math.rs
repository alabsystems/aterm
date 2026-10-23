// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared `0x00RRGGBB` color math.

/// Rotate a `0x00RRGGBB` color's hue by the §4.2 nudge code, preserving saturation and value.
#[inline]
pub fn hue_nudge(rgb: u32, code: u8) -> u32 {
    let (h, s, v) = rgb2hsv(rgb);
    hsv2rgb(h + (-18.0 + f32::from(code) * 2.4), s, v)
}

/// The hues a light page cannot show cleanly, in turns: deep orange through yellow to chartreuse.
pub const LIGHT_MUDDY_HUES: (f32, f32) = (36.0 / 360.0, 96.0 / 360.0);

/// `hue` (turns) as a spectrum laid on a light page shows it.
#[must_use]
#[inline]
pub fn light_spectrum_hue(hue: f32) -> f32 {
    let (b0, b1) = LIGHT_MUDDY_HUES;
    let h = if hue.is_finite() {
        hue.rem_euclid(1.0)
    } else {
        0.0
    } * (1.0 - (b1 - b0));
    if h >= b0 { h + (b1 - b0) } else { h }
}

/// `0x00RRGGBB` → HSV (`h` degrees, `s`/`v` in `0..=1`); the inverse of [`hsv2rgb`].
#[inline]
pub fn rgb2hsv(rgb: u32) -> (f32, f32, f32) {
    let r = ((rgb >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb & 0xff) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let c = max - min;
    let h = if c == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / c).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / c + 2.0)
    } else {
        60.0 * ((r - g) / c + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { c / max };
    (h, s, max)
}

/// WCAG channel linearization times channel weight, per byte value.
pub(crate) struct RelativeLuminanceTable {
    pub(crate) linear: [f32; 256],
    red: [f32; 256],
    green: [f32; 256],
    blue: [f32; 256],
}

static RELATIVE_LUMINANCE: std::sync::LazyLock<RelativeLuminanceTable> =
    std::sync::LazyLock::new(|| {
        let linear: [f32; 256] = std::array::from_fn(|i| {
            let n = i as f32 / 255.0;
            if n <= 0.03928 {
                n / 12.92
            } else {
                ((n + 0.055) / 1.055).powf(2.4)
            }
        });
        RelativeLuminanceTable {
            red: std::array::from_fn(|i| 0.2126 * linear[i]),
            green: std::array::from_fn(|i| 0.7152 * linear[i]),
            blue: std::array::from_fn(|i| 0.0722 * linear[i]),
            linear,
        }
    });

#[inline]
pub(crate) fn relative_luminance_table() -> &'static RelativeLuminanceTable {
    &RELATIVE_LUMINANCE
}

#[inline]
pub(crate) fn relative_luminance_rgb(
    table: &RelativeLuminanceTable,
    r: usize,
    g: usize,
    b: usize,
) -> f32 {
    (table.red[r & 0xff] + table.green[g & 0xff]) + table.blue[b & 0xff]
}

#[inline]
pub(crate) fn relative_luminance_with(table: &RelativeLuminanceTable, rgb: u32) -> f32 {
    relative_luminance_rgb(
        table,
        ((rgb >> 16) & 0xff) as usize,
        ((rgb >> 8) & 0xff) as usize,
        (rgb & 0xff) as usize,
    )
}

/// WCAG relative luminance of a `0x00RRGGBB` colour.
#[inline]
pub fn relative_luminance(rgb: u32) -> f32 {
    relative_luminance_with(relative_luminance_table(), rgb)
}

/// WCAG contrast ratio (`>= 1`) between two `0x00RRGGBB` colours, on [`relative_luminance`].
#[must_use]
#[inline]
pub fn contrast_ratio(a: u32, b: u32) -> f32 {
    let table = relative_luminance_table();
    let (la, lb) = (
        relative_luminance_with(table, a),
        relative_luminance_with(table, b),
    );
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Floor `rgb`'s WCAG contrast against `ground` to at least `min` by moving its lightness alone.
#[must_use]
pub fn floor_contrast_keeping_hue(rgb: u32, ground: u32, min: f32) -> u32 {
    let rgb = rgb & 0x00FF_FFFF;
    if min.partial_cmp(&1.0) != Some(std::cmp::Ordering::Greater) {
        return rgb;
    }
    let table = relative_luminance_table();
    let lg = relative_luminance_with(table, ground);
    let meets = |c: u32| {
        let lc = relative_luminance_with(table, c);
        (lc.max(lg) + 0.05) / (lc.min(lg) + 0.05) >= min
    };
    if meets(rgb) {
        return rgb;
    }
    let darken = (lg + 0.05) * (lg + 0.05) >= 0.05 * 1.05;
    let chan = |sh: u32| ((rgb >> sh) & 0xff) as f32;
    let (r, g, b) = (chan(16), chan(8), chan(0));
    let peak = r.max(g).max(b);
    let up = if peak > 0.0 { 255.0 / peak } else { 1.0 };
    let pack = |r: f32, g: f32, b: f32| {
        let q = |c: f32| (c.round().clamp(0.0, 255.0)) as u32;
        (q(r) << 16) | (q(g) << 8) | q(b)
    };
    let path = |t: f32| -> u32 {
        if darken {
            let s = 1.0 - t;
            pack(r * s, g * s, b * s)
        } else if t <= 0.5 {
            let s = 1.0 + (up - 1.0) * (t * 2.0);
            pack(r * s, g * s, b * s)
        } else {
            let u = (t - 0.5) * 2.0;
            let tint = |c: f32| c * up + (255.0 - c * up) * u;
            pack(tint(r), tint(g), tint(b))
        }
    };
    let end = path(1.0);
    if !meets(end) {
        return end;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (rgb, end);
    for i in 0..24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = path(mid);
        if meets(mid_rgb) {
            hi = mid;
            hi_rgb = mid_rgb;
        } else {
            lo = mid;
            lo_rgb = mid_rgb;
        }
        if i >= 1 && lo_rgb == hi_rgb {
            break;
        }
    }
    hi_rgb
}

/// Below this HSV saturation a colour reads as grey: its hue is noise, so
/// [`light_body_fill`] leaves its saturation alone instead of inventing one.
const CHROMA_FLOOR: f32 = 0.10;

/// How far [`light_body_fill`] lowers value at full heat.
const LIGHT_DEPTH_MAX: f32 = 0.40;

/// A caret body's fill on a LIGHT page at `heat` `0..=1`: the hue concentrated instead of whitened.
#[must_use]
#[inline]
pub fn light_body_fill(rgb: u32, heat: f32) -> u32 {
    let heat = if heat.is_finite() {
        heat.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (h, s, v) = rgb2hsv(rgb & 0x00FF_FFFF);
    let s = if s < CHROMA_FLOOR {
        s
    } else {
        s + (1.0 - s) * (0.6 + 0.4 * heat)
    };
    hsv2rgb(h, s, v * (1.0 - LIGHT_DEPTH_MAX * heat))
}

/// A caret colour `rgb` charged by `k` `0..=1` on a LIGHT page.
#[must_use]
#[inline]
pub fn light_charge_in_hue(rgb: u32, k: f32) -> u32 {
    let k = if k.is_finite() {
        k.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if k <= 0.0 {
        return rgb & 0x00FF_FFFF;
    }
    let (h, s, v) = rgb2hsv(rgb & 0x00FF_FFFF);
    let s = if s < CHROMA_FLOOR {
        s
    } else {
        s + (1.0 - s) * k
    };
    hsv2rgb(h, s, v * (1.0 - LIGHT_DEPTH_MAX * k))
}

/// A caret body built on a PINNED cursor colour.
#[must_use]
#[inline]
pub fn wear_pinned(pinned: u32, styled: u32, charge: f32, settled: f32) -> u32 {
    let k = if charge.is_finite() && settled < 1.0 {
        ((charge - settled) / (1.0 - settled)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if k <= 0.0 || pinned == styled {
        return pinned & 0x00FF_FFFF;
    }
    let ch = |sh: u32| {
        let (a, b) = (
            ((pinned >> sh) & 0xff) as f32,
            ((styled >> sh) & 0xff) as f32,
        );
        ((a + (b - a) * k).round().clamp(0.0, 255.0) as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
}

/// OKLab `[L, a, b]` of a `0x00RRGGBB` colour (Björn Ottosson's matrices), the uniform space.
#[inline]
pub(crate) fn oklab(rgb: u32) -> [f32; 3] {
    let linear = &relative_luminance_table().linear;
    let r = linear[((rgb >> 16) & 0xff) as usize];
    let g = linear[((rgb >> 8) & 0xff) as usize];
    let b = linear[(rgb & 0xff) as usize];
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// [`oklab`]'s inverse.
#[inline]
pub(crate) fn oklab_to_rgb([l, a, b]: [f32; 3]) -> u32 {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let byte = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        let n = if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (n * 255.0).round().clamp(0.0, 255.0) as u32
    };
    let r = byte(4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3);
    let g = byte(-1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3);
    let bl = byte(-0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3);
    (r << 16) | (g << 8) | bl
}

/// The Euclidean OKLab distance between two `0x00RRGGBB` colours.
#[cfg(test)]
#[inline]
pub(crate) fn oklab_distance(a: u32, b: u32) -> f32 {
    let (x, y) = (oklab(a), oklab(b));
    let (dl, da, db) = (x[0] - y[0], x[1] - y[1], x[2] - y[2]);
    (dl * dl + da * da + db * db).sqrt()
}

/// HSV (`h` degrees, `s`/`v` in `0..=1`) → `0x00RRGGBB`.
#[inline]
pub fn hsv2rgb(h: f32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let q = |f: f32| ((f + m).clamp(0.0, 1.0) * 255.0).round() as u32;
    (q(r) << 16) | (q(g) << 8) | q(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page floor clears the ratio on both poles, moves lightness alone
    /// (the HSV hue of the floored colour is the input's), and leaves a
    /// colour that already clears it byte-identical.
    #[test]
    fn the_page_floor_clears_the_ratio_and_keeps_the_hue() {
        for (rgb, ground) in [
            (0x0051_0A00, 0x0011_1318),
            (0x000F_70B7, 0x0011_1318),
            (0x00B4_8CFF, 0x00FF_FFFF),
            (0x00FF_D9A0, 0x00FF_FFFF),
            (0x009E_D6FF, 0x00FD_F6E3),
            (0x0000_00FF, 0x0011_1318),
        ] {
            let before = contrast_ratio(rgb, ground);
            assert!(before < 4.5, "control: {rgb:06x} starts under the floor");
            let floored = floor_contrast_keeping_hue(rgb, ground, 4.5);
            assert!(
                contrast_ratio(floored, ground) >= 4.5,
                "{rgb:06x} on {ground:06x} floored to {floored:06x} at {:.2}:1",
                contrast_ratio(floored, ground)
            );
            let (h0, s0, _) = rgb2hsv(rgb);
            let (h1, _, _) = rgb2hsv(floored);
            let dh = (h0 - h1).abs().min(360.0 - (h0 - h1).abs());
            assert!(
                s0 < 0.05 || dh < 4.0,
                "{rgb:06x} → {floored:06x}: the hue moved {dh:.1}°"
            );
        }
        for (rgb, ground) in [(0x0009_69DA, 0x00FF_FFFF), (0x00FF_FFFF, 0x0011_1318)] {
            assert_eq!(floor_contrast_keeping_hue(rgb, ground, 4.5), rgb);
        }
        assert_eq!(
            floor_contrast_keeping_hue(0x0011_1318, 0x0011_1318, 1.0),
            0x0011_1318
        );
        assert_eq!(
            floor_contrast_keeping_hue(0x0011_1318, 0x0011_1318, f32::NAN),
            0x0011_1318
        );
    }

    /// The light-page body fill deepens with heat (lower luminance, higher
    /// saturation, the same hue), and a grey stays grey.
    #[test]
    fn the_light_body_fill_concentrates_the_hue_as_it_heats() {
        for rgb in [0x00B4_8CFF, 0x0009_69DA, 0x009E_D6FF, 0x00FF_D9A0] {
            let (h0, s0, _) = rgb2hsv(rgb);
            let rest = light_body_fill(rgb, 0.0);
            let hot = light_body_fill(rgb, 1.0);
            let (hr, sr, _) = rgb2hsv(rest);
            let (hh, sh, _) = rgb2hsv(hot);
            assert!(sr >= s0 && sh >= sr, "{rgb:06x}: saturation climbs");
            assert!(
                relative_luminance(hot) < relative_luminance(rest),
                "{rgb:06x}: heat deepens the fill ({rest:06x} → {hot:06x})"
            );
            for h in [hr, hh] {
                let dh = (h0 - h).abs().min(360.0 - (h0 - h).abs());
                assert!(dh < 4.0, "{rgb:06x}: the hue moved {dh:.1}°");
            }
        }
        let grey = light_body_fill(0x0080_8080, 0.5);
        let (_, s, _) = rgb2hsv(grey);
        assert!(s < 0.02, "a grey stays grey, got {grey:06x}");
    }
}
