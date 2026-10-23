// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stats surface colours derived in OkLab from ground, foreground, and accent.

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

#[inline]
fn srgb_to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
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
#[inline]
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
#[inline]
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
#[inline]
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
#[inline]
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
#[inline]
#[must_use]
pub fn luminance(c: Rgb) -> f32 {
    0.2126 * srgb_to_linear(c[0]) + 0.7152 * srgb_to_linear(c[1]) + 0.0722 * srgb_to_linear(c[2])
}

/// WCAG contrast ratio between two colours (≥ 1).
#[inline]
#[must_use]
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether `ground` is a dark ground.
#[inline]
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

/// `away(ground, dl)` with `dl` grown from its first value until the result
/// stands at least `floor` : 1 from the ground (the floors of design §12.1
/// are solved, not assumed: the rules stay, the values move).
fn away_floored(ground: Rgb, dl: f32, floor: f32, dark: bool) -> Rgb {
    let mut d = dl;
    let mut c = away(ground, d, dark);
    while contrast(c, ground) < floor && d < 1.0 {
        d += 0.005;
        c = away(ground, d, dark);
    }
    c
}

/// `fg` mixed `t` of the way toward `ground`, with `t` eased back toward the
/// foreground until the mix reads at least `floor` : 1 on the ground; a
/// theme whose own foreground falls short (Solarized Light's is 4.1:1 on its
/// background) is then moved further from the ground in lightness.
fn words_floored(fg: Rgb, ground: Rgb, t: f32, floor: f32) -> Rgb {
    let mut k = t;
    let mut c = mix(fg, ground, k);
    while contrast(c, ground) < floor && k > 0.0 {
        k = (k - 0.01).max(0.0);
        c = mix(fg, ground, k);
    }
    let dark = is_dark(ground);
    let mut d = 0.0;
    while contrast(c, ground) < floor && d < 1.0 {
        d += 0.01;
        c = away(fg, d, dark);
    }
    c
}

/// Walks `x` from `start` by `step` until `done(x)`, and returns that `x`;
/// where no `x` in `0..=1` is done, the end of the range (the best it
/// allows).
fn walk(start: f32, step: f32, done: impl Fn(f32) -> bool) -> f32 {
    let mut i = 0.0;
    loop {
        let x = (start + step * i).clamp(0.0, 1.0);
        if done(x) || x <= 0.0 || x >= 1.0 {
            return x;
        }
        i += 1.0;
    }
}

/// `c` itself when `ok(c)`; else `c`'s hue and chroma at the first
/// lightness, stepped by `step` away from a dark ground (lighter) or a light
/// one (darker), at which `ok` holds, or at the end of the range.
fn pushed(c: Rgb, step: f32, dark: bool, ok: impl Fn(Rgb) -> bool) -> Rgb {
    if ok(c) {
        return c;
    }
    let x = to_lab(c);
    let at = |l: f32| from_lab(Lab { l, ..x });
    at(walk(x.l, if dark { step } else { -step }, |l| ok(at(l))))
}

/// `c` stepped `dl` in lightness at a time away from a dark ground (lighter)
/// or a light one (darker) until `ok` holds, each step taken from the 8-bit
/// colour the one before drew, with the chroma `tint` gives it; where the
/// range runs out first, the last step.
fn stepped(
    c: Rgb,
    dl: f32,
    dark: bool,
    tint: impl Fn(Lab) -> (f32, f32),
    ok: impl Fn(Rgb) -> bool,
) -> Rgb {
    let away = if dark { dl } else { -dl };
    let short = |asked: f32, drawn: f32| (asked - drawn) / away >= 0.5;
    let inside = |l: f32| if dark { l < 1.0 } else { l > 0.0 };
    let (mut c, mut asked) = (c, to_lab(c).l);
    while !ok(c) && inside(asked) {
        let x = to_lab(c);
        let (a, b) = tint(x);
        let from = if short(asked, x.l) { asked } else { x.l };
        asked = (from + away).clamp(0.0, 1.0);
        c = from_lab(Lab { l: asked, a, b });
    }
    c
}

/// `c` pushed away from `ground` in lightness, hue kept, until it reads at
/// least `floor` : 1 on it.
fn floored(c: Rgb, ground: Rgb, floor: f32, dark: bool) -> Rgb {
    pushed(c, 0.004, dark, |x| contrast(x, ground) >= floor)
}

/// The point of the OkLab polyline through `stops` whose lightness is `l`;
/// before the first stop or past the last, that stop's hue and chroma at
/// `l`.
fn polyline_at(stops: &[Lab], l: f32) -> Lab {
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if (a.l.min(b.l)..=a.l.max(b.l)).contains(&l) {
            let t = if a.l == b.l {
                0.0
            } else {
                (l - a.l) / (b.l - a.l)
            };
            return Lab {
                l,
                a: a.a + (b.a - a.a) * t,
                b: a.b + (b.b - a.b) * t,
            };
        }
    }
    let (first, last) = (stops[0], stops[stops.len() - 1]);
    let end = if (l - first.l).abs() < (l - last.l).abs() {
        first
    } else {
        last
    };
    Lab { l, ..end }
}

