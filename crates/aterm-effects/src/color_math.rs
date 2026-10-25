// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared `0x00RRGGBB` color math.

#[inline]
const fn rem_euclid_pos(x: f32, rhs: f32) -> f32 {
    let r = x % rhs;
    if r < 0.0 { r + rhs } else { r }
}

/// Rotate a `0x00RRGGBB` color's hue by the §4.2 nudge code, preserving saturation and value.
#[must_use]
#[inline]
pub const fn hue_nudge(rgb: u32, code: u8) -> u32 {
    let (h, s, v) = rgb2hsv(rgb);
    hsv2rgb(h + (-18.0 + code as f32 * 2.4), s, v)
}

/// The hues a light page cannot show cleanly, in turns: deep orange through yellow to chartreuse.
pub const LIGHT_MUDDY_HUES: (f32, f32) = (36.0 / 360.0, 96.0 / 360.0);

/// `hue` (turns) as a spectrum laid on a light page shows it.
#[must_use]
#[inline]
pub const fn light_spectrum_hue(hue: f32) -> f32 {
    let (b0, b1) = LIGHT_MUDDY_HUES;
    let h = if hue >= 0.0 && hue < 1.0 {
        hue
    } else if hue.is_finite() {
        rem_euclid_pos(hue, 1.0)
    } else {
        0.0
    } * (1.0 - (b1 - b0));
    if h >= b0 { h + (b1 - b0) } else { h }
}

