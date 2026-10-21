// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared `0x00RRGGBB` color math: HSV round-trip, the sparkle-words v2 ink-pair
//! hue nudge (docs/sparkle-words-v2-design.md §4.2/§4.3/§6.4/§6.5), and WCAG
//! relative luminance. A leaf module, so its users need not depend on
//! `word_decorations.rs`.

/// Rotate a `0x00RRGGBB` color's hue by the §4.2 nudge code (`0..=15` →
/// `-18°..=+18°`), preserving saturation and value.
pub fn hue_nudge(rgb: u32, code: u8) -> u32 {
    let (h, s, v) = rgb2hsv(rgb);
    hsv2rgb(h + (-18.0 + f32::from(code) * 2.4), s, v)
}

/// The hues a light page cannot show cleanly, in turns: deep orange through
/// yellow to chartreuse. Deepened by lightness until it reads on paper
/// (an ink under the letters, a caret on the page), a hue in this band turns
/// brown, khaki or olive, so a spectrum laid on paper steps over it
/// ([`light_spectrum_hue`]).
pub const LIGHT_MUDDY_HUES: (f32, f32) = (36.0 / 360.0, 96.0 / 360.0);

/// `hue` (turns) as a spectrum laid on a light page shows it: the wheel
/// compressed so it steps over [`LIGHT_MUDDY_HUES`] (orange runs straight on
/// into green), every other hue kept in order. A dark page shows the whole
/// wheel; only the light arms call this.
#[must_use]
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

/// WCAG channel linearization times channel weight, per byte value. An exact
/// cache of the scalar expression (same rounded `f32` multiplies, same add
/// order); built at runtime because `powf` is not `const`.
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
pub(crate) fn relative_luminance_with(table: &RelativeLuminanceTable, rgb: u32) -> f32 {
    (table.red[((rgb >> 16) & 0xff) as usize] + table.green[((rgb >> 8) & 0xff) as usize])
        + table.blue[(rgb & 0xff) as usize]
}

/// WCAG relative luminance of a `0x00RRGGBB` colour. Same definition the
/// renderer floors with, so the host can check final ink before rendering.
pub fn relative_luminance(rgb: u32) -> f32 {
    relative_luminance_with(relative_luminance_table(), rgb)
}