/// The least lightness step between adjacent inks of `ramp`, counted away
/// from the ground (a step back toward it is negative).
fn min_rise(ramp: &[Rgb], dark: bool) -> f32 {
    let away = if dark { 1.0 } else { -1.0 };
    ramp.windows(2)
        .map(|pair| (to_lab(pair[1]).l - to_lab(pair[0]).l) * away)
        .fold(f32::INFINITY, f32::min)
}

/// The seven-step ramps the heat solver weighs on `ground` with step 0 at
/// `floor` (Stats Deck v2 §9.1), in the order it weighs them, every floor
/// judged on the 8-bit colour drawn.
fn heat_ramps(
    ground: Rgb,
    floor: f32,
    stops: &[Rgb],
    dark: bool,
) -> impl Iterator<Item = [Rgb; 7]> + use<> {
    let stops: Vec<Lab> = stops.iter().map(|&c| to_lab(c)).collect();
    let (first, last) = (stops[0].l, stops[stops.len() - 1].l);
    let step = if dark { 0.002 } else { -0.002 };
    let at = move |l: f32| from_lab(polyline_at(&stops, l));
    let g = to_lab(ground).l;
    let far = |c: Rgb| (to_lab(c).l - g) * step > 0.0;
    let l0 = walk(first, step, |l| {
        let c = at(l);
        far(c) && contrast(c, ground) >= floor
    });
    let start = if dark { last.max(l0) } else { last.min(l0) };
    let n = ((if dark { 1.0 - start } else { start }) / 0.002).ceil() as usize;
    (0..=n).map(move |i| {
        let l6 = (start + step * i as f32).clamp(0.0, 1.0);
        std::array::from_fn(|k| at(l0 + (l6 - l0) * k as f32 / 6.0))
    })
}

/// The heat ramp on `ground` with step 0 at `floor`: the first of
/// [`heat_ramps`] whose adjacent steps all stand [`HEAT_DL`] apart or,
/// where none does, the one whose closest steps stand furthest apart (the
/// first of equals), the best the range allows.
fn heat_ramp(ground: Rgb, floor: f32, stops: &[Rgb], dark: bool) -> [Rgb; 7] {
    let mut best = ([[0; 3]; 7], f32::NEG_INFINITY);
    for ramp in heat_ramps(ground, floor, stops, dark) {
        let rise = min_rise(&ramp, dark);
        if rise >= HEAT_DL {
            return ramp;
        }
        if rise > best.1 {
            best = (ramp, rise);
        }
    }
    best.0
}

/// The distance between two colours in OkLab.
#[must_use]
pub fn distance(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (to_lab(a), to_lab(b));
    ((x.l - y.l).powi(2) + (x.a - y.a).powi(2) + (x.b - y.b).powi(2)).sqrt()
}

/// The WCAG AA floor every word on a stats surface meets.
pub const WORD_AA: f32 = 4.5;
/// The rim contrast floor against its ground.
pub const RIM_FLOOR: f32 = 1.5;

/// The load ramp's five stops (0, 25, 50, 75, 100%) for a ground's polarity.
#[inline]
#[must_use]
pub const fn ramp_stops(dark: bool) -> [Rgb; 5] {
    if dark { LOAD_DARK } else { LOAD_LIGHT }
}

const LOAD_DARK: [Rgb; 5] = [
    [0x27, 0x39, 0x52],
    [0x4A, 0x60, 0x76],
    [0x72, 0x8B, 0x92],
    [0xC4, 0xB6, 0x5F],
    [0xFF, 0xE1, 0x48],
];

const LOAD_LIGHT: [Rgb; 5] = [
    [0xBF, 0xD4, 0xE7],
    [0x99, 0xB2, 0xCF],
    [0x7D, 0x8E, 0xAD],
    [0xA8, 0x54, 0x19],
    [0x8C, 0x29, 0x01],
];

/// The light heat polyline: the light load ramp with a near-neutral stop
/// (`#857069`) where it turns from blue to rust, so the middle step reads
/// "middle" rather than "hot", and a darker hot end (`#751A00`), so seven
/// steps clear [`HEAT_DL`].
const HEAT_LIGHT: [Rgb; 7] = [
    [0xBF, 0xD4, 0xE7],
    [0x99, 0xB2, 0xCF],
    [0x7D, 0x8E, 0xAD],
    [0x85, 0x70, 0x69],
    [0xA8, 0x54, 0x19],
    [0x8C, 0x29, 0x01],
    [0x75, 0x1A, 0x00],
];

/// The heat ramp's stops for a ground's polarity.
#[inline]
#[must_use]
pub const fn heat_stops(dark: bool) -> &'static [Rgb] {
    if dark { &LOAD_DARK } else { &HEAT_LIGHT }
}

/// The light Increase Contrast polyline: step 0 at 3:1 on a light ground
/// sits just above the light polyline's turn to rust, so there step 1 would
/// already read hot.
const HEAT_IC_LIGHT: [Rgb; 7] = [
    [0xBF, 0xD4, 0xE7],
    [0x99, 0xB2, 0xCF],
    [0x7D, 0x8E, 0xAD],
    [0x85, 0x70, 0x69],
    [0x7F, 0x51, 0x3C],
    [0x8C, 0x29, 0x01],
    [0x75, 0x1A, 0x00],
];

