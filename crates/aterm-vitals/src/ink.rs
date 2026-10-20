// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stats surfaces' colours (design §12.1, §12.6), derived in OkLab from
//! the ground they sit on, the theme's foreground and its accent, so no
//! surface invents a colour.
//!
//! * **The load ramp** is fixed and colour-blind safe (blue-grey → steel →
//!   olive → yellow on dark grounds; pale blue → slate → rust on light), with
//!   linear OkLab interpolation between its five stops, so mid levels pass
//!   through low chroma and only hot tiles turn warm. It never comes from the
//!   theme accent: the Default theme's accent is a green cursor, and an
//!   accent → warn → error ramp is the green → red failure.
//! * **The scope ladder** is solved in lightness from the ground: others,
//!   aterm, then the Tab in the accent, each further from the ground.
//! * **Rise** is the warm glow ink; **frost** the cold stuck ink (O35): warm
//!   is work getting busier, cold is work that stopped.

/// An sRGB colour.
pub type Rgb = [u8; 3];

/// A colour in OkLab.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lab {
    /// Lightness, `0..=1`.
    pub l: f32,
    /// Green–red.
    pub a: f32,
    /// Blue–yellow.
    pub b: f32,
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

/// sRGB → OkLab.
#[must_use]
pub fn to_lab(c: Rgb) -> Lab {
    let (r, g, b) = (
        srgb_to_linear(c[0]),
        srgb_to_linear(c[1]),
        srgb_to_linear(c[2]),
    );
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    Lab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

/// OkLab → sRGB, clipped to the gamut.
#[must_use]
pub fn from_lab(c: Lab) -> Rgb {
    let l = c.l + 0.396_337_78 * c.a + 0.215_803_76 * c.b;
    let m = c.l - 0.105_561_346 * c.a - 0.063_854_17 * c.b;
    let s = c.l - 0.089_484_18 * c.a - 1.291_485_5 * c.b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        linear_to_srgb(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s),
        linear_to_srgb(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s),
        linear_to_srgb(-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s),
    ]
}

/// OkLCh (lightness, chroma, hue in degrees) → sRGB.
#[must_use]
pub fn oklch(l: f32, c: f32, h_deg: f32) -> Rgb {
    let h = h_deg.to_radians();
    from_lab(Lab {
        l,
        a: c * h.cos(),
        b: c * h.sin(),
    })
}

/// Linear interpolation in OkLab, `t` in `0..=1`.
#[must_use]
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let (x, y) = (to_lab(a), to_lab(b));
    let t = t.clamp(0.0, 1.0);
    from_lab(Lab {
        l: x.l + (y.l - x.l) * t,
        a: x.a + (y.a - x.a) * t,
        b: x.b + (y.b - x.b) * t,
    })
}

/// WCAG relative luminance.
#[must_use]
pub fn luminance(c: Rgb) -> f32 {
    0.2126 * srgb_to_linear(c[0]) + 0.7152 * srgb_to_linear(c[1]) + 0.0722 * srgb_to_linear(c[2])
}

/// WCAG contrast ratio between two colours (≥ 1).
#[must_use]
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether `ground` is a dark ground (the polarity every rule keys on).
#[must_use]
pub fn is_dark(ground: Rgb) -> bool {
    to_lab(ground).l < 0.6
}

/// `c` moved `dl` in OkLab lightness away from a dark ground (lighter) or a
/// light one (darker), keeping its hue and chroma.
fn away(c: Rgb, dl: f32, dark: bool) -> Rgb {
    let mut x = to_lab(c);
    x.l = (x.l + if dark { dl } else { -dl }).clamp(0.0, 1.0);
    from_lab(x)
}

/// The load ramp's five stops (0, 25, 50, 75, 100 %) for a ground's polarity:
/// the sRGB design §12.1 measured for its OkLCh stops (`oklch(0.340 0.050
/// 258)` … `oklch(0.910 0.170 98)` dark, `oklch(0.860 0.035 245)` …
/// `oklch(0.430 0.150 38)` light), taken as measured because the hot dark stop
/// lies outside sRGB and its gamut mapping is part of what was measured.
#[must_use]
pub fn ramp_stops(dark: bool) -> [Rgb; 5] {
    if dark {
        [
            [0x27, 0x39, 0x52],
            [0x4A, 0x60, 0x76],
            [0x72, 0x8B, 0x92],
            [0xC4, 0xB6, 0x5F],
            [0xFF, 0xE1, 0x48],
        ]
    } else {
        [
            [0xBF, 0xD4, 0xE7],
            [0x99, 0xB2, 0xCF],
            [0x7D, 0x8E, 0xAD],
            [0xA8, 0x54, 0x19],
            [0x8C, 0x29, 0x01],
        ]
    }
}

