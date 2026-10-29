// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow spectrum colour lookup ([`SPECTRUM_ANCHORS`](crate::spectrum::SPECTRUM_ANCHORS), [`SPECTRUM_STRIDE`](crate::spectrum::SPECTRUM_STRIDE), [`SPECTRUM_LUT`](crate::spectrum::SPECTRUM_LUT),
//! [`spectrum_with_min_saturation`](crate::spectrum::spectrum_with_min_saturation)):
//!
//! ```text
//! spectrum(t) -> rgb       t in [0, 1],  0 = red ... 1 = violet
//! ```

use crate::effect_util::lerp_rgb;

/// The seven names, `0x00RRGGBB`, red → violet.
pub const SPECTRUM_ANCHORS: [u32; SPECTRUM_STOPS] = [
    0x00FF_0000,
    0x00FF_7F00,
    0x00FF_FF00,
    0x0000_FF00,
    0x0000_00FF,
    0x004B_0082,
    0x0094_00D3,
];

/// How many named stops the arc has.
pub const SPECTRUM_STOPS: usize = 7;

/// Index of the green→blue interval, the one drawn in HSV rather than per
/// channel.
#[cfg(test)]
const SPECTRUM_CROSSING_SEG: usize = 3;

/// The LUT's length: **511 entries, just under 2 KiB**.
pub const SPECTRUM_LUT_LEN: usize = 511;

/// The path's slots per anchor interval, the coordinate the arc is drawn in.
pub const SPECTRUM_STRIDE: usize = (SPECTRUM_LUT_LEN - 1) / (SPECTRUM_STOPS - 1);
const _: () = assert!(
    SPECTRUM_STRIDE * (SPECTRUM_STOPS - 1) == SPECTRUM_LUT_LEN - 1,
    "the seven anchors must land on exact path slots"
);

/// The share of the pace that is hue, against OKLab distance: `0.60`.
#[cfg(test)]
const SPECTRUM_PACE_HUE_SHARE: f64 = 0.60;

/// The share of the pace that is bytes (Chebyshev distance along the path):
/// `0.30`.
#[cfg(test)]
const SPECTRUM_PACE_BYTE_SHARE: f64 = 0.30;

/// Samples per path slot for the pacing integral: `64` gives about 32 600
/// samples; doubling it moves entry positions by under a thousandth of a slot.
#[cfg(test)]
const SPECTRUM_PACE_FINE: usize = 64;

/// The most of the table the green→blue crossing may have: `167` of `510`
/// steps (uncapped, its long route through `#00FFFF` takes 42 %).
#[cfg(test)]
const SPECTRUM_CROSSING_SHARE_CAP: f64 =
    (SPECTRUM_CROSSING_ENTRY_CAP as f64 + 0.5) / (SPECTRUM_LUT_LEN - 1) as f64;

/// [`SPECTRUM_CROSSING_SHARE_CAP`] in the unit it resolves to: table entries.
#[cfg(test)]
const SPECTRUM_CROSSING_ENTRY_CAP: usize = 167;

/// Where the seven names landed, in table indices.
pub const SPECTRUM_ANCHOR_AT: [usize; SPECTRUM_STOPS] = [0, 58, 130, 237, 404, 474, 510];