/// `0x00RRGGBB` → HSV (`h` degrees, `s`/`v` in `0..=1`); the inverse of [`hsv2rgb`].
#[must_use]
#[inline]
pub const fn rgb2hsv(rgb: u32) -> (f32, f32, f32) {
    let r = ((rgb >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb & 0xff) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let c = max - min;
    let h = if c == 0.0 {
        0.0
    } else if max == r {
        let d = (g - b) / c;
        60.0 * if d >= 0.0 { d } else { d + 6.0 }
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

#[inline]
pub(crate) const fn exp_f64_to_f32(y: f64) -> f32 {
    if y.is_nan() {
        return f32::NAN;
    }
    if y <= -104.0 {
        return 0.0;
    }
    if y >= 89.0 {
        return f32::INFINITY;
    }
    let q = y / std::f64::consts::LN_2;
    let n_int = if q >= 0.0 {
        (q + 0.5) as i32
    } else {
        (q - 0.5) as i32
    };
    let r = y - (n_int as f64) * std::f64::consts::LN_2;
    let mut exp_r = 1.0;
    let mut r_term = 1.0;
    let mut step = 1;
    while step <= 20 {
        r_term *= r / (step as f64);
        exp_r += r_term;
        step += 1;
    }
    let scale = f64::from_bits(((1023 + n_int) as u64) << 52);
    (exp_r * scale) as f32
}

#[inline]
pub(crate) const fn exp_f32(x: f32) -> f32 {
    exp_f64_to_f32(x as f64)
}

#[inline]
pub(crate) const fn sqrt_f64_to_f32(s: f64) -> f32 {
    if s.is_nan() || s < 0.0 {
        return f32::NAN;
    }
    if s == 0.0 || s.is_infinite() {
        return s as f32;
    }
    let mut r = f64::from_bits((s.to_bits() >> 1) + (1023u64 << 51));
    let mut step = 0;
    while step < 5 {
        r = 0.5 * (r + s / r);
        step += 1;
    }
    r as f32
}

#[inline]
pub(crate) const fn sqrt_f32(x: f32) -> f32 {
    if x < 0.0 {
        return f32::NAN;
    }
    if x == 0.0 {
        return x;
    }
    sqrt_f64_to_f32(x as f64)
}

#[inline]
pub(crate) const fn hypot_f32(x: f32, y: f32) -> f32 {
    if x.is_infinite() || y.is_infinite() {
        return f32::INFINITY;
    }
    let (xf, yf) = (x as f64, y as f64);
    sqrt_f64_to_f32(xf * xf + yf * yf)
}

#[inline]
pub(crate) const fn ln_pos_f64(x: f32) -> f64 {
    let xf = x as f64;
    let bits = xf.to_bits();
    let exp_bits = ((bits >> 52) & 0x7ff) as i32;
    let mut k = exp_bits - 1023;
    let mut m = f64::from_bits((bits & 0x000F_FFFF_FFFF_FFFF) | (1023u64 << 52));
    if m > std::f64::consts::SQRT_2 {
        m *= 0.5;
        k += 1;
    }
    let u = (m - 1.0) / (m + 1.0);
    let u2 = u * u;
    let mut term = u;
    let mut sum = u;
    let mut j = 3;
    while j <= 25 {
        term *= u2;
        sum += term / (j as f64);
        j += 2;
    }
    2.0 * sum + (k as f64) * std::f64::consts::LN_2
}

#[inline]
pub(crate) const fn ln_pos_f32(x: f32) -> f32 {
    if x.is_nan() || x < 0.0 {
        return f32::NAN;
    }
    if x == 0.0 {
        return f32::NEG_INFINITY;
    }
    if x.is_infinite() {
        return f32::INFINITY;
    }
    if x == 1.0 {
        return 0.0;
    }
    ln_pos_f64(x) as f32
}

#[inline]
pub(crate) const fn powf_pos_f32(x: f32, exp: f32) -> f32 {
    if x.is_nan() || exp.is_nan() {
        return f32::NAN;
    }
    if exp == 0.0 || x == 1.0 {
        return 1.0;
    }
    if x <= 0.0 {
        return if exp > 0.0 { 0.0 } else { f32::INFINITY };
    }
    if x.is_infinite() {
        return if exp > 0.0 { f32::INFINITY } else { 0.0 };
    }
    exp_f64_to_f32((exp as f64) * ln_pos_f64(x))
}

#[inline]
const fn srgb_pow_2_4(x: f32) -> f32 {
    powf_pos_f32(x, 2.4)
}

#[inline]
const fn srgb_encode_ref(c: f32) -> u8 {
    let c = if c.is_finite() {
        c.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let n = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * powf_pos_f32(c, 1.0 / 2.4) - 0.055
    };
    (n * 255.0).round().clamp(0.0, 255.0) as u8
}

const SRGB_ENCODE_THRESHOLDS: [f32; 255] = {
    let mut out = [0.0f32; 255];
    let mut k = 0;
    let mut lo_start = 0.0_f32.to_bits();
    let hi_max = 1.0_f32.to_bits();
    while k < 255 {
        let target = (k + 1) as u8;
        let mut lo = lo_start;
        let mut hi = hi_max;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if srgb_encode_ref(f32::from_bits(mid)) >= target {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        out[k] = f32::from_bits(lo);
        lo_start = lo;
        k += 1;
    }
    out
};

/// Encode a linear-light channel `c` (`0..=1`) to an sRGB byte `0..=255`.
#[inline]
#[must_use]
pub(crate) const fn srgb_encode(c: f32) -> u8 {
    if c.is_nan() || c <= 0.0 {
        return 0;
    }
    if c >= 1.0 {
        return 255;
    }
    let mut lo = 0usize;
    let mut hi = 255usize;
    while lo < hi {
        let mid = (lo + hi) >> 1;
        if c >= SRGB_ENCODE_THRESHOLDS[mid] {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo as u8
}

impl RelativeLuminanceTable {
    const fn build() -> Self {
        let mut linear = [0.0f32; 256];
        let mut red = [0.0f32; 256];
        let mut green = [0.0f32; 256];
        let mut blue = [0.0f32; 256];
        let mut i = 0;
        while i < 256 {
            let n = i as f32 / 255.0;
            let lin = if n <= 0.03928 {
                n / 12.92
            } else {
                srgb_pow_2_4((n + 0.055) / 1.055)
            };
            linear[i] = lin;
            red[i] = 0.2126 * lin;
            green[i] = 0.7152 * lin;
            blue[i] = 0.0722 * lin;
            i += 1;
        }
        Self {
            linear,
            red,
            green,
            blue,
        }
    }
}

static RELATIVE_LUMINANCE: RelativeLuminanceTable = RelativeLuminanceTable::build();

#[inline]
pub(crate) const fn relative_luminance_table() -> &'static RelativeLuminanceTable {
    &RELATIVE_LUMINANCE
}

#[inline]
#[must_use]
pub(crate) const fn relative_luminance_rgb(
    table: &RelativeLuminanceTable,
    r: usize,
    g: usize,
    b: usize,
) -> f32 {
    (table.red[r & 0xff] + table.green[g & 0xff]) + table.blue[b & 0xff]
}

#[inline]
#[must_use]
pub(crate) const fn relative_luminance_with(table: &RelativeLuminanceTable, rgb: u32) -> f32 {
    relative_luminance_rgb(
        table,
        ((rgb >> 16) & 0xff) as usize,
        ((rgb >> 8) & 0xff) as usize,
        (rgb & 0xff) as usize,
    )
}

/// WCAG relative luminance of a `0x00RRGGBB` colour.
#[must_use]
#[inline]
pub const fn relative_luminance(rgb: u32) -> f32 {
    relative_luminance_with(relative_luminance_table(), rgb)
}

/// WCAG contrast ratio (`>= 1`) between two `0x00RRGGBB` colours, on [`relative_luminance`].
#[must_use]
#[inline]
pub const fn contrast_ratio(a: u32, b: u32) -> f32 {
    let table = relative_luminance_table();
    let (la, lb) = (
        relative_luminance_with(table, a),
        relative_luminance_with(table, b),
    );
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Floor `rgb`'s WCAG contrast against `ground` to at least `min` by moving its lightness alone.
#[must_use]
pub const fn floor_contrast_keeping_hue(rgb: u32, ground: u32, min: f32) -> u32 {
    let rgb = rgb & 0x00FF_FFFF;
    if min.is_nan() || min <= 1.0 {
        return rgb;
    }
    let table = relative_luminance_table();
    let lg = relative_luminance_with(table, ground);
    #[inline]
    const fn meets(table: &RelativeLuminanceTable, lg: f32, min: f32, c: u32) -> bool {
        let lc = relative_luminance_with(table, c);
        (lc.max(lg) + 0.05) / (lc.min(lg) + 0.05) >= min
    }
    if meets(table, lg, min, rgb) {
        return rgb;
    }
    let darken = (lg + 0.05) * (lg + 0.05) >= 0.05 * 1.05;
    let end = if darken { 0x0000_0000 } else { 0x00FF_FFFF };
    if !meets(table, lg, min, end) {
        return end;
    }
    let (r, g, b) = (
        ((rgb >> 16) & 0xff) as f32,
        ((rgb >> 8) & 0xff) as f32,
        (rgb & 0xff) as f32,
    );
    let peak = r.max(g).max(b);
    let up = if peak > 0.0 { 255.0 / peak } else { 1.0 };
    let (r_up, g_up, b_up) = (r * up, g * up, b * up);
    #[inline]
    const fn round_chan(c: f32) -> u32 {
        c.round().clamp(0.0, 255.0) as u32
    }
    #[inline]
    const fn pack(r: f32, g: f32, b: f32) -> u32 {
        (round_chan(r) << 16) | (round_chan(g) << 8) | round_chan(b)
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (rgb, end);
    let mut i = 0;
    while i < 24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = if darken {
            let s = 1.0 - mid;
            pack(r * s, g * s, b * s)
        } else if mid <= 0.5 {
            let s = 1.0 + (up - 1.0) * (mid * 2.0);
            pack(r * s, g * s, b * s)
        } else {
            let u = (mid - 0.5) * 2.0;
            pack(
                r_up + (255.0 - r_up) * u,
                g_up + (255.0 - g_up) * u,
                b_up + (255.0 - b_up) * u,
            )
        };
        if mid_rgb == lo_rgb {
            lo = mid;
        } else if mid_rgb == hi_rgb || meets(table, lg, min, mid_rgb) {
            hi = mid;
            hi_rgb = mid_rgb;
        } else {
            lo = mid;
            lo_rgb = mid_rgb;
        }
        if i >= 1 && lo_rgb == hi_rgb {
            break;
        }
        i += 1;
    }
    hi_rgb
}

/// Lift `rgb` along its hue (its peak scaled to full, then tinted toward white) to the least
/// lightness at which `meets` holds, `meets` taken to hold from there to white: `rgb` itself where
/// it already does, white where nothing short of white does.
#[must_use]
pub(crate) fn lift_keeping_hue(rgb: u32, meets: impl Fn(u32) -> bool) -> u32 {
    let rgb = rgb & 0x00FF_FFFF;
    if meets(rgb) {
        return rgb;
    }
    least_on_hue_path(rgb, false, meets)
}

/// The first colour on `rgb`'s hue path at which `meets` holds, by bisection: toward black when
/// `darken`, else its peak scaled to full and then tinted toward white; the path's end where
/// `meets` fails even there.
fn least_on_hue_path(rgb: u32, darken: bool, meets: impl Fn(u32) -> bool) -> u32 {
    let end = if darken { 0x0000_0000 } else { 0x00FF_FFFF };
    if !meets(end) {
        return end;
    }
    let chan = |sh: u32| ((rgb >> sh) & 0xff) as f32;
    let (r, g, b) = (chan(16), chan(8), chan(0));
    let peak = r.max(g).max(b);
    let up = if peak > 0.0 { 255.0 / peak } else { 1.0 };
    let (r_up, g_up, b_up) = (r * up, g * up, b * up);
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
            let tint = |c_up: f32| c_up + (255.0 - c_up) * u;
            pack(tint(r_up), tint(g_up), tint(b_up))
        }
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (rgb & 0x00FF_FFFF, end);
    for i in 0..24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = path(mid);
        if mid_rgb == lo_rgb {
            lo = mid;
        } else if mid_rgb == hi_rgb || meets(mid_rgb) {
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
pub const fn light_body_fill(rgb: u32, heat: f32) -> u32 {
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
pub const fn light_charge_in_hue(rgb: u32, k: f32) -> u32 {
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
pub const fn wear_pinned(pinned: u32, styled: u32, charge: f32, settled: f32) -> u32 {
    let k = if charge.is_finite() && settled < 1.0 {
        ((charge - settled) / (1.0 - settled)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if k <= 0.0 || pinned == styled {
        return pinned & 0x00FF_FFFF;
    }
    const fn ch(pinned: u32, styled: u32, k: f32, sh: u32) -> u32 {
        let (a, b) = (
            ((pinned >> sh) & 0xff) as f32,
            ((styled >> sh) & 0xff) as f32,
        );
        ((a + (b - a) * k).round().clamp(0.0, 255.0) as u32) << sh
    }
    ch(pinned, styled, k, 16) | ch(pinned, styled, k, 8) | ch(pinned, styled, k, 0)
}

#[inline]
pub(crate) const fn cbrt_f32(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let xf = x as f64;
    let mut y = f64::from_bits(0x2AA0_0000_0000_0000 + xf.to_bits() / 3);
    let mut i = 0;
    while i < 6 {
        y = (2.0 * y + xf / (y * y)) / 3.0;
        i += 1;
    }
    y as f32
}

/// OKLab `[L, a, b]` of a `0x00RRGGBB` colour (Björn Ottosson's matrices), the uniform space.
#[inline]
pub(crate) const fn oklab(rgb: u32) -> [f32; 3] {
    let linear = &relative_luminance_table().linear;
    let r = linear[((rgb >> 16) & 0xff) as usize];
    let g = linear[((rgb >> 8) & 0xff) as usize];
    let b = linear[(rgb & 0xff) as usize];
    let l = cbrt_f32(0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b);
    let m = cbrt_f32(0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b);
    let s = cbrt_f32(0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b);
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// [`oklab`]'s inverse.
#[inline]
pub(crate) const fn oklab_to_rgb([l, a, b]: [f32; 3]) -> u32 {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = srgb_encode(4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3) as u32;
    let g = srgb_encode(-1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3) as u32;
    let bl = srgb_encode(-0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3) as u32;
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
#[must_use]
#[inline]
pub const fn hsv2rgb(h: f32, s: f32, v: f32) -> u32 {
    const fn q(f: f32, m: f32) -> u32 {
        ((f + m).clamp(0.0, 1.0) * 255.0).round() as u32
    }
    let c = v * s;
    let h_deg = if h >= 0.0 && h < 360.0 {
        h
    } else if h >= 360.0 && h < 720.0 {
        h - 360.0
    } else {
        rem_euclid_pos(h, 360.0)
    };
    let hp = h_deg / 60.0;
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
    (q(r, m) << 16) | (q(g, m) << 8) | q(b, m)
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

    /// The lift is the page floor's lightening path held to any test: on a near-black ground,
    /// where the floor lightens, lifting to the floor's own ratio gives the floor's colour, and a
    /// colour that already passes is kept. NEGATIVE CONTROL: on a mid grey the floor darkens the
    /// blue while the lift lightens it past the grey, so the two are not one path everywhere.
    #[test]
    fn the_lift_is_the_floors_lightening_path() {
        let night = 0x0011_1318;
        for rgb in [0x0051_0A00, 0x0000_00FF, 0x004B_0082, 0x0094_00D3] {
            let floored = floor_contrast_keeping_hue(rgb, night, 4.5);
            assert_ne!(floored, rgb, "CONTROL: {rgb:06x} starts under the floor");
            assert_eq!(
                lift_keeping_hue(rgb, |c| contrast_ratio(c, night) >= 4.5),
                floored,
                "{rgb:06x} on {night:06x}"
            );
        }
        assert_eq!(lift_keeping_hue(0x0000_00FF, |_| true), 0x0000_00FF);
        let grey = 0x0092_9292;
        let lifted = lift_keeping_hue(0x0000_00FF, |c| contrast_ratio(c, grey) >= 3.0);
        assert!(
            lifted != floor_contrast_keeping_hue(0x0000_00FF, grey, 3.0)
                && relative_luminance(lifted) > relative_luminance(grey),
            "NEGATIVE CONTROL: on {grey:06x} the lift lightens the blue to {lifted:06x}"
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

    #[test]
    fn srgb_linear_table_matches_powf_within_one_ulp() {
        let table = relative_luminance_table();
        for i in 0..256 {
            let n = i as f32 / 255.0;
            let expected = if n <= 0.03928 {
                n / 12.92
            } else {
                ((n + 0.055) / 1.055).powf(2.4)
            };
            let ulp = table.linear[i].to_bits().abs_diff(expected.to_bits());
            assert!(
                ulp <= 1,
                "channel byte {i}: got {:08x} ({}), expected {:08x} ({expected}), ulp={ulp}",
                table.linear[i].to_bits(),
                table.linear[i],
                expected.to_bits(),
            );
        }
    }
}