/// The halo's blend: light added on a dark ground, a veil laid over a light
/// one (added light is invisible over white).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaloMode {
    /// Add light.
    Add,
    /// Lay a translucent veil.
    Over,
}

/// Every ink the stats surfaces draw with, for one ground.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VitalsInks {
    /// Whether the ground is dark.
    pub dark: bool,
    /// The ground the inks were solved on.
    pub ground: Rgb,
    /// The load ramp's stops.
    pub load: [Rgb; 5],
    /// An idle capacity: a bar's or tile's empty part.
    pub track: Rgb,
    /// A tile's or capsule's rim.
    pub rim: Rgb,
    /// The 1 px tide line at a fill's top or a bar's end.
    pub tide: Rgb,
    /// The glow and the rising sliver's pull.
    pub rise: Rgb,
    /// How the glow blends.
    pub rise_mode: HaloMode,
    /// Others' share (the machine less aterm).
    pub others: Rgb,
    /// aterm's share.
    pub aterm: Rgb,
    /// The tab's share.
    pub tab: Rgb,
    /// Memory: used, compressed, cached files, free (a neutral ladder, never
    /// a scope ink, so memory never reads as ownership).
    pub mem: [Rgb; 4],
    /// Stuck work (frost).
    pub stuck: Rgb,
    /// Primary words.
    pub words: Rgb,
    /// Secondary words (labels, units).
    pub words2: Rgb,
}

/// Solve the inks for `ground`, with the theme's foreground `fg` and accent
/// `accent` (the cursor colour).
#[must_use]
pub fn vitals_inks(ground: Rgb, fg: Rgb, accent: Rgb) -> VitalsInks {
    let dark = is_dark(ground);
    let g = to_lab(ground);
    let track = away(ground, 0.055, dark);
    let others = {
        let mut x = to_lab(ground);
        x.l = (g.l + if dark { 0.17 } else { -0.17 }).clamp(0.0, 1.0);
        let h = x.b.atan2(x.a);
        x.a = 0.008 * h.cos();
        x.b = 0.008 * h.sin();
        from_lab(x)
    };
    let acc = to_lab(accent);
    let acc_c = acc.a.hypot(acc.b);
    let acc_h = acc.b.atan2(acc.a);
    let aterm = from_lab(Lab {
        l: (g.l + if dark { 0.30 } else { -0.30 }).clamp(0.0, 1.0),
        a: 0.6 * acc_c * acc_h.cos(),
        b: 0.6 * acc_c * acc_h.sin(),
    });
    let tab = {
        let want = if dark { g.l + 0.43 } else { g.l - 0.43 };
        let mut x = acc;
        if (dark && x.l < want) || (!dark && x.l > want) {
            x.l = want.clamp(0.0, 1.0);
        }
        from_lab(x)
    };
    let neutral = |dl: f32| {
        let mut x = to_lab(ground);
        x.l = (g.l + if dark { dl } else { -dl }).clamp(0.0, 1.0);
        x.a *= 0.5;
        x.b *= 0.5;
        from_lab(x)
    };
    VitalsInks {
        dark,
        ground,
        load: ramp_stops(dark),
        track,
        rim: away(ground, 0.12, dark),
        tide: if dark {
            oklch(0.96, 0.01, 250.0)
        } else {
            oklch(0.22, 0.02, 255.0)
        },
        rise: if dark {
            [0xFC, 0xA9, 0x42]
        } else {
            [0xDF, 0x7B, 0x0C]
        },
        rise_mode: if dark { HaloMode::Add } else { HaloMode::Over },
        others,
        aterm,
        tab,
        mem: [neutral(0.42), neutral(0.30), neutral(0.17), track],
        stuck: if dark {
            oklch(0.90, 0.06, 225.0)
        } else {
            oklch(0.42, 0.07, 235.0)
        },
        words: mix(fg, ground, 0.05),
        words2: mix(fg, ground, 0.38),
    }
}

impl VitalsInks {
    /// The load ramp at `level` (`0..=1`), linear in OkLab between stops.
    #[must_use]
    pub fn load_at(&self, level: f32) -> Rgb {
        let t = if level.is_finite() {
            level.clamp(0.0, 1.0) * 4.0
        } else {
            0.0
        };
        let i = (t.floor() as usize).min(3);
        mix(self.load[i], self.load[i + 1], t - i as f32)
    }