/// Cubic smoothstep `0..1`, clamping: the segment's eased coordinate. It gives
/// the arc zero hue slope at every anchor, so the reflected sweep is C¹ at both
/// turnarounds.
#[inline]
#[cfg(test)]
const fn smoothstep01(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// HSV of a `0x00RRGGBB`: hue in degrees, saturation and value in `0..1`.
#[must_use]
#[inline]
pub const fn spectrum_hsv(rgb: u32) -> (f64, f64, f64) {
    let r = ((rgb >> 16) & 0xff) as f64 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f64 / 255.0;
    let b = (rgb & 0xff) as f64 / 255.0;
    let hi = r.max(g).max(b);
    let d = hi - r.min(g).min(b);
    let hue = if d <= 0.0 {
        0.0
    } else if hi == r {
        let q = (g - b) / d;
        60.0 * if q < 0.0 { q + 6.0 } else { q }
    } else if hi == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (hue, if hi <= 0.0 { 0.0 } else { d / hi }, hi)
}

/// HSV → unquantized sRGB `0..1`, hue in degrees; the generator rounds once,
/// when it commits an entry.
#[cfg(test)]
const fn hsv_srgb(hue_deg: f64, s: f64, v: f64) -> [f64; 3] {
    let rem = hue_deg % 360.0;
    let h = (if rem < 0.0 { rem + 360.0 } else { rem }) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// The path at any real position `x` in path slots, `0 ..= 510` (`85 * i` is
/// anchor `i`): the arc as drawn, before the pace spends the table on it.
#[cfg(test)]
const fn spectrum_path(x: f64) -> [f64; 3] {
    const fn chan(c: u32, sh: u32) -> f64 {
        ((c >> sh) & 0xff) as f64 / 255.0
    }
    let x = x.clamp(0.0, (SPECTRUM_LUT_LEN - 1) as f64);
    let seg_raw = (x / SPECTRUM_STRIDE as f64).floor() as usize;
    let seg = if seg_raw < SPECTRUM_STOPS - 2 {
        seg_raw
    } else {
        SPECTRUM_STOPS - 2
    };
    let slot = x - (seg * SPECTRUM_STRIDE) as f64;
    let k = smoothstep01(slot / SPECTRUM_STRIDE as f64);
    if seg == SPECTRUM_CROSSING_SEG {
        let (h0, s0, v0) = spectrum_hsv(SPECTRUM_ANCHORS[seg]);
        let (h1, s1, v1) = spectrum_hsv(SPECTRUM_ANCHORS[seg + 1]);
        return hsv_srgb(h0 + (h1 - h0) * k, s0 + (s1 - s0) * k, v0 + (v1 - v0) * k);
    }
    let (a, b) = (SPECTRUM_ANCHORS[seg], SPECTRUM_ANCHORS[seg + 1]);
    [
        chan(a, 16) + (chan(b, 16) - chan(a, 16)) * k,
        chan(a, 8) + (chan(b, 8) - chan(a, 8)) * k,
        chan(a, 0) + (chan(b, 0) - chan(a, 0)) * k,
    ]
}

/// The path colour as the byte triple an entry commits (the one rounding).
#[cfg(test)]
const fn spectrum_path_byte(x: f64) -> u32 {
    const fn byte(v: f64) -> u32 {
        (v.clamp(0.0, 1.0) * 255.0).round() as u32
    }
    let c = spectrum_path(x);
    (byte(c[0]) << 16) | (byte(c[1]) << 8) | byte(c[2])
}

/// OKLab of an unquantized sRGB triple (Björn Ottosson's matrices): the uniform
/// space the pace measures perceived step length in.
#[cfg(test)]
fn spectrum_oklab(c: [f64; 3]) -> [f64; 3] {
    let lin = |v: f64| {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(c[0]), lin(c[1]), lin(c[2]));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

/// The HSV hue of an unquantized triple, in degrees — [`spectrum_hsv`]'s hue,
/// asked of the path rather than of a committed byte.
#[cfg(test)]
const fn spectrum_path_hue(c: [f64; 3]) -> f64 {
    let (r, g, b) = (c[0], c[1], c[2]);
    let hi = r.max(g).max(b);
    let d = hi - r.min(g).min(b);
    if d <= 0.0 {
        return 0.0;
    }
    if hi == r {
        let q = (g - b) / d;
        60.0 * if q < 0.0 { q + 6.0 } else { q }
    } else if hi == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    }
}

/// The generator: builds [`SPECTRUM_LUT`] and [`SPECTRUM_ANCHOR_AT`] by resampling.
#[must_use]
#[cfg(test)]
pub fn generate_spectrum_lut() -> [u32; SPECTRUM_LUT_LEN] {
    generate_spectrum_table().lut
}

/// The generator's full result.
#[cfg(test)]
pub struct SpectrumTable {
    /// The committed [`SPECTRUM_LUT`].
    pub lut: [u32; SPECTRUM_LUT_LEN],
    /// The committed [`SPECTRUM_ANCHOR_AT`].
    pub anchor_at: [usize; SPECTRUM_STOPS],
    /// Each entry's position on the path, in path slots.
    pub path_at: [f32; SPECTRUM_LUT_LEN],
}

/// The full generator result (see [`SpectrumTable`]).
#[must_use]
#[cfg(test)]
pub fn generate_spectrum_table() -> SpectrumTable {
    spectrum_table_at_cap(SPECTRUM_CROSSING_SHARE_CAP)
}

/// [`generate_spectrum_table`] at a given crossing share cap.
#[cfg(test)]
pub(crate) fn spectrum_table_at_cap(cap: f64) -> SpectrumTable {
    const LEGS: usize = SPECTRUM_STOPS - 1;
    let mut bounds = [0.0f64; LEGS + 1];
    let mut bi = 0;
    while bi < bounds.len() {
        bounds[bi] = (bi * SPECTRUM_STRIDE) as f64;
        bi += 1;
    }

    let mut walk: Vec<Vec<(f64, [f64; 3])>> = Vec::with_capacity(LEGS);
    let mut totals = [0.0f64; 3];
    let mut leg = 0;
    while leg < LEGS {
        let (a, b) = (bounds[leg], bounds[leg + 1]);
        let steps = ((b - a) * SPECTRUM_PACE_FINE as f64) as usize;
        let mut cum = Vec::with_capacity(steps + 1);
        let mut acc = [0.0f64; 3];
        let mut prev = spectrum_path(a);
        cum.push((a, acc));
        let mut i = 1;
        while i <= steps {
            let x = a + (b - a) * i as f64 / steps as f64;
            let cur = spectrum_path(x);
            let (pl, cl) = (spectrum_oklab(prev), spectrum_oklab(cur));
            acc[0] += ((pl[0] - cl[0]).powi(2) + (pl[1] - cl[1]).powi(2) + (pl[2] - cl[2]).powi(2))
                .sqrt();
            acc[1] += {
                let d = spectrum_path_hue(cur) - spectrum_path_hue(prev);
                (d - 360.0 * (d / 180.0).trunc()).abs()
            };
            acc[2] += (prev[0] - cur[0])
                .abs()
                .max((prev[1] - cur[1]).abs())
                .max((prev[2] - cur[2]).abs());
            cum.push((x, acc));
            prev = cur;
            i += 1;
        }
        totals[0] += acc[0];
        totals[1] += acc[1];
        totals[2] += acc[2];
        walk.push(cum);
        leg += 1;
    }
    let cost = |c: [f64; 3]| {
        (1.0 - SPECTRUM_PACE_BYTE_SHARE)
            * ((1.0 - SPECTRUM_PACE_HUE_SHARE) * c[0] / totals[0]
                + SPECTRUM_PACE_HUE_SHARE * c[1] / totals[1])
            + SPECTRUM_PACE_BYTE_SHARE * c[2] / totals[2]
    };

    let mut budget = [0usize; LEGS];
    let mut want = [0.0f64; LEGS];
    let mut legcost = [0.0f64; LEGS];
    let mut free = 0.0f64;
    let mut leg = 0;
    while leg < LEGS {
        let samples = &walk[leg];
        let c = cost(samples[samples.len() - 1].1);
        legcost[leg] = c;
        free += c;
        leg += 1;
    }
    let purse = SPECTRUM_LUT_LEN - 1;
    let mut leg = 0;
    while leg < LEGS {
        want[leg] = legcost[leg] / free * purse as f64;
        leg += 1;
    }

    let ceiling = (cap * purse as f64).floor();
    let capped = want[SPECTRUM_CROSSING_SEG] > ceiling;
    if capped {
        let surplus = want[SPECTRUM_CROSSING_SEG] - ceiling;
        want[SPECTRUM_CROSSING_SEG] = ceiling;
        let mut rest = 0.0f64;
        let mut leg = 0;
        while leg < LEGS {
            if leg != SPECTRUM_CROSSING_SEG {
                rest += legcost[leg];
            }
            leg += 1;
        }
        let mut leg = 0;
        while leg < LEGS {
            if leg != SPECTRUM_CROSSING_SEG {
                want[leg] += surplus * legcost[leg] / rest;
            }
            leg += 1;
        }
    }

    let mut budget_sum = 0usize;
    let mut order = [0usize; LEGS];
    let mut order_len = 0usize;
    let mut leg = 0;
    while leg < LEGS {
        let b = want[leg].floor() as usize;
        budget[leg] = b;
        budget_sum += b;
        if !(capped && leg == SPECTRUM_CROSSING_SEG) {
            order[order_len] = leg;
            order_len += 1;
        }
        leg += 1;
    }
    order[..order_len].sort_by(|&a, &b| {
        (want[b] - budget[b] as f64)
            .partial_cmp(&(want[a] - budget[a] as f64))
            .expect("the pace is finite")
    });
    let mut short = SPECTRUM_LUT_LEN - 1 - budget_sum;
    let mut oi = 0;
    while oi < order_len {
        if short == 0 {
            break;
        }
        budget[order[oi]] += 1;
        short -= 1;
        oi += 1;
    }
    assert_eq!(short, 0, "the pace could not spend the table");

    let mut lut = [0u32; SPECTRUM_LUT_LEN];
    let mut path_at = [0.0f32; SPECTRUM_LUT_LEN];
    let mut anchor_at = [0usize; SPECTRUM_STOPS];
    let mut idx = 0usize;
    let mut leg = 0;
    while leg < LEGS {
        let start = bounds[leg];
        if (start as usize).is_multiple_of(SPECTRUM_STRIDE) && (start as usize) < SPECTRUM_LUT_LEN {
            anchor_at[start as usize / SPECTRUM_STRIDE] = idx;
        }
        let samples = &walk[leg];
        let total = cost(samples[samples.len() - 1].1);
        let mut j = 0;
        while j < budget[leg] {
            let x = spectrum_pace_at(samples, &cost, total * j as f64 / budget[leg] as f64);
            lut[idx + j] = spectrum_path_byte(x);
            path_at[idx + j] = x as f32;
            j += 1;
        }
        idx += budget[leg];
        leg += 1;
    }
    anchor_at[SPECTRUM_STOPS - 1] = SPECTRUM_LUT_LEN - 1;
    path_at[SPECTRUM_LUT_LEN - 1] = (SPECTRUM_LUT_LEN - 1) as f32;
    let mut ai = 0;
    while ai < anchor_at.len() {
        let at = anchor_at[ai];
        lut[at] = SPECTRUM_ANCHORS[ai];
        path_at[at] = (ai * SPECTRUM_STRIDE) as f32;
        ai += 1;
    }
    SpectrumTable {
        lut,
        anchor_at,
        path_at,
    }
}

/// Where the green→blue crossing sits in `t`.
#[cfg(test)]
#[must_use]
pub(crate) const fn spectrum_crossing_position() -> f32 {
    let mid = 0.5
        * (spectrum_hsv(SPECTRUM_ANCHORS[SPECTRUM_CROSSING_SEG]).0
            + spectrum_hsv(SPECTRUM_ANCHORS[SPECTRUM_CROSSING_SEG + 1]).0);
    let (mut best, mut near) = (0usize, f64::MAX);
    let mut i = 0;
    while i < SPECTRUM_LUT.len() {
        let d = (spectrum_hsv(SPECTRUM_LUT[i]).0 - mid).abs();
        if d < near {
            near = d;
            best = i;
        }
        i += 1;
    }
    best as f32 * (1.0 / (SPECTRUM_LUT_LEN - 1) as f32)
}

/// Width of the green→blue leg in `t`.
#[cfg(test)]
#[must_use]
pub(crate) const fn spectrum_crossing_span() -> f32 {
    (SPECTRUM_ANCHOR_AT[SPECTRUM_CROSSING_SEG + 1] - SPECTRUM_ANCHOR_AT[SPECTRUM_CROSSING_SEG])
        as f32
        * (1.0 / (SPECTRUM_LUT_LEN - 1) as f32)
}

/// Where along a leg the pace has spent `want`: the inverse of the cumulative
/// cost, by binary search and one linear step inside the bracket.
#[cfg(test)]
fn spectrum_pace_at(cum: &[(f64, [f64; 3])], cost: &impl Fn([f64; 3]) -> f64, want: f64) -> f64 {
    if want <= 0.0 {
        return cum[0].0;
    }
    let (mut lo, mut hi) = (0usize, cum.len() - 1);
    if want >= cost(cum[hi].1) {
        return cum[hi].0;
    }
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if cost(cum[mid].1) <= want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (c0, c1) = (cost(cum[lo].1), cost(cum[hi].1));
    if c1 <= c0 {
        return cum[lo].0;
    }
    cum[lo].0 + (cum[hi].0 - cum[lo].0) * (want - c0) / (c1 - c0)
}

/// The one spectrum at position `t`.
#[inline]
#[must_use]
#[allow(
    clippy::manual_clamp,
    reason = "`clamp` PROPAGATES NaN and this fold must not: a NaN index falls \
              through the lerp to a black quad — a silent hole in a mark, at no \
              cost to any test. `f32::max` returns the other operand when one is \
              NaN, so this spelling is total where `clamp` is not."
)]
pub const fn spectrum(t: f32) -> u32 {
    let x = t.max(0.0).min(1.0) * (SPECTRUM_LUT_LEN - 1) as f32;
    let xi = x as usize;
    if xi >= SPECTRUM_LUT_LEN - 1 {
        return SPECTRUM_LUT[SPECTRUM_LUT_LEN - 1];
    }
    lerp_rgb(SPECTRUM_LUT[xi], SPECTRUM_LUT[xi + 1], x - xi as f32)
}