/// WCAG contrast ratio (`>= 1`) between two `0x00RRGGBB` colours, on
/// [`relative_luminance`].
#[must_use]
pub fn contrast_ratio(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Floor `rgb`'s WCAG contrast against `ground` to at least `min` by moving
/// its lightness alone, so the hue holds.
///
/// The colour moves toward the pole the ground contrasts with more (the
/// choice `aterm_render::floor_fg_contrast` makes, so any `min` up to
/// `sqrt(21) ≈ 4.58` is reachable on every ground):
///
/// * toward black it is SHADED: every channel scaled by one factor, which
///   keeps hue and saturation exactly;
/// * toward white it is first BRIGHTENED the same way, at constant hue and
///   saturation, until its brightest channel reaches 255, and only past that
///   tinted toward white.
///
/// It never blends toward another colour (a theme cursor, an accent), so an
/// ember stays an ember and a violet stays violet. Bisected on the quantized
/// colour along that monotone path, so the returned bytes clear `min`. An
/// unreachable `min` returns the pole's end of the path.
#[must_use]
pub fn floor_contrast_keeping_hue(rgb: u32, ground: u32, min: f32) -> u32 {
    let rgb = rgb & 0x00FF_FFFF;
    // A ratio of 1 or less (or NaN) asks for no floor.
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
    // contrast(black, g) = (L + 0.05) / 0.05 wins iff (L + 0.05)² >= 0.0525.
    let darken = (lg + 0.05) * (lg + 0.05) >= 0.05 * 1.05;
    let chan = |sh: u32| ((rgb >> sh) & 0xff) as f32;
    let (r, g, b) = (chan(16), chan(8), chan(0));
    let peak = r.max(g).max(b);
    let up = if peak > 0.0 { 255.0 / peak } else { 1.0 };
    let pack = |r: f32, g: f32, b: f32| {
        let q = |c: f32| (c.round().clamp(0.0, 255.0)) as u32;
        (q(r) << 16) | (q(g) << 8) | q(b)
    };
    // One parameter `t` in 0..=1 walks the whole path, luminance monotone.
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

/// A caret body's fill on a LIGHT page at `heat` `0..=1`: the hue
/// concentrated instead of whitened.
///
/// On a dark page a body whitens as it heats; on a white one that sinks the
/// caret into the page ("additive light cannot brighten a white ground").
/// Here saturation climbs toward the pure hue (60% of the way at rest, all of
/// it at full heat) and value falls by up to [`LIGHT_DEPTH_MAX`], so typing
/// reads as the colour deepening, the light-theme law the rainbow body already
/// follows ("a saturated hue just darkens"). Raising saturation first is what
/// keeps a pastel style hue (sparkle gold, comet ice, storm violet) from going
/// to mud when the page floor later lowers its value. A near-grey keeps its
/// saturation.
#[must_use]
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

/// A caret colour `rgb` charged by `k` `0..=1` on a LIGHT page, continuously
/// from `rgb` itself at `k = 0`: its saturation climbs toward the pure hue
/// and its value falls by up to [`LIGHT_DEPTH_MAX`], the same deepening
/// [`light_body_fill`] gives a body at full heat, so a hot caret reads as
/// its own colour concentrating, never as a warm tint mixed into it (blue
/// and orange mix to grey, pink and orange to brown). A near-grey keeps its
/// saturation.
#[must_use]
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

/// A caret body built on a PINNED cursor colour (a configured `cursor_color`
/// or a program's OSC 12) at `charge` `0..=1`: exactly `pinned` at rest, and
/// moving toward the body's own `styled` fill only as the body charges.
///
/// A pinned colour is the body's base and passes the page floor untouched,
/// as the same cursor with effects off would (owner, 2026-08-29), so at rest
/// the caret IS that cursor: no whitening frost, no deepening, no hue
/// whisper. `settled` is the body's settle threshold (the charge at or under
/// which it stops asking for frames): from there down the fill is `pinned`
/// to the byte, and the move toward `styled` starts from it continuously. A
/// non-finite charge reads as rest.
#[must_use]
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

/// OKLab `[L, a, b]` of a `0x00RRGGBB` colour (Björn Ottosson's matrices), the
/// uniform space a tint's distance from its page is weighed in.
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

/// The Euclidean OKLab distance between two `0x00RRGGBB` colours.
pub(crate) fn oklab_distance(a: u32, b: u32) -> f32 {
    let (x, y) = (oklab(a), oklab(b));
    ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2)).sqrt()
}

/// HSV (`h` degrees, `s`/`v` in `0..=1`) → `0x00RRGGBB`.
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
    /// colour that already clears it byte-identical. The grounds are the two
    /// shipped default pages and white.
    #[test]
    fn the_page_floor_clears_the_ratio_and_keeps_the_hue() {
        for (rgb, ground) in [
            (0x0051_0A00, 0x0011_1318), // the fire ember on the dark default
            (0x000F_70B7, 0x0011_1318), // still water on the dark default
            (0x00B4_8CFF, 0x00FF_FFFF), // storm violet on white
            (0x00FF_D9A0, 0x00FF_FFFF), // sparkle gold on white
            (0x009E_D6FF, 0x00FD_F6E3), // comet ice on Solarized light
            (0x0000_00FF, 0x0011_1318), // a pure blue on the dark default
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
        // A colour that already clears the floor is returned as is.
        for (rgb, ground) in [(0x0009_69DA, 0x00FF_FFFF), (0x00FF_FFFF, 0x0011_1318)] {
            assert_eq!(floor_contrast_keeping_hue(rgb, ground, 4.5), rgb);
        }
        // A ratio of 1 (or NaN) is no floor at all.
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