    /// The rising sliver's ink over a fill of colour `fill`: the fill moved
    /// 0.14 in lightness (away from the ground, or toward it where that would
    /// leave the range) with its hue pulled halfway to the rise ink (F6).
    #[must_use]
    pub fn sliver(&self, fill: Rgb) -> Rgb {
        let f = to_lab(fill);
        let r = to_lab(self.rise);
        let up = if self.dark { 0.14 } else { -0.14 };
        let l = if (0.0..=1.0).contains(&(f.l + up)) {
            f.l + up
        } else {
            f.l - up
        };
        from_lab(Lab {
            l,
            a: f.a + (r.a - f.a) * 0.5,
            b: f.b + (r.b - f.b) * 0.5,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_DARK_BAND: Rgb = [0x30, 0x31, 0x35];
    const DEFAULT_FG: Rgb = [0xF8, 0xF8, 0xF2];
    const DEFAULT_ACCENT: Rgb = [0x50, 0xFA, 0x7B];
    const LIGHT_BAND: Rgb = [0xE2, 0xE3, 0xE5];
    const LIGHT_FG: Rgb = [0x2B, 0x2D, 0x33];
    const LIGHT_ACCENT: Rgb = [0x1A, 0x7F, 0x37];

    #[test]
    fn oklab_round_trips_srgb() {
        for c in [
            [0, 0, 0],
            [255, 255, 255],
            [0x30, 0x31, 0x35],
            [0xFC, 0xA9, 0x42],
        ] {
            let back = from_lab(to_lab(c));
            for k in 0..3 {
                assert!(back[k].abs_diff(c[k]) <= 1, "{c:?} → {back:?}");
            }
        }
    }

    #[test]
    fn the_ramp_stops_are_the_designs_oklch_stops() {
        // Each in-gamut stop is its OkLCh value to within a unit; both hot
        // stops lie outside sRGB, so only their measured mapping stands.
        let want = [
            (true, 0, (0.340, 0.050, 258.0)),
            (true, 2, (0.620, 0.030, 215.0)),
            (false, 0, (0.860, 0.035, 245.0)),
            (false, 2, (0.645, 0.050, 262.0)),
        ];
        for (dark, i, (l, c, h)) in want {
            let got = ramp_stops(dark)[i];
            let ok = oklch(l, c, h);
            for k in 0..3 {
                assert!(got[k].abs_diff(ok[k]) <= 1, "{dark} {i}: {got:?} vs {ok:?}");
            }
        }
    }

    #[test]
    fn the_ramps_lightness_is_monotone_away_from_the_ground() {
        for (ground, fg, acc) in [
            (DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT),
            (LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT),
        ] {
            let inks = vitals_inks(ground, fg, acc);
            let mut last = to_lab(inks.load_at(0.0)).l;
            for i in 1..=20 {
                let l = to_lab(inks.load_at(i as f32 / 20.0)).l;
                if inks.dark {
                    assert!(l >= last - 1e-3, "{i}: {l} < {last}");
                } else {
                    assert!(l <= last + 1e-3, "{i}: {l} > {last}");
                }
                last = l;
            }
            // F3: full load stands out at least 3:1 from the track.
            assert!(contrast(inks.load_at(1.0), inks.track) >= 3.0);
        }
    }

    #[test]
    fn the_scope_ladder_steps_away_from_the_ground() {
        for (ground, fg, acc) in [
            (DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT),
            (LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT),
        ] {
            let inks = vitals_inks(ground, fg, acc);
            let d = |c: Rgb| (to_lab(c).l - to_lab(ground).l).abs();
            assert!(d(inks.track) < d(inks.others), "{inks:?}");
            assert!(d(inks.others) < d(inks.aterm), "{inks:?}");
            assert!(d(inks.aterm) <= d(inks.tab) + 1e-3, "{inks:?}");
            // F2: the tide reads ≥ 3:1 on the track.
            assert!(contrast(inks.tide, inks.track) >= 3.0);
            // F5: primary words reach 4.5:1 on the ground.
            assert!(contrast(inks.words, ground) >= 4.5);
            // F7 (first cut): frost reaches 3:1 on the ground.
            assert!(contrast(inks.stuck, ground) >= 3.0, "{inks:?}");
        }
    }

    #[test]
    fn the_rising_sliver_differs_from_its_fill_at_every_level() {
        for ground in [DEFAULT_DARK_BAND, LIGHT_BAND] {
            let inks = vitals_inks(ground, DEFAULT_FG, DEFAULT_ACCENT);
            for i in 0..=20 {
                let fill = inks.load_at(i as f32 / 20.0);
                let s = inks.sliver(fill);
                let (a, b) = (to_lab(fill), to_lab(s));
                // F6 (first cut): a visible lightness step.
                assert!((a.l - b.l).abs() >= 0.08, "{i}: {fill:?} vs {s:?}");
            }
        }
    }

    #[test]
    fn polarity_and_halo_mode_follow_the_ground() {
        assert!(is_dark(DEFAULT_DARK_BAND));
        assert!(!is_dark(LIGHT_BAND));
        let dark = vitals_inks(DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT);
        let light = vitals_inks(LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT);
        assert_eq!(dark.rise_mode, HaloMode::Add);
        assert_eq!(light.rise_mode, HaloMode::Over);
    }
}