/// A producer's chroma floor.
#[must_use]
#[inline]
pub const fn spectrum_with_min_saturation(rgb: u32, s_min: f32) -> u32 {
    if !s_min.is_finite() || s_min <= 0.0 {
        return rgb;
    }
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let rg_max = if r > g { r } else { g };
    let mx = if rg_max > b { rg_max } else { b };
    let rg_min = if r < g { r } else { g };
    let mn = if rg_min < b { rg_min } else { b };
    if mx == 0 || mx == mn {
        return rgb;
    }
    let s_min = s_min.clamp(0.0, 1.0);
    let spread = (mx - mn) as f32;
    if spread >= s_min * mx as f32 {
        return rgb;
    }
    let k = s_min * mx as f32 / spread;
    #[inline]
    const fn ch(mx: u32, c: u32, k: f32) -> u32 {
        (mx as f32 - (mx - c) as f32 * k + 0.5).clamp(0.0, 255.0) as u32
    }
    (ch(mx, r, k) << 16) | (ch(mx, g, k) << 8) | ch(mx, b, k)
}

/// The pixel the emitter writes for one glow quad over one ground.
#[inline]
#[must_use]
pub const fn compose_on_glass(ground: u32, premul: u32, alpha: u8) -> u32 {
    if alpha == 0 {
        aterm_render::add_sat(ground, premul)
    } else {
        aterm_render::over_premul(ground, premul, alpha)
    }
}