/// The Increase Contrast heat ramp's stops.
#[inline]
#[must_use]
pub const fn heat_ic_stops(dark: bool) -> &'static [Rgb] {
    if dark { &LOAD_DARK } else { &HEAT_IC_LIGHT }
}

/// The heat steps' lower edges as fractions of max.
pub const HEAT_EDGES: [f32; 6] = [0.05, 0.20, 0.40, 0.60, 0.80, 0.95];

/// Hysteresis margin past a heat step's edge.
pub const HEAT_HOLD: f32 = 0.01;

/// The heat step (`0..=6`) of `v`, a fraction of max.
#[inline]
#[must_use]
pub fn heat_step(v: f32) -> u8 {
    debug_assert!(!v.is_nan(), "a heat step for a value that is not a reading");
    let mut step = 0;
    for edge in HEAT_EDGES {
        if v >= edge {
            step += 1;
        }
    }
    step
}

/// The hysteresis-held heat step of `v` from `prev`.
#[inline]
#[must_use]
pub fn heat_step_held(v: f32, prev: u8) -> u8 {
    let up = heat_step(v - HEAT_HOLD);
    let down = heat_step(v + HEAT_HOLD);
    if up > prev {
        up
    } else if down < prev {
        down
    } else {
        prev
    }
}

/// Heat cell height in device px for `step` under Differentiate Without Color.
#[inline]
#[must_use]
pub fn dwc_height(cell: u32, step: u8) -> u32 {
    let q = ((cell - cell.div_ceil(4)) / 6).max(1);
    cell.saturating_sub(u32::from(6 - step.min(6)) * q)
}

/// Minimum OkLab lightness step between adjacent heat steps.
pub const HEAT_DL: f32 = 0.05;
/// Step 0 contrast floor on a dark ground.
pub const HEAT_FLOOR_DARK: f32 = 2.05;
/// Step 0 contrast floor on a light ground.
pub const HEAT_FLOOR_LIGHT: f32 = 1.80;
/// The WCAG non-text contrast floor.
pub const NON_TEXT: f32 = 3.0;
/// Minimum OkLab lightness step between adjacent scope inks.
pub const SCOPE_DL: f32 = 0.08;

/// Halo blend mode.
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
    /// A hairline groove between two bars.
    pub groove: Rgb,
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
    /// Memory ladder: used, compressed, cached files, free.
    pub mem: [Rgb; 4],
    /// Stuck work (frost).
    pub stuck: Rgb,
    /// Quiet work: frost eased toward the ground.
    pub quiet: Rgb,
    /// Warning state ink.
    pub warn: Rgb,
    /// Critical state ink.
    pub critical: Rgb,
    /// Primary words.
    pub words: Rgb,
    /// Secondary words (labels, units).
    pub words2: Rgb,
    /// The Deck's seven-step heat ramp.
    pub heat: [Rgb; 7],
    /// The % figure's ink at each heat step.
    pub heat_text: [Rgb; 7],
    /// The heat ramp under Increase Contrast.
    pub heat_ic: [Rgb; 7],
    /// A column with no reading.
    pub hollow: Rgb,
    /// The table's rules between rows and columns.
    pub grid: Rgb,
    /// The Deck's seam above the grid.
    pub seam: Rgb,
    /// The scope bar's Global segment.
    pub machine: Rgb,
    /// The Global segment under Increase Contrast.
    pub machine_ic: Rgb,
    /// aterm's segment under Increase Contrast.
    pub aterm_ic: Rgb,
    /// The Tab's segment under Increase Contrast.
    pub tab_ic: Rgb,
    /// The over-max mark.
    pub over: Rgb,
    /// The Deck's warn words and marks.
    pub warn_text: Rgb,
    /// The Deck's critical words and marks.
    pub critical_text: Rgb,
}

/// The over mark's orange (never red) before it is floored on the ground.
const fn over_orange(dark: bool) -> Rgb {
    if dark {
        [0xFF, 0x9A, 0x2E]
    } else {
        [0xE0, 0x7A, 0x1F]
    }
}