/// Where named stop `i` sits in `t` (out-of-range indices clamp).
#[inline]
#[must_use]
pub(crate) const fn spectrum_stop_position(i: usize) -> f32 {
    let idx = if i < SPECTRUM_STOPS - 1 {
        i
    } else {
        SPECTRUM_STOPS - 1
    };
    SPECTRUM_ANCHOR_AT[idx] as f32 * (1.0 / (SPECTRUM_LUT_LEN - 1) as f32)
}

/// The colour of named stop `i` (red, orange, yellow, green, blue, indigo, violet).
#[inline]
#[must_use]
pub const fn spectrum_stop(i: usize) -> u32 {
    let idx = if i < SPECTRUM_STOPS - 1 {
        i
    } else {
        SPECTRUM_STOPS - 1
    };
    SPECTRUM_ANCHORS[idx]
}

/// Midpoints between consecutive [`SPECTRUM_ANCHOR_AT`] entries. Because the
/// anchor table is strictly increasing, `x > SPECTRUM_SNAP_MIDS[k]` is
/// identical to `(x - at[k + 1]).abs() < (x - at[k]).abs()`, and ties (`x == mid`)
/// stay with the lower stop `k` just like a strict `<` scan.
const SPECTRUM_SNAP_MIDS: [f32; SPECTRUM_STOPS - 1] = {
    let mut mids = [0.0f32; SPECTRUM_STOPS - 1];
    let mut k = 0;
    while k < SPECTRUM_STOPS - 1 {
        mids[k] = 0.5 * (SPECTRUM_ANCHOR_AT[k] + SPECTRUM_ANCHOR_AT[k + 1]) as f32;
        k += 1;
    }
    mids
};

/// The index of the named stop nearest `t` ([`SPECTRUM_ANCHOR_AT`]).
#[inline]
#[must_use]
pub const fn spectrum_snap_index(t: f32) -> usize {
    let x = t.clamp(0.0, 1.0) * (SPECTRUM_LUT_LEN - 1) as f32;
    (x > SPECTRUM_SNAP_MIDS[0]) as usize
        + (x > SPECTRUM_SNAP_MIDS[1]) as usize
        + (x > SPECTRUM_SNAP_MIDS[2]) as usize
        + (x > SPECTRUM_SNAP_MIDS[3]) as usize
        + (x > SPECTRUM_SNAP_MIDS[4]) as usize
        + (x > SPECTRUM_SNAP_MIDS[5]) as usize
}

/// Snap to a name: the nearest named stop's colour to `t`.
#[inline]
#[must_use]
pub const fn spectrum_snap(t: f32) -> u32 {
    spectrum_stop(spectrum_snap_index(t))
}

/// The arc's steepest byte rate.
#[must_use]
#[cfg(test)]
pub(crate) const fn spectrum_max_byte_rate() -> f32 {
    let mut worst = 0u32;
    let mut i = 0;
    while i + 1 < SPECTRUM_LUT.len() {
        let a = SPECTRUM_LUT[i];
        let b = SPECTRUM_LUT[i + 1];
        let dr = ((a >> 16) & 0xff).abs_diff((b >> 16) & 0xff);
        let dg = ((a >> 8) & 0xff).abs_diff((b >> 8) & 0xff);
        let db = (a & 0xff).abs_diff(b & 0xff);
        if dr > worst {
            worst = dr;
        }
        if dg > worst {
            worst = dg;
        }
        if db > worst {
            worst = db;
        }
        i += 1;
    }
    worst as f32 * (SPECTRUM_LUT_LEN - 1) as f32
}

/// The one spectrum, resolved.
#[rustfmt::skip]
pub const SPECTRUM_LUT: [u32; SPECTRUM_LUT_LEN] = [
    0x00FF_0000, 0x00FF_0300, 0x00FF_0500, 0x00FF_0800, 0x00FF_0B00, 0x00FF_0D00,
    0x00FF_1000, 0x00FF_1300, 0x00FF_1500, 0x00FF_1800, 0x00FF_1A00, 0x00FF_1D00,
    0x00FF_1F00, 0x00FF_2200, 0x00FF_2400, 0x00FF_2600, 0x00FF_2900, 0x00FF_2B00,
    0x00FF_2D00, 0x00FF_3000, 0x00FF_3200, 0x00FF_3400, 0x00FF_3700, 0x00FF_3900,
    0x00FF_3B00, 0x00FF_3D00, 0x00FF_3F00, 0x00FF_4200, 0x00FF_4400, 0x00FF_4600,
    0x00FF_4800, 0x00FF_4A00, 0x00FF_4C00, 0x00FF_4E00, 0x00FF_5000, 0x00FF_5200,
    0x00FF_5400, 0x00FF_5600, 0x00FF_5800, 0x00FF_5A00, 0x00FF_5C00, 0x00FF_5E00,
    0x00FF_6000, 0x00FF_6200, 0x00FF_6400, 0x00FF_6600, 0x00FF_6800, 0x00FF_6A00,
    0x00FF_6C00, 0x00FF_6E00, 0x00FF_7000, 0x00FF_7200, 0x00FF_7400, 0x00FF_7600,
    0x00FF_7800, 0x00FF_7900, 0x00FF_7B00, 0x00FF_7D00, 0x00FF_7F00, 0x00FF_8100,
    0x00FF_8300, 0x00FF_8500, 0x00FF_8600, 0x00FF_8800, 0x00FF_8A00, 0x00FF_8C00,
    0x00FF_8E00, 0x00FF_9000, 0x00FF_9100, 0x00FF_9300, 0x00FF_9500, 0x00FF_9700,
    0x00FF_9900, 0x00FF_9A00, 0x00FF_9C00, 0x00FF_9E00, 0x00FF_A000, 0x00FF_A200,
    0x00FF_A300, 0x00FF_A500, 0x00FF_A700, 0x00FF_A900, 0x00FF_AB00, 0x00FF_AC00,
    0x00FF_AE00, 0x00FF_B000, 0x00FF_B200, 0x00FF_B300, 0x00FF_B500, 0x00FF_B700,
    0x00FF_B900, 0x00FF_BB00, 0x00FF_BC00, 0x00FF_BE00, 0x00FF_C000, 0x00FF_C200,
    0x00FF_C300, 0x00FF_C500, 0x00FF_C700, 0x00FF_C900, 0x00FF_CA00, 0x00FF_CC00,
    0x00FF_CE00, 0x00FF_D000, 0x00FF_D100, 0x00FF_D300, 0x00FF_D500, 0x00FF_D700,
    0x00FF_D800, 0x00FF_DA00, 0x00FF_DC00, 0x00FF_DE00, 0x00FF_DF00, 0x00FF_E100,
    0x00FF_E300, 0x00FF_E500, 0x00FF_E600, 0x00FF_E800, 0x00FF_EA00, 0x00FF_EC00,
    0x00FF_ED00, 0x00FF_EF00, 0x00FF_F100, 0x00FF_F300, 0x00FF_F400, 0x00FF_F600,
    0x00FF_F800, 0x00FF_FA00, 0x00FF_FB00, 0x00FF_FD00, 0x00FF_FF00, 0x00FD_FF00,
    0x00FB_FF00, 0x00F9_FF00, 0x00F7_FF00, 0x00F4_FF00, 0x00F2_FF00, 0x00F0_FF00,
    0x00EE_FF00, 0x00EC_FF00, 0x00EA_FF00, 0x00E8_FF00, 0x00E5_FF00, 0x00E3_FF00,
    0x00E1_FF00, 0x00DF_FF00, 0x00DD_FF00, 0x00DB_FF00, 0x00D9_FF00, 0x00D6_FF00,
    0x00D4_FF00, 0x00D2_FF00, 0x00D0_FF00, 0x00CE_FF00, 0x00CC_FF00, 0x00C9_FF00,
    0x00C7_FF00, 0x00C5_FF00, 0x00C3_FF00, 0x00C1_FF00, 0x00BE_FF00, 0x00BC_FF00,
    0x00BA_FF00, 0x00B8_FF00, 0x00B6_FF00, 0x00B3_FF00, 0x00B1_FF00, 0x00AF_FF00,
    0x00AD_FF00, 0x00AA_FF00, 0x00A8_FF00, 0x00A6_FF00, 0x00A4_FF00, 0x00A1_FF00,
    0x009F_FF00, 0x009D_FF00, 0x009A_FF00, 0x0098_FF00, 0x0096_FF00, 0x0094_FF00,
    0x0091_FF00, 0x008F_FF00, 0x008D_FF00, 0x008A_FF00, 0x0088_FF00, 0x0086_FF00,
    0x0083_FF00, 0x0081_FF00, 0x007F_FF00, 0x007C_FF00, 0x007A_FF00, 0x0077_FF00,
    0x0075_FF00, 0x0073_FF00, 0x0070_FF00, 0x006E_FF00, 0x006B_FF00, 0x0069_FF00,
    0x0067_FF00, 0x0064_FF00, 0x0062_FF00, 0x005F_FF00, 0x005D_FF00, 0x005A_FF00,
    0x0058_FF00, 0x0055_FF00, 0x0053_FF00, 0x0050_FF00, 0x004E_FF00, 0x004B_FF00,
    0x0049_FF00, 0x0046_FF00, 0x0043_FF00, 0x0041_FF00, 0x003E_FF00, 0x003C_FF00,
    0x0039_FF00, 0x0036_FF00, 0x0034_FF00, 0x0031_FF00, 0x002E_FF00, 0x002C_FF00,
    0x0029_FF00, 0x0026_FF00, 0x0024_FF00, 0x0021_FF00, 0x001E_FF00, 0x001C_FF00,
    0x0019_FF00, 0x0016_FF00, 0x0013_FF00, 0x0011_FF00, 0x000E_FF00, 0x000B_FF00,
    0x0008_FF00, 0x0006_FF00, 0x0003_FF00, 0x0000_FF00, 0x0000_FF04, 0x0000_FF08,
    0x0000_FF0C, 0x0000_FF10, 0x0000_FF14, 0x0000_FF18, 0x0000_FF1C, 0x0000_FF20,
    0x0000_FF24, 0x0000_FF28, 0x0000_FF2C, 0x0000_FF30, 0x0000_FF34, 0x0000_FF38,
    0x0000_FF3B, 0x0000_FF3F, 0x0000_FF43, 0x0000_FF46, 0x0000_FF4A, 0x0000_FF4E,
    0x0000_FF51, 0x0000_FF55, 0x0000_FF58, 0x0000_FF5C, 0x0000_FF5F, 0x0000_FF63,
    0x0000_FF66, 0x0000_FF6A, 0x0000_FF6D, 0x0000_FF71, 0x0000_FF74, 0x0000_FF77,
    0x0000_FF7B, 0x0000_FF7E, 0x0000_FF81, 0x0000_FF85, 0x0000_FF88, 0x0000_FF8B,
    0x0000_FF8F, 0x0000_FF92, 0x0000_FF95, 0x0000_FF98, 0x0000_FF9C, 0x0000_FF9F,
    0x0000_FFA2, 0x0000_FFA5, 0x0000_FFA9, 0x0000_FFAC, 0x0000_FFAF, 0x0000_FFB2,
    0x0000_FFB5, 0x0000_FFB9, 0x0000_FFBC, 0x0000_FFBF, 0x0000_FFC2, 0x0000_FFC5,
    0x0000_FFC8, 0x0000_FFCC, 0x0000_FFCF, 0x0000_FFD2, 0x0000_FFD5, 0x0000_FFD8,
    0x0000_FFDB, 0x0000_FFDF, 0x0000_FFE2, 0x0000_FFE5, 0x0000_FFE8, 0x0000_FFEB,
    0x0000_FFEE, 0x0000_FFF1, 0x0000_FFF4, 0x0000_FFF8, 0x0000_FFFB, 0x0000_FFFE,
    0x0000_FDFF, 0x0000_FBFF, 0x0000_F8FF, 0x0000_F6FF, 0x0000_F4FF, 0x0000_F1FF,
    0x0000_EFFF, 0x0000_ECFF, 0x0000_EAFF, 0x0000_E7FF, 0x0000_E5FF, 0x0000_E2FF,
    0x0000_E0FF, 0x0000_DDFF, 0x0000_DBFF, 0x0000_D8FF, 0x0000_D6FF, 0x0000_D3FF,
    0x0000_D1FF, 0x0000_CEFF, 0x0000_CCFF, 0x0000_C9FF, 0x0000_C7FF, 0x0000_C4FF,
    0x0000_C2FF, 0x0000_BFFF, 0x0000_BDFF, 0x0000_BAFF, 0x0000_B8FF, 0x0000_B5FF,
    0x0000_B3FF, 0x0000_B0FF, 0x0000_AEFF, 0x0000_ABFF, 0x0000_A9FF, 0x0000_A6FF,
    0x0000_A4FF, 0x0000_A1FF, 0x0000_9FFF, 0x0000_9CFF, 0x0000_9AFF, 0x0000_97FF,
    0x0000_95FF, 0x0000_92FF, 0x0000_90FF, 0x0000_8DFF, 0x0000_8AFF, 0x0000_88FF,
    0x0000_85FF, 0x0000_83FF, 0x0000_80FF, 0x0000_7EFF, 0x0000_7BFF, 0x0000_78FF,
    0x0000_76FF, 0x0000_73FF, 0x0000_70FF, 0x0000_6EFF, 0x0000_6BFF, 0x0000_69FF,
    0x0000_66FF, 0x0000_63FF, 0x0000_60FF, 0x0000_5EFF, 0x0000_5BFF, 0x0000_58FF,
    0x0000_55FF, 0x0000_53FF, 0x0000_50FF, 0x0000_4DFF, 0x0000_4AFF, 0x0000_47FF,
    0x0000_44FF, 0x0000_41FF, 0x0000_3EFF, 0x0000_3BFF, 0x0000_38FF, 0x0000_35FF,
    0x0000_32FF, 0x0000_2FFF, 0x0000_2BFF, 0x0000_28FF, 0x0000_25FF, 0x0000_21FF,
    0x0000_1EFF, 0x0000_1AFF, 0x0000_17FF, 0x0000_13FF, 0x0000_0FFF, 0x0000_0CFF,
    0x0000_08FF, 0x0000_04FF, 0x0000_00FF, 0x0001_00FD, 0x0003_00FA, 0x0004_00F8,
    0x0006_00F5, 0x0007_00F3, 0x0009_00F1, 0x000A_00EE, 0x000B_00EC, 0x000D_00EA,
    0x000E_00E8, 0x000F_00E5, 0x0011_00E3, 0x0012_00E1, 0x0013_00DF, 0x0015_00DC,
    0x0016_00DA, 0x0017_00D8, 0x0019_00D6, 0x001A_00D4, 0x001B_00D2, 0x001C_00D0,
    0x001E_00CE, 0x001F_00CC, 0x0020_00CA, 0x0021_00C8, 0x0022_00C6, 0x0023_00C4,
    0x0025_00C2, 0x0026_00C0, 0x0027_00BE, 0x0028_00BC, 0x0029_00BB, 0x002A_00B9,
    0x002B_00B7, 0x002C_00B5, 0x002D_00B3, 0x002E_00B2, 0x002F_00B0, 0x0030_00AE,
    0x0031_00AD, 0x0032_00AB, 0x0033_00A9, 0x0034_00A8, 0x0035_00A6, 0x0036_00A4,
    0x0037_00A3, 0x0038_00A1, 0x0039_00A0, 0x003A_009E, 0x003B_009D, 0x003C_009B,
    0x003D_009A, 0x003E_0098, 0x003E_0097, 0x003F_0096, 0x0040_0094, 0x0041_0093,
    0x0042_0091, 0x0043_0090, 0x0043_008F, 0x0044_008D, 0x0045_008C, 0x0046_008B,
    0x0047_0089, 0x0047_0088, 0x0048_0087, 0x0049_0086, 0x004A_0084, 0x004A_0083,
    0x004B_0082, 0x004D_0084, 0x004F_0086, 0x0050_0088, 0x0052_008A, 0x0054_008C,
    0x0056_008E, 0x0058_0090, 0x005A_0092, 0x005C_0094, 0x005E_0097, 0x005F_0099,
    0x0061_009B, 0x0063_009D, 0x0065_009F, 0x0067_00A1, 0x0069_00A4, 0x006B_00A6,
    0x006D_00A8, 0x006F_00AA, 0x0072_00AD, 0x0074_00AF, 0x0076_00B1, 0x0078_00B4,
    0x007A_00B6, 0x007C_00B8, 0x007E_00BB, 0x0080_00BD, 0x0082_00C0, 0x0085_00C2,
    0x0087_00C4, 0x0089_00C7, 0x008B_00C9, 0x008D_00CC, 0x0090_00CE, 0x0092_00D1,
    0x0094_00D3,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Arc samples for "holds everywhere" checks: about eight per LUT interval.
    const WALK: usize = 4096;

    /// The committed tables as the generator writes them, ready to paste.
    fn regenerated_tables() -> String {
        let table = generate_spectrum_table();
        let (lut, anchor_at) = (table.lut, table.anchor_at);
        let mut out =
            format!("pub const SPECTRUM_ANCHOR_AT: [usize; SPECTRUM_STOPS] = {anchor_at:?};\n");
        for row in lut.chunks(6) {
            let cells: Vec<String> = row
                .iter()
                .map(|c| format!("0x{:04X}_{:04X},", c >> 16, c & 0xffff))
                .collect();
            out.push_str(&format!("    {}\n", cells.join(" ")));
        }
        out
    }

    /// **THE COMMITTED TABLE IS THE LAW'S OWN OUTPUT.** Byte-for-byte, no
    /// tolerance: the moment an anchor or interpolation rule moves, this fails and
    /// the table has to be regenerated rather than nudged — the failure prints the
    /// regenerated tables to paste.
    #[test]
    fn spectrum_lut_is_byte_reproducible_from_its_generator() {
        let fresh = generate_spectrum_lut();
        assert!(
            fresh == SPECTRUM_LUT && generate_spectrum_table().anchor_at == SPECTRUM_ANCHOR_AT,
            "SPECTRUM_LUT / SPECTRUM_ANCHOR_AT drifted from the generator; paste:\n{}",
            regenerated_tables()
        );
    }

    /// The anchors are stored bit for bit at their generated indices.
    #[test]
    fn spectrum_reproduces_its_anchors_exactly() {
        let anchor_at = generate_spectrum_table().anchor_at;
        assert_eq!(
            anchor_at, SPECTRUM_ANCHOR_AT,
            "SPECTRUM_ANCHOR_AT drifted from the pace that places the anchors"
        );
        assert_eq!(SPECTRUM_ANCHOR_AT[0], 0, "red is the arc's own start");
        assert_eq!(
            SPECTRUM_ANCHOR_AT[SPECTRUM_STOPS - 1],
            SPECTRUM_LUT_LEN - 1,
            "violet is the arc's own end"
        );
        assert!(
            SPECTRUM_ANCHOR_AT.windows(2).all(|w| w[0] < w[1]),
            "the seven names must land in ROYGBIV order: {SPECTRUM_ANCHOR_AT:?}"
        );
        for (i, &anchor) in SPECTRUM_ANCHORS.iter().enumerate() {
            assert_eq!(
                SPECTRUM_LUT[SPECTRUM_ANCHOR_AT[i]], anchor,
                "table index {} is not anchor {i}",
                SPECTRUM_ANCHOR_AT[i]
            );
            assert_eq!(spectrum_stop(i), anchor, "stop {i}");
            assert_eq!(
                spectrum(spectrum_stop_position(i)),
                anchor,
                "the open arc misses anchor {i} at its own position"
            );
        }
    }

    /// HSV hue climbs from red to violet and never reverses (a reversal shows a
    /// colour twice on one mark).
    #[test]
    fn spectrum_hue_climbs_from_red_to_violet_without_reversing() {
        let mut prev = f64::NEG_INFINITY;
        let mut worst_drop = 0.0f64;
        for (i, &entry) in SPECTRUM_LUT.iter().enumerate() {
            let hue = spectrum_hsv(entry).0;
            let chan = |sh: u32| f64::from((entry >> sh) & 0xff);
            let spread = chan(16).max(chan(8)).max(chan(0)) - chan(16).min(chan(8)).min(chan(0));
            let rotation = if spread > 0.0 { 60.0 / spread } else { 360.0 };
            if hue < prev {
                worst_drop = worst_drop.max(prev - hue);
            }
            assert!(
                hue >= prev - rotation,
                "hue reverses at entry {i}: {prev:.3}° -> {hue:.3}° \
                 (byte rotation here is {rotation:.3}°)"
            );
            prev = hue;
        }
        assert!(
            worst_drop <= 90.0 / 255.0,
            "the table's worst hue reversal is {worst_drop:.3}°, past the \
             {:.3}° a rounded byte can account for",
            90.0 / 255.0
        );
        for i in 1..SPECTRUM_STOPS {
            assert!(
                spectrum_hsv(spectrum_stop(i)).0 > spectrum_hsv(spectrum_stop(i - 1)).0,
                "anchor {i} does not climb"
            );
        }
        assert_eq!(spectrum_hsv(SPECTRUM_LUT[0]).0, 0.0, "red is hue 0");
        assert_eq!(
            SPECTRUM_LUT[0], SPECTRUM_ANCHORS[0],
            "the table starts on the authored red"
        );
        assert_eq!(
            SPECTRUM_LUT[SPECTRUM_LUT_LEN - 1],
            SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1],
            "the table ends on the authored violet"
        );
    }

    /// The arc is vivid.
    #[test]
    fn spectrum_stays_saturated_across_the_whole_arc() {
        let mut min_sat = 1.0f64;
        let mut sum_sat = 0.0f64;
        let mut min_val = 1.0f64;
        for &rgb in SPECTRUM_LUT.iter() {
            let (_, s, v) = spectrum_hsv(rgb);
            min_sat = min_sat.min(s);
            sum_sat += s;
            min_val = min_val.min(v);
        }
        let mean_sat = sum_sat / SPECTRUM_LUT_LEN as f64;

        for (i, &rgb) in SPECTRUM_LUT.iter().enumerate() {
            let (_, s, _) = spectrum_hsv(rgb);
            assert!(
                s >= 0.999,
                "entry {i} desaturates to S {s:.4} — the palette lost its \
                 shared zero channel, or a chroma taper came back"
            );
        }
        assert!(min_sat >= 0.999, "the arc's floor fell to S {min_sat:.4}");
        assert!(mean_sat >= 0.999, "mean saturation is only {mean_sat:.4}");

        let anchor_min_v = SPECTRUM_ANCHORS
            .iter()
            .map(|&c| spectrum_hsv(c).2)
            .fold(f64::MAX, f64::min);
        assert!(
            (min_val - anchor_min_v).abs() < 1e-9,
            "the arc's darkest entry is V {min_val:.4}, but the darkest AUTHORED \
             anchor is V {anchor_min_v:.4} — the arc is darker between its stops \
             than at any of them"
        );

        let chroma = |c: u32| {
            let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            r.max(g).max(b) - r.min(g).min(b)
        };
        let min_chroma = SPECTRUM_LUT.iter().copied().map(chroma).min().unwrap();
        assert!(
            min_chroma >= 24,
            "a neutral hole reopened in the table: min chroma {min_chroma}"
        );
    }

    /// Adjacent entries in [`SPECTRUM_LUT`] stay close enough that the LUT prints
    /// no visible staircase and [`spectrum`] stays continuous between them.
    #[test]
    fn spectrum_lut_has_bounded_adjacent_steps() {
        const CEILING: i32 = 5;
        const {
            let mut i = 0;
            while i < SPECTRUM_LUT_LEN - 1 {
                let mut shift = 0u32;
                while shift <= 16 {
                    let a = ((SPECTRUM_LUT[i] >> shift) & 0xff) as i32;
                    let b = ((SPECTRUM_LUT[i + 1] >> shift) & 0xff) as i32;
                    assert!(
                        (a - b).abs() <= CEILING,
                        "SPECTRUM_LUT adjacent step exceeds CEILING"
                    );
                    shift += 8;
                }
                i += 1;
            }
        }
        let mut worst = 0i32;
        for i in 0..SPECTRUM_LUT_LEN - 1 {
            let step = [16u32, 8, 0]
                .into_iter()
                .map(|shift| {
                    let a = ((SPECTRUM_LUT[i] >> shift) & 0xff) as i32;
                    let b = ((SPECTRUM_LUT[i + 1] >> shift) & 0xff) as i32;
                    (a - b).abs()
                })
                .max()
                .unwrap_or(0);
            worst = worst.max(step);
        }
        assert!(worst > 0, "a table with no structure would pass vacuously");
        for i in 0..=WALK {
            let t = i as f32 / WALK as f32;
            let a = spectrum(t);
            let b = spectrum((t + 0.5 / (SPECTRUM_LUT_LEN - 1) as f32).min(1.0));
            for shift in [16u32, 8, 0] {
                let d = ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff) as i32;
                assert!(d <= worst, "the read steps {d} across half a table entry");
            }
        }
    }

    /// Point-marks snap to a name (§2.3.3): [`spectrum_snap`] resolves to one of
    /// exactly seven colours.
    #[test]
    fn spectrum_snap_resolves_to_one_of_the_seven_names() {
        let named: Vec<u32> = (0..SPECTRUM_STOPS).map(spectrum_stop).collect();
        assert_eq!(named, SPECTRUM_ANCHORS.to_vec());
        for i in 0..=WALK {
            let snapped = spectrum_snap(i as f32 / WALK as f32);
            assert!(
                named.contains(&snapped),
                "snap produced #{snapped:06X}, which is not a named stop"
            );
        }
        for (i, &stop) in named.iter().enumerate() {
            assert_eq!(
                spectrum_snap(spectrum_stop_position(i)),
                stop,
                "a stop does not snap to itself"
            );
            assert!(
                (0..=WALK).any(|k| spectrum_snap(k as f32 / WALK as f32) == stop),
                "stop {i} is unreachable by snapping"
            );
        }
    }

    /// Both ends clamp, NaN included: nothing walks violet back into red.
    #[test]
    fn spectrum_endpoints_are_the_named_red_and_violet() {
        assert_eq!(spectrum(0.0), SPECTRUM_ANCHORS[0]);
        assert_eq!(spectrum(1.0), SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1]);
        for t in [-1.0f32, -0.001, f32::NEG_INFINITY] {
            assert_eq!(spectrum(t), SPECTRUM_ANCHORS[0], "below the arc at {t}");
        }
        for t in [1.001f32, 4.0, f32::INFINITY] {
            assert_eq!(
                spectrum(t),
                SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1],
                "above the arc at {t}"
            );
        }
        assert_eq!(spectrum(f32::NAN), SPECTRUM_ANCHORS[0], "NaN draws red");
        assert!(spectrum_max_byte_rate() > 0.0);
    }
}