/// Solve the inks for `ground`, with the theme's `fg` and `accent`.
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
    let aterm_ab = (0.6 * acc_c * acc_h.cos(), 0.6 * acc_c * acc_h.sin());
    let aterm = from_lab(Lab {
        l: (g.l + if dark { 0.30 } else { -0.30 }).clamp(0.0, 1.0),
        a: aterm_ab.0,
        b: aterm_ab.1,
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
    let stuck = if dark {
        oklch(0.90, 0.06, 225.0)
    } else {
        oklch(0.42, 0.07, 235.0)
    };
    let quiet = {
        let mut k = 0.4;
        let mut c = mix(stuck, ground, k);
        while contrast(c, ground) < 3.0 && k > 0.0 {
            k = (k - 0.02).max(0.0);
            c = mix(stuck, ground, k);
        }
        c
    };
    let warn = if dark {
        [0xE9, 0xC4, 0x6A]
    } else {
        [0x8A, 0x61, 0x00]
    };
    let critical = if dark {
        [0xFF, 0x6E, 0x6E]
    } else {
        [0xC4, 0x26, 0x2E]
    };
    let heat_floor = if dark {
        HEAT_FLOOR_DARK
    } else {
        HEAT_FLOOR_LIGHT
    };
    let heat = heat_ramp(ground, heat_floor, heat_stops(dark), dark);
    let halved = |x: Lab| (x.a * 0.5, x.b * 0.5);
    let machine = neutral(0.21);
    let beyond = |c: Rgb, inner: Rgb| {
        (to_lab(c).l - to_lab(inner).l) * if dark { 1.0 } else { -1.0 } >= SCOPE_DL
    };
    let machine_ic = stepped(machine, 0.005, dark, halved, |c| {
        contrast(c, track) >= NON_TEXT
    });
    let aterm_ic = stepped(aterm, 0.005, dark, |_| aterm_ab, |c| beyond(c, machine_ic));
    let tab_ic = pushed(tab, 0.005, dark, |c| beyond(c, aterm_ic));
    VitalsInks {
        dark,
        ground,
        load: ramp_stops(dark),
        track,
        rim: away_floored(ground, 0.12, RIM_FLOOR, dark),
        groove: {
            let mut x = g;
            x.l = (g.l - if dark { 0.025 } else { 0.035 }).clamp(0.0, 1.0);
            from_lab(x)
        },
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
        stuck,
        quiet,
        warn,
        critical,
        words: words_floored(fg, ground, 0.05, WORD_AA),
        words2: words_floored(fg, ground, 0.38, WORD_AA),
        heat,
        heat_text: heat.map(|c| floored(c, ground, WORD_AA, dark)),
        heat_ic: heat_ramp(ground, NON_TEXT, heat_ic_stops(dark), dark),
        hollow: stepped(neutral(0.12), 0.005, dark, halved, |c| {
            contrast(c, ground) >= NON_TEXT
        }),
        grid: neutral(0.085),
        seam: neutral(0.10),
        machine,
        machine_ic,
        aterm_ic,
        tab_ic,
        over: floored(over_orange(dark), ground, NON_TEXT, dark),
        warn_text: floored(warn, ground, WORD_AA, dark),
        critical_text: floored(critical, ground, WORD_AA, dark),
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

    /// The rising sliver's ink over a fill of colour `fill`.
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

    /// Returns any Deck contrast/lightness floors these inks miss.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn deck_floor_misses(&self) -> Vec<FloorMiss> {
        const OVER_APART: f32 = 0.10;
        let g = self.ground;
        let away = if self.dark { 1.0 } else { -1.0 };
        let at_end = |c: Rgb| {
            from_lab(Lab {
                l: if self.dark { 1.0 } else { 0.0 },
                ..to_lab(c)
            })
        };
        let beyond = |inner: Rgb, outer: Rgb| (to_lab(outer).l - to_lab(inner).l) * away;
        let mut misses = Vec::new();
        let mut want = |held: bool, reached: bool, measured: f32, what: String| {
            if !held {
                misses.push(FloorMiss {
                    what,
                    measured,
                    out_of_reach: !reached,
                });
            }
        };
        let step0 = if self.dark {
            HEAT_FLOOR_DARK
        } else {
            HEAT_FLOOR_LIGHT
        };
        for (name, ramp, floor, stops) in [
            ("heat", &self.heat, step0, heat_stops(self.dark)),
            ("heat_ic", &self.heat_ic, NON_TEXT, heat_ic_stops(self.dark)),
        ] {
            let best = heat_ramps(g, floor, stops, self.dark)
                .map(|r| min_rise(&r, self.dark))
                .fold(f32::NEG_INFINITY, f32::max);
            let rise = min_rise(ramp, self.dark);
            want(
                rise >= HEAT_DL,
                best >= HEAT_DL || rise < best,
                rise,
                format!("{name} steps {rise:.3} apart in L, {best:.3} at best"),
            );
            let c = contrast(ramp[0], g);
            want(
                c >= floor,
                contrast(at_end(ramp[0]), g) >= floor,
                c,
                format!("{name} step 0 at {c:.2}:1, floor {floor}"),
            );
            let near = ramp.iter().filter(|&&c| beyond(g, c) <= 0.0).count();
            want(
                near == 0,
                true,
                near as f32,
                format!("{name} has {near} of 7 steps on the ground's near side"),
            );
        }
        let floored = (0..7).map(|i| {
            let name = format!("heat_text {i}");
            (name, self.heat_text[i], self.heat[i], WORD_AA)
        });
        for (name, ink, from, floor) in floored.chain([
            ("hollow".to_owned(), self.hollow, self.hollow, NON_TEXT),
            (
                "over".to_owned(),
                self.over,
                over_orange(self.dark),
                NON_TEXT,
            ),
            ("warn_text".to_owned(), self.warn_text, self.warn, WORD_AA),
            (
                "critical_text".to_owned(),
                self.critical_text,
                self.critical,
                WORD_AA,
            ),
        ]) {
            let c = contrast(ink, g);
            want(
                c >= floor,
                contrast(at_end(from), g) >= floor,
                c,
                format!("{name} at {c:.2}:1, floor {floor}"),
            );
        }
        let c = contrast(self.machine_ic, self.track);
        want(
            c >= NON_TEXT,
            contrast(at_end(self.machine_ic), self.track) >= NON_TEXT,
            c,
            format!("machine_ic at {c:.2}:1 on the track"),
        );
        for (inner, outer, a, b, from) in [
            (
                "machine_ic",
                "aterm_ic",
                self.machine_ic,
                self.aterm_ic,
                self.aterm,
            ),
            ("aterm_ic", "tab_ic", self.aterm_ic, self.tab_ic, self.tab),
        ] {
            let dl = beyond(a, b);
            want(
                dl >= SCOPE_DL,
                beyond(a, at_end(from)) >= SCOPE_DL,
                dl,
                format!("{outer} {dl:.3} beyond {inner} in L"),
            );
        }
        for (inner, outer, a, b) in [
            ("machine", "aterm", self.machine, self.aterm),
            ("aterm", "tab", self.aterm, self.tab),
        ] {
            let dl = beyond(a, b);
            want(
                dl >= SCOPE_DL,
                true,
                dl,
                format!("{outer} {dl:.3} beyond {inner} in L"),
            );
        }
        for (name, other) in [
            ("critical_text", self.critical_text),
            ("heat 6", self.heat[6]),
            ("heat_ic 6", self.heat_ic[6]),
        ] {
            let d = distance(self.over, other);
            want(d >= OVER_APART, true, d, format!("over {d:.3} from {name}"));
        }
        let stops = ramp_stops(self.dark);
        let moved = (0..5).filter(|&i| self.load[i] != stops[i]).count();
        want(
            moved == 0,
            true,
            moved as f32,
            format!("load has {moved} of 5 stops off the load ramp"),
        );
        misses
    }
}

/// One Deck floor a set of inks misses ([`VitalsInks::deck_floor_misses`]).
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug, PartialEq)]
pub struct FloorMiss {
    /// The floor and what was measured against it.
    pub what: String,
    /// Measured value in the floor's own terms.
    pub measured: f32,
    /// Whether the ground put the floor out of reach.
    pub out_of_reach: bool,
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
    const NORD_BAND: Rgb = [0x49, 0x4F, 0x5B];

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
            assert!(contrast(inks.load_at(1.0), inks.track) >= 3.0);
        }
    }

    /// On a light ground the Increase Contrast ramp warms step by step: its
    /// first two steps stay muted (OkLab chroma under 0.06), so a low load
    /// does not read as hot, and it reaches rust (over 0.12) by step 5.
    #[test]
    fn the_light_ic_ramp_warms_step_by_step() {
        let chroma = |c: Rgb| {
            let l = to_lab(c);
            l.a.hypot(l.b)
        };
        let warms = |ramp: &[Rgb; 7]| {
            let c: Vec<f32> = ramp.iter().map(|&x| chroma(x)).collect();
            c[0] < 0.06 && c[1] < 0.06 && c[..6].iter().any(|&x| x > 0.12)
        };
        let grounds = [
            LIGHT_BAND,
            [0xFF; 3],
            [0xF6, 0xF8, 0xFA],
            [0xFD, 0xF6, 0xE3],
        ];
        for ground in grounds {
            let inks = vitals_inks(ground, LIGHT_FG, LIGHT_ACCENT);
            assert!(!inks.dark, "{ground:02X?}");
            assert!(warms(&inks.heat_ic), "{ground:02X?}: {:02X?}", inks.heat_ic);
        }
        let plain = heat_ramp(LIGHT_BAND, NON_TEXT, heat_stops(false), false);
        assert!(!warms(&plain), "{plain:02X?}");
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
            assert!(contrast(inks.tide, inks.track) >= 3.0);
            assert!(contrast(inks.words, ground) >= 4.5);
            assert!(contrast(inks.stuck, ground) >= 3.0, "{inks:?}");
            assert!(contrast(inks.quiet, ground) >= 3.0, "{inks:?}");
        }
    }

    /// Every word on a stats surface meets AA and every rim F1, on the
    /// grounds the Deck, the sill and the lens paint on: the band grounds of
    /// Default and Default Light, the terminal backgrounds the sill sits on,
    /// and the extremes, pure black and pure white.
    #[test]
    fn words_meet_aa_and_rims_meet_f1_on_every_ground() {
        let cases = [
            (DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT),
            (LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT),
            ([0x11, 0x13, 0x18], [0xD0, 0xD0, 0xD0], DEFAULT_ACCENT),
            ([0xF6, 0xF7, 0xF9], LIGHT_FG, LIGHT_ACCENT),
            ([0, 0, 0], [0xC0, 0xC0, 0xC0], DEFAULT_ACCENT),
            ([255, 255, 255], [0x60, 0x60, 0x60], LIGHT_ACCENT),
            ([0x00, 0x2B, 0x36], [0x83, 0x94, 0x96], [0x93, 0xA1, 0xA1]),
            ([0xFD, 0xF6, 0xE3], [0x65, 0x7B, 0x83], [0x58, 0x6E, 0x75]),
        ];
        for (ground, fg, acc) in cases {
            let inks = vitals_inks(ground, fg, acc);
            assert!(
                contrast(inks.words, ground) >= WORD_AA,
                "{ground:?} {inks:?}"
            );
            assert!(
                contrast(inks.words2, ground) >= WORD_AA,
                "{ground:?} {inks:?}"
            );
            assert!(
                contrast(inks.rim, ground) >= RIM_FLOOR,
                "{ground:?} {inks:?}"
            );
            assert!(
                contrast(inks.words2, ground) <= contrast(inks.words, ground) + 1e-3,
                "{ground:?} {inks:?}"
            );
        }
    }

    /// The warn and critical inks read at 3:1 on both band grounds (they mark
    /// a state, beside words that carry it too).
    #[test]
    fn the_state_inks_read_on_both_grounds() {
        for (ground, fg, acc) in [
            (DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT),
            (LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT),
        ] {
            let inks = vitals_inks(ground, fg, acc);
            assert!(contrast(inks.warn, ground) >= 3.0, "{inks:?}");
            assert!(contrast(inks.critical, ground) >= 3.0, "{inks:?}");
            let d = |c: Rgb| (to_lab(c).l - to_lab(ground).l).abs();
            assert!(d(inks.groove) > 0.01 && d(inks.groove) < 0.06, "{inks:?}");
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
                assert!((a.l - b.l).abs() >= 0.08, "{i}: {fill:?} vs {s:?}");
            }
        }
    }

    /// The heat bins of design v2 §6.0, edges included in the step above.
    #[test]
    fn heat_steps_follow_the_bins() {
        for (v, step) in [
            (0.0, 0),
            (0.049, 0),
            (0.05, 1),
            (0.199, 1),
            (0.20, 2),
            (0.399, 2),
            (0.40, 3),
            (0.599, 3),
            (0.60, 4),
            (0.799, 4),
            (0.80, 5),
            (0.949, 5),
            (0.95, 6),
            (1.0, 6),
            (1.7, 6),
            (-0.2, 0),
        ] {
            assert_eq!(heat_step(v), step, "{v}");
        }
    }

    /// T-V3: an unknown value never becomes the filled 0 % cell.
    #[test]
    #[should_panic(expected = "not a reading")]
    fn a_nan_is_never_a_heat_step() {
        let _ = heat_step(f32::NAN);
    }

    #[test]
    #[should_panic(expected = "not a reading")]
    fn a_nan_is_never_a_held_heat_step() {
        let _ = heat_step_held(f32::NAN, 3);
    }

    /// Differentiate Without Color's heights, design v2 §9.5's table, and
    /// the rule's promise: in every cell of 7 px or more the height rises
    /// strictly with the step and is never under 1 px.
    #[test]
    fn dwc_heights_follow_the_table() {
        for (cell, want) in [
            (8, [2, 3, 4, 5, 6, 7, 8]),
            (9, [3, 4, 5, 6, 7, 8, 9]),
            (16, [4, 6, 8, 10, 12, 14, 16]),
            (17, [5, 7, 9, 11, 13, 15, 17]),
            (18, [6, 8, 10, 12, 14, 16, 18]),
        ] {
            let got: [u32; 7] = std::array::from_fn(|k| dwc_height(cell, k as u8));
            assert_eq!(got, want, "cell {cell}");
        }
        for cell in 7..=64 {
            assert!(dwc_height(cell, 0) >= 1, "cell {cell}");
            assert_eq!(dwc_height(cell, 6), cell, "cell {cell}");
            for k in 1..=6 {
                assert!(
                    dwc_height(cell, k) > dwc_height(cell, k - 1),
                    "cell {cell} step {k}"
                );
            }
        }
    }

    /// The live ink changes step only a point past an edge, either way, and
    /// a value that leaps several bins lands on the step it cleared.
    #[test]
    fn a_held_step_moves_only_a_point_past_an_edge() {
        for v in [0.395, 0.40, 0.405] {
            assert_eq!(heat_step_held(v, 2), 2, "{v} from 2");
            assert_eq!(heat_step_held(v, 3), 3, "{v} from 3");
        }
        assert_eq!(heat_step_held(0.415, 2), 3);
        assert_eq!(heat_step_held(0.385, 3), 2);
        assert_eq!(heat_step_held(0.955, 0), 5);
        assert_eq!(heat_step_held(0.97, 0), 6);
        assert_eq!(heat_step_held(0.02, 6), 0);
        assert_eq!(heat_step_held(0.045, 6), 1);
        for v in [0.1, 0.3, 0.5, 0.7, 0.9] {
            assert_eq!(heat_step_held(v, heat_step(v)), heat_step(v), "{v}");
        }
    }

    fn hex(s: &str) -> Rgb {
        let n = u32::from_str_radix(s.trim_start_matches('#'), 16).expect("a hex colour");
        [(n >> 16) as u8, (n >> 8) as u8, n as u8]
    }

    fn hexes<const N: usize>(s: &str) -> [Rgb; N] {
        let v: Vec<Rgb> = s.split_whitespace().map(hex).collect();
        v.try_into().expect("N colours")
    }

    fn near(name: &str, got: Rgb, want: Rgb) {
        for k in 0..3 {
            assert!(
                got[k].abs_diff(want[k]) <= 1,
                "{name}: {got:02X?}, the table has {want:02X?}"
            );
        }
    }

    /// T-V9 on the reference grounds: the solver reproduces the design v2
    /// §9.1 table within one unit per channel, and every floor holds.
    #[test]
    fn the_deck_inks_reproduce_the_reference_table() {
        struct Row {
            ground: &'static str,
            fg: &'static str,
            accent: &'static str,
            heat: &'static str,
            heat_text: &'static str,
            heat_ic: &'static str,
            grid_seam: &'static str,
            hollow: &'static str,
            scopes: &'static str,
            scopes_ic: &'static str,
            states: &'static str,
        }
        let rows = [
            Row {
                ground: "#303135",
                fg: "#F8F8F2",
                accent: "#50FA7B",
                heat: "#4C6277 #607886 #768D90 #9DA27D #C3B660 #E1CB57 #FFE148",
                heat_text: "#839BB2 #839CAB #859DA0 #9DA27D #C3B660 #E1CB57 #FFE148",
                heat_ic: "#657E89 #798F8F #979F80 #B5AF6C #D0BF5C #E7D054 #FFE148",
                grid_seam: "#474749 #4B4B4D",
                hollow: "#7A7A7A",
                scopes: "#696A6C #439955 #50FA7B",
                scopes_ic: "#898989 #62B871 #50FA7B",
                states: "#FF9A2E #E9C46A #FF6E6E #B4E7FC",
            },
            Row {
                ground: "#E2E3E5",
                fg: "#24292F",
                accent: "#1A7F37",
                heat: "#94ACC9 #8598B6 #818394 #94664F #9C420F #8C2901 #751A00",
                heat_text: "#516782 #556682 #626474 #885B44 #9C420F #8C2901 #751A00",
                heat_ic: "#82808D #84665B #834931 #802100 #690B00 #530000 #3D0000",
                grid_seam: "#C7C7C8 #C2C3C4",
                hollow: "#818181",
                scopes: "#A0A0A1 #619368 #01732C",
                scopes_ic: "#757575 #3A6B42 #00570E",
                states: "#CA6600 #865E00 #C3242D #20536E",
            },
        ];
        for row in rows {
            let inks = vitals_inks(hex(row.ground), hex(row.fg), hex(row.accent));
            let g = row.ground;
            let ramps = [
                ("heat", inks.heat, hexes::<7>(row.heat)),
                ("heat_text", inks.heat_text, hexes(row.heat_text)),
                ("heat_ic", inks.heat_ic, hexes(row.heat_ic)),
            ];
            for (name, got, want) in ramps {
                for i in 0..7 {
                    near(&format!("{g} {name} {i}"), got[i], want[i]);
                }
            }
            let singles = [
                ("grid", inks.grid, hexes::<2>(row.grid_seam)[0]),
                ("seam", inks.seam, hexes::<2>(row.grid_seam)[1]),
                ("hollow", inks.hollow, hex(row.hollow)),
                ("machine", inks.machine, hexes::<3>(row.scopes)[0]),
                ("aterm", inks.aterm, hexes::<3>(row.scopes)[1]),
                ("tab", inks.tab, hexes::<3>(row.scopes)[2]),
                ("machine_ic", inks.machine_ic, hexes::<3>(row.scopes_ic)[0]),
                ("aterm_ic", inks.aterm_ic, hexes::<3>(row.scopes_ic)[1]),
                ("tab_ic", inks.tab_ic, hexes::<3>(row.scopes_ic)[2]),
                ("over", inks.over, hexes::<4>(row.states)[0]),
                ("warn_text", inks.warn_text, hexes::<4>(row.states)[1]),
                (
                    "critical_text",
                    inks.critical_text,
                    hexes::<4>(row.states)[2],
                ),
                ("stuck", inks.stuck, hexes::<4>(row.states)[3]),
            ];
            for (name, got, want) in singles {
                near(&format!("{g} {name}"), got, want);
            }
            let from = if inks.dark { 3 } else { 4 };
            assert_eq!(inks.heat_text[from..], inks.heat[from..], "{g}");
            assert_eq!(inks.deck_floor_misses(), [], "{g}");
        }
        assert_eq!(
            ramp_stops(true),
            hexes::<5>("#273952 #4A6076 #728B92 #C4B65F #FFE148")
        );
        assert_eq!(
            ramp_stops(false),
            hexes::<5>("#BFD4E7 #99B2CF #7D8EAD #A85419 #8C2901")
        );
        assert_eq!(heat_stops(true), ramp_stops(true));
    }

    /// T-V9 on a spread of grounds, black to the default dark band and white
    /// to greys darker than the default light band (on #C0C0C0 the Increase
    /// Contrast aterm ink once stalled short of its floor), with accents of
    /// every kind: every floor holds.
    #[test]
    fn the_deck_inks_hold_on_every_ground() {
        let accents = [
            DEFAULT_ACCENT,
            LIGHT_ACCENT,
            [0x26, 0x8B, 0xD2],
            [0xFF, 0x79, 0xC6],
            [0xD0, 0xD0, 0xD0],
        ];
        let grounds = [
            ([0x00, 0x00, 0x00], [0xC0, 0xC0, 0xC0]),
            ([0x11, 0x13, 0x18], [0xD0, 0xD0, 0xD0]),
            ([0x00, 0x2B, 0x36], [0x83, 0x94, 0x96]),
            ([0x1E, 0x1E, 0x2E], [0xCD, 0xD6, 0xF4]),
            ([0x28, 0x2A, 0x36], [0xF8, 0xF8, 0xF2]),
            (DEFAULT_DARK_BAND, DEFAULT_FG),
            ([0xFF, 0xFF, 0xFF], [0x24, 0x29, 0x2F]),
            ([0xF6, 0xF7, 0xF9], LIGHT_FG),
            ([0xFD, 0xF6, 0xE3], [0x65, 0x7B, 0x83]),
            ([0xEF, 0xF1, 0xF5], [0x4C, 0x4F, 0x69]),
            (LIGHT_BAND, LIGHT_FG),
            ([0xC8, 0xCA, 0xCE], [0x20, 0x20, 0x20]),
            ([0xC0, 0xC0, 0xC0], [0x20, 0x20, 0x20]),
        ];
        for (ground, fg) in grounds {
            for accent in accents {
                let inks = vitals_inks(ground, fg, accent);
                assert_eq!(inks.deck_floor_misses(), [], "{ground:02X?} {accent:02X?}");
            }
        }
    }

    /// A ground lighter than a dark band usually is leaves the Increase
    /// Contrast ramp too little lightness above its 3:1 step 0 for seven
    /// steps 0.05 apart once its yellow hot end clips.
    #[test]
    fn a_floor_out_of_reach_is_said_and_the_solver_still_answers() {
        let inks = vitals_inks(NORD_BAND, [0xEC, 0xEF, 0xF4], [0x88, 0xC0, 0xD0]);
        let misses = inks.deck_floor_misses();
        assert_eq!(misses.len(), 1, "{misses:?}");
        assert!(misses[0].out_of_reach, "{misses:?}");
        assert!(misses[0].what.starts_with("heat_ic steps"), "{misses:?}");
        for ground in [NORD_BAND, [0x3A, 0x3B, 0x4E]] {
            let rise = min_rise(
                &heat_ramp(ground, NON_TEXT, heat_ic_stops(true), true),
                true,
            );
            let rises: Vec<f32> = heat_ramps(ground, NON_TEXT, heat_ic_stops(true), true)
                .map(|ramp| min_rise(&ramp, true))
                .collect();
            for &other in &rises {
                assert!(other <= rise && other < HEAT_DL, "{ground:02X?}: {rises:?}");
            }
            let end = rises[rises.len() - 1];
            assert!(rise > end + 0.001, "{ground:02X?}: {rise} vs {end}");
            assert!(rise > 0.035, "{ground:02X?}: {rise}");
        }

        let grey = [0x7A, 0x7A, 0x7A];
        let inks = vitals_inks(grey, [0xFF; 3], DEFAULT_ACCENT);
        let misses = inks.deck_floor_misses();
        assert!(
            misses.iter().any(|m| m.what.starts_with("heat_text")),
            "{misses:?}"
        );
        for miss in misses.iter().filter(|m| m.what.starts_with("heat_text")) {
            assert!(miss.out_of_reach, "{miss:?}");
        }
        for text in inks.heat_text {
            assert!(to_lab(text).l > 0.95, "{text:?}");
        }
    }

    /// NEGATIVE CONTROL: the check sees a broken ink, and never calls a
    /// solver's fault the ground's.
    #[test]
    fn the_floor_check_catches_a_broken_ink() {
        let good = vitals_inks(DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT);
        let mut broken = good;
        broken.over = good.critical_text;
        broken.heat_text[1] = good.heat[1];
        broken.heat[4] = good.heat[3];
        broken.load[2] = good.heat[2];
        broken.heat_ic[0] = good.groove;
        let mut misses = broken.deck_floor_misses();
        let mut settled = vitals_inks(NORD_BAND, [0xEC, 0xEF, 0xF4], [0x88, 0xC0, 0xD0]);
        let ramps = heat_ramps(NORD_BAND, NON_TEXT, heat_ic_stops(true), true);
        settled.heat_ic = ramps.last().expect("the end of the range");
        misses.extend(settled.deck_floor_misses());
        for (start, n) in [
            ("over 0.000 from critical_text", 1),
            ("heat_text 1", 1),
            ("heat steps", 1),
            ("heat_ic has 1 of 7 steps on the ground's near side", 1),
            ("heat_ic steps 0.036", 1),
            ("load", 1),
        ] {
            let found: Vec<_> = misses
                .iter()
                .filter(|m| m.what.starts_with(start))
                .collect();
            assert_eq!(found.len(), n, "{start}: {misses:?}");
            assert!(!found[0].out_of_reach, "{start}: {misses:?}");
        }
    }

    /// A mid-tone ground lies beyond the heat polyline's first stop, which
    /// can read step 0's floor on the ground's near side already; step 0
    /// stands on the far side still, so the ramp never passes back through
    /// the ground (on #686868 to #9C9C9C step 0 once sat under the ground,
    /// and middle steps read 1.0:1 on it).
    #[test]
    fn the_heat_ramp_never_crosses_a_mid_tone_ground() {
        for v in 0x60..=0xA0 {
            let ground = [v; 3];
            let dark = is_dark(ground);
            let fg = if dark { [0xFF; 3] } else { [0; 3] };
            let inks = vitals_inks(ground, fg, DEFAULT_ACCENT);
            let g = to_lab(ground).l;
            let away = if dark { 1.0 } else { -1.0 };
            for ramp in [inks.heat, inks.heat_ic] {
                for c in ramp {
                    let past = (to_lab(c).l - g) * away;
                    assert!(past > 0.0, "{ground:02X?}: {ramp:02X?}");
                }
            }
            let misses = inks.deck_floor_misses();
            for miss in misses.iter().filter(|m| m.what.starts_with("heat")) {
                assert!(miss.out_of_reach, "{ground:02X?}: {miss:?}");
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