#[cfg(test)]
mod min_saturation {
    use super::*;

    fn hsv(rgb: u32) -> (f64, f64, f64) {
        spectrum_hsv(rgb)
    }

    /// Five desaturated witnesses around the hue circle reach the floor with
    /// hue and value untouched.
    #[test]
    fn the_floor_is_reached_with_hue_and_value_untouched() {
        for (i, &under) in [
            0x006F_EBC2u32,
            0x006F_B1EB,
            0x00C0_8080,
            0x0080_C0A0,
            0x009A_9AD0,
        ]
        .iter()
        .enumerate()
        {
            let (h0, s0, v0) = hsv(under);
            assert!(
                s0 < 0.60,
                "witness {i} is not under S 0.60 ({s0:.3}) — this pin needs a colour the floor can move"
            );
            for floor in [0.65f32, 0.80, 0.90, 1.0] {
                let got = spectrum_with_min_saturation(under, floor);
                let (h1, s1, v1) = hsv(got);
                assert!(
                    (h1 - h0).abs() < 1.0,
                    "witness {i} at floor {floor}: hue moved {h0:.1}° → {h1:.1}° (#{got:06X})"
                );
                assert_eq!(
                    (v1 * 255.0).round() as u32,
                    (v0 * 255.0).round() as u32,
                    "witness {i} at floor {floor}: value moved"
                );
                assert!(
                    (s1 - f64::from(floor)).abs() <= 1.5 / 255.0 * 2.0,
                    "witness {i} at floor {floor}: S landed at {s1:.3}, not on the floor"
                );
            }
        }
        assert_eq!(spectrum_with_min_saturation(0x006F_B1EB, 1.0), 0x0000_7DEB);
    }

    #[test]
    fn a_colour_at_or_over_the_floor_and_every_grey_is_returned_bit_for_bit() {
        for &anchor in &SPECTRUM_ANCHORS {
            assert_eq!(
                spectrum_with_min_saturation(anchor, 1.0),
                anchor,
                "every anchor has a zero channel"
            );
        }
        for grey in [0u32, 0x0080_8080, 0x00FF_FFFF, 0x0011_1111] {
            assert_eq!(
                spectrum_with_min_saturation(grey, 1.0),
                grey,
                "a grey has no hue to keep"
            );
        }
        assert_eq!(
            spectrum_with_min_saturation(0x006F_EBC2, 0.50),
            0x006F_EBC2,
            "S 0.53 is over a 0.50 floor"
        );
        assert_eq!(
            spectrum_with_min_saturation(0x006F_EBC2, f32::NAN),
            0x006F_EBC2,
            "a non-finite floor is no floor"
        );
        assert_eq!(spectrum_with_min_saturation(0x006F_EBC2, 0.0), 0x006F_EBC2);
    }
}
