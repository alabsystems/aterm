// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Glyph tint and light-theme ink.

use super::*;

/// The themes the tint is proved against: a real light spread including
/// two that sit BELOW AA on their own body text, so the "never demand more
/// than the theme delivers" arm is exercised rather than assumed.
const TINT_THEMES: [(&str, u32, u32); 5] = [
    ("GitHub Light", 0x001F_2328, 0x00FF_FFFF),
    ("GitHub canvas", 0x001F_2328, 0x00F6_F8FA),
    ("Solarized Light", 0x0065_7B83, 0x00FD_F6E3), // 4.13:1 — sub-AA
    ("Grey on grey", 0x0064_6464, 0x00E0_E0E0),    // 3.86:1 — sub-AA
    ("Paper", 0x0033_3333, 0x00FA_F8F5),
];

/// ONE SPECTRUM, NO SEAM — the unification, pinned.
///
/// Every mark that lands on the light rail line reads `rainbow_sweep_at` /
/// `rainbow_band_at`. Two properties matter and neither is obvious:
/// the sweep must be CONTINUOUS (the six bands are an acyclic ramp, so a
/// wrapping sweep walks violet straight into red and prints a hard seam),
/// and the band a given cell resolves to must be THE SAME for the rail, the
/// pop's bar and the glyph tint.
#[test]
fn one_rainbow_sweep_is_continuous_and_shared() {
    // CONTINUITY: no jump bigger than one step of the sweep's own rate,
    // anywhere across several full traversals — a `rem_euclid` wrap shows
    // up here as a jump of nearly 1.0.
    let step = RAINBOW_LIGHT_RAIL_SPREAD;
    for phase_i in 0..8 {
        let phase = phase_i as f32 * 0.37;
        let mut prev = rainbow_sweep_at(0, phase);
        for col in 1..400u16 {
            let now = rainbow_sweep_at(col, phase);
            assert!(
                (now - prev).abs() <= step * 1.5 + 1e-5,
                "seam at col {col}, phase {phase}: {prev} -> {now}"
            );
            assert!((0.0..=1.0).contains(&now), "sweep stays in range: {now}");
            prev = now;
        }
    }
    // …and it is a real traversal, not a constant: both ends of the
    // spectrum are actually reached.
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for col in 0..400u16 {
        let v = rainbow_sweep_at(col, 0.0);
        lo = lo.min(v);
        hi = hi.max(v);
    }
    assert!(
        lo < 0.05 && hi > 0.95,
        "the sweep spans the spectrum: {lo}..{hi}"
    );
    // SHARED: the band a cell resolves to is a function of (col, phase)
    // alone, so every consumer that calls it agrees by construction. Pin
    // that the resolver is drawn from the seven anchors and nothing else.
    for col in 0..64u16 {
        for phase_i in 0..8 {
            let band = rainbow_band_at(col, phase_i as f32 * 0.37);
            assert!(
                RAINBOW_BANDS.contains(&band),
                "col {col}: {band:06X} is one of the seven anchors"
            );
        }
    }
}

/// THE BOX GUARD IS NOT DECORATION — a negative control.
///
/// An ENDPOINT-only bound (`L(ink) <= cap`, which is what a natural reading
/// of "both ends are dark" would give you) is UNSOUND: a lerp between a
/// bluish foreground and a reddish ink passes through a colour whose
/// channels are the max of both, and that corner can be brighter than
/// either end. This pins that the corner bound is strictly stronger, so a
/// future simplification back to endpoints fails here instead of shipping.
#[test]
fn glyph_tint_box_corner_is_stronger_than_its_endpoints() {
    let mut found = false;
    for &(_, fg, bg) in &TINT_THEMES {
        let Some((_, cap)) = glyph_ink_bound(fg, bg) else {
            continue;
        };
        for &band in &RAINBOW_BANDS {
            // A candidate that passes the ENDPOINT test but fails the BOX
            // test must exist somewhere in the family, or the guard is
            // vacuous on this palette.
            for step in 0..=64 {
                let k = step as f32 / 64.0;
                let ink = {
                    let (r, g, b) = (
                        ((band >> 16) & 0xff) as f32,
                        ((band >> 8) & 0xff) as f32,
                        (band & 0xff) as f32,
                    );
                    let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
                    let span = (hi - lo).max(1e-3);
                    let ch = |c: f32| ((((c - lo) / span * hi) * k + 0.5) as u32).min(255);
                    (ch(r) << 16) | (ch(g) << 8) | ch(b)
                };
                if rel_luminance(ink) <= cap && rel_luminance(chan_max(ink, fg)) > cap {
                    found = true;
                }
            }
        }
    }
    assert!(
        found,
        "the box corner rejects candidates the endpoint test accepts — \
         if this ever stops being true the guard has gone vacuous"
    );
}

/// BISECTION SOUNDNESS — the property the palette's exactness rests on.
///
/// `fresh_ink_glyph_ink` finds its scale by bisection, which is only valid
/// if the predicate is MONOTONE in `k` — and the predicate is evaluated on
/// the QUANTIZED candidate, so it has to stay monotone after rounding to
/// u8, not merely in the reals. This walks 257 values of `k` per band per
/// theme and asserts the predicate never goes false→true, and separately
/// that the RETURNED ink satisfies the predicate it was selected by (a
/// bisection can converge on the wrong side of a step if the endpoints are
/// mishandled).
#[test]
fn glyph_tint_bisection_predicate_is_monotone_and_its_answer_holds() {
    for (name, fg, bg) in TINT_THEMES {
        let (side, cap) = glyph_ink_bound(fg, bg).expect("a usable theme");
        for &band in &RAINBOW_BANDS {
            // Rebuild the family exactly as the emitter's helper does.
            let (r, g, b) = (
                ((band >> 16) & 0xff) as f32,
                ((band >> 8) & 0xff) as f32,
                (band & 0xff) as f32,
            );
            let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
            let span = (hi - lo).max(1e-3);
            let sat = |c: f32| ((c - lo) / span * hi).clamp(0.0, 255.0);
            let s3 = [sat(r), sat(g), sat(b)];
            let cand = |k: f32| -> u32 {
                let ch = |i: usize| -> u32 {
                    let v = match side {
                        GlyphInkSide::Dark => s3[i] * k,
                        GlyphInkSide::Light => s3[i] + (255.0 - s3[i]) * (1.0 - k),
                    };
                    ((v + 0.5) as u32).min(255)
                };
                (ch(0) << 16) | (ch(1) << 8) | ch(2)
            };
            let ok = |ink: u32| -> bool {
                let corner = match side {
                    GlyphInkSide::Dark => chan_max(ink, fg),
                    GlyphInkSide::Light => chan_min(ink, fg),
                };
                let l = rel_luminance(corner);
                match side {
                    GlyphInkSide::Dark => l <= cap,
                    GlyphInkSide::Light => l >= cap,
                }
            };
            let mut seen_false = false;
            for step in 0..=256 {
                let k = step as f32 / 256.0;
                let v = ok(cand(k));
                if !v {
                    seen_false = true;
                } else {
                    assert!(
                        !seen_false,
                        "{name} band {band:06X}: the predicate went false -> true at k={k:.4}, \
                         so bisection is not valid on this family"
                    );
                }
            }
            // …and the answer that ships satisfies the predicate.
            let ink = fresh_ink_glyph_ink(band, fg, side, cap).expect("a usable band");
            assert!(
                ok(ink),
                "{name} band {band:06X}: the RETURNED ink {ink:06X} fails its own predicate"
            );
        }
    }
}

/// THE PALETTE IS MEMOIZED ON THE THEME, not recomputed per frame — and it
/// re-derives the moment the theme moves.
#[test]
fn glyph_tint_palette_is_memoized_on_the_theme_key() {
    let g = geom();
    let mut c = rainbow_pop_light_cfg();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    let first = glow
        .glyph_palette
        .expect("a key is memoized on the first tick");
    assert_eq!(first.0, (c.theme_fg, c.theme_bg));
    assert!(first.1.is_some(), "the fixture theme is usable");
    // A tick with an unchanged theme keeps the SAME memo (bit-identical).
    glow.tick(
        Some((3, 3)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert_eq!(glow.glyph_palette, Some(first));
    // Moving the theme re-derives it.
    c.theme_fg = 0x0065_7B83;
    c.theme_bg = 0x00FD_F6E3;
    glow.tick(
        Some((3, 4)),
        t0 + Duration::from_millis(32),
        &c,
        g,
        &mut out,
    );
    let second = glow.glyph_palette.expect("re-derived");
    assert_eq!(second.0, (0x0065_7B83, 0x00FD_F6E3));
    assert_ne!(
        second.1, first.1,
        "a different theme yields a different palette"
    );
    // A REFUSAL is memoized too, so it is decided once and not per frame.
    c.theme_fg = 0x0012_3456;
    c.theme_bg = 0x0012_3456;
    glow.tick(
        Some((3, 5)),
        t0 + Duration::from_millis(48),
        &c,
        g,
        &mut out,
    );
    assert_eq!(glow.glyph_palette, Some(((0x0012_3456, 0x0012_3456), None)));
    assert!(
        glow.charred().is_empty(),
        "a conceal-shaped theme substitutes no foreground"
    );
}

/// PATHOLOGICAL THEMES GET NO TINT — the layer stands down rather than
/// inventing a side the theme does not have.
#[test]
fn glyph_tint_suppresses_itself_on_a_theme_with_no_usable_side() {
    // A sentinel is not a colour — masking its high byte would read as
    // pure black and flip the ink side.
    assert!(fresh_ink_glyph_palette(aterm_core::render::COLOR_UNSET, 0x00FF_FFFF).is_none());
    assert!(fresh_ink_glyph_palette(0x001F_2328, aterm_core::render::COLOR_UNSET).is_none());
    // Conceal-shaped: never reveal what the theme hid.
    assert!(fresh_ink_glyph_palette(0x0012_3456, 0x0012_3456).is_none());
    // Below the usable floor.
    assert!(fresh_ink_glyph_palette(0x00FF_FFFF, 0x00CC_CCCC).is_none());
    // Equiluminant but different hues: contrast is ~1:1, so there is no
    // side at all. Caught by the floor, not by the side test.
    let a = 0x00D3_0000;
    let b = 0x0000_7A00;
    assert!(
        (rel_luminance(a) - rel_luminance(b)).abs() < 0.02,
        "fixture is equiluminant ({:.3} vs {:.3})",
        rel_luminance(a),
        rel_luminance(b)
    );
    assert!(fresh_ink_glyph_palette(a, b).is_none());
}

/// GREYSCALE-EXHAUSTIVE: over every grey (fg, bg) pair, wherever the
/// palette exists the invariant holds at nine weights across all seven bands.
/// This is the sweep that turns "the argument is sound" into "the argument
/// is sound for every input the type admits", within the grey slice.
#[test]
fn glyph_tint_invariant_holds_across_every_grey_theme() {
    let mut usable = 0usize;
    for f in 0..=255u32 {
        for b in 0..=255u32 {
            let fg = (f << 16) | (f << 8) | f;
            let bg = (b << 16) | (b << 8) | b;
            let Some(palette) = fresh_ink_glyph_palette(fg, bg) else {
                continue;
            };
            usable += 1;
            let (side, cap) = glyph_ink_bound(fg, bg).expect("a palette implies a bound");
            let bar = wcag_ratio(fg, bg).min(FRESH_INK_GLYPH_TARGET_RATIO);
            for &ink in &palette {
                let corner = match side {
                    GlyphInkSide::Dark => chan_max(ink, fg),
                    GlyphInkSide::Light => chan_min(ink, fg),
                };
                match side {
                    GlyphInkSide::Dark => assert!(rel_luminance(corner) <= cap + 1e-6),
                    GlyphInkSide::Light => assert!(rel_luminance(corner) >= cap - 1e-6),
                }
                for step in 0..=8 {
                    let w = step as f32 / 8.0;
                    let px = lerp_rgb(fg, ink, w);
                    assert!(
                        wcag_ratio(px, bg) + 0.02 >= bar,
                        "fg {f:02X} bg {b:02X} ink {ink:06X} w {w:.2}: \
                         {:.2}:1 under the {bar:.2}:1 bar",
                        wcag_ratio(px, bg)
                    );
                }
            }
        }
    }
    assert!(
        usable > 4000,
        "the sweep actually exercised the tint on {usable} grey themes"
    );
}

/// THE LIGHT-THEME INK BAR: [`light_ink`] pulls EVERY rainbow band under
/// both [`LIGHT_INK_MAX_LUMA`] and [`LIGHT_INK_MAX_CHANNEL`] while keeping
/// it recognisably its own hue, so a white-ground capture reads as one
/// material rather than as yellow-tinted paper beside violet stains.
#[test]
fn light_ink_darkens_every_band_to_one_bar() {
    for &band in &RAINBOW_BANDS {
        let ink = light_ink(band);
        assert!(
            luma709(ink) <= LIGHT_INK_MAX_LUMA + 1.0,
            "band {band:06X} -> {ink:06X} luma {} exceeds the bar",
            luma709(ink)
        );
        let hi = ((ink >> 16) & 0xff).max((ink >> 8) & 0xff).max(ink & 0xff) as f32;
        assert!(
            hi <= LIGHT_INK_MAX_CHANNEL + 1.0,
            "band {band:06X} -> {ink:06X} peaks at {hi}, over the channel bar"
        );
        // HUE PRESERVED: uniform scaling can TIE two channels that rounding
        // brings together, but it can never INVERT them — so the band is
        // still its own colour at ink weight. Stated as "no inversion"
        // rather than "same boolean triple" because the triple treats a tie
        // as information: the spectrum's yellow is `#838400`, whose R and G
        // are one level apart, and scaling it to ink weight lands them
        // equal. That is the scaling working, not the hue moving.
        for (hi, lo) in [(16u32, 8u32), (8, 0), (16, 0)] {
            let chan = |c: u32, sh: u32| ((c >> sh) & 0xff) as i32;
            let (bh, bl) = (chan(band, hi), chan(band, lo));
            let (ih, il) = (chan(ink, hi), chan(ink, lo));
            assert!(
                (bh <= bl || ih >= il) && (bh >= bl || ih <= il),
                "band {band:06X} inverts channels {hi}/{lo} at ink weight ({ink:06X})"
            );
        }
    }
    // A colour already under both bars is returned untouched — the dark
    // bands are not needlessly crushed.
    assert_eq!(light_ink(0x0010_2030), 0x0010_2030);
}

/// THE LEADING-ONLY BAR BINDS FOR EVERY NAMED STOP. [`light_ink_bold`]'s job
/// is ONE ink weight across the whole spectrum, so a sweep rolling under a
/// moving cursor keeps a constant underline weight.
///
/// The retired recipe only ever scaled DOWN, so the bar could bind only for
/// a hue whose own saturated luma already exceeded it — true of orange,
/// yellow and green, false of red, blue and violet. Measured across the six
/// bands the ink came out at luma 46 / 120 / 120 / 120 / 108 / 27: a 4.4x
/// swing in ink weight as the sweep rolled.
#[test]
fn light_ink_bold_holds_one_ink_weight_across_the_spectrum() {
    let order = |c: u32| {
        let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        (r >= g, g >= b, r >= b)
    };
    for &band in &RAINBOW_BANDS {
        let ink = light_ink_bold(band);
        let y = luma709(ink);
        assert!(
            (y - LIGHT_INK_BOLD_MAX_LUMA).abs() <= 1.5,
            "band {band:06X} -> {ink:06X} lands at luma {y}, not ON the bar"
        );
        let hi = ((ink >> 16) & 0xff).max((ink >> 8) & 0xff).max(ink & 0xff) as f32;
        assert!(
            hi <= LIGHT_INK_BOLD_MAX_CHANNEL + 1.0,
            "band {band:06X} -> {ink:06X} peaks at {hi}, over the channel bar"
        );
        // HUE PRESERVED: the band is still recognisably its own colour.
        assert_eq!(
            order(band),
            order(ink),
            "band {band:06X} keeps its hue at ink weight ({ink:06X})"
        );
    }
    // THE WARM MIDDLE IS UNTOUCHED. Orange, yellow and green already met
    // the bar by scaling alone, and are byte-identical to the retired
    // recipe — the change reached exactly the bands the bar never governed.
    assert_eq!(light_ink_bold(0x00FF_9900), 0x00BB_7000);
    assert_eq!(light_ink_bold(0x00FF_FF00), 0x0081_8100);
    assert_eq!(light_ink_bold(0x0033_FF00), 0x0020_9E00);
    // AN ACHROMATIC INPUT IS NOT BLACK. `r == g == b` leaves a zero chroma
    // span; the retired recipe divided by it and returned literal BLACK for
    // WHITE — the single most common "invisible on paper" hue there is.
    assert_eq!(light_ink_bold(0x00FF_FFFF), 0x0078_7878);
    assert_eq!(light_ink_bold(0x0080_8080), 0x0078_7878);
    // A genuinely black input still has no hue to weigh.
    assert_eq!(light_ink_bold(0), 0);
}

/// **THE COMMITTED BAND TABLE IS THE SPECTRUM'S OWN STOPS** — the pin that
/// stops the transcription in [`RAINBOW_BANDS`] from drifting away from
/// [`crate::spectrum`].
///
/// The table has to stay a `const` (array lengths and const contexts across
/// this file depend on it), which is the only reason it is written out at
/// all. Every entry is `spectrum_stop(i)` and nothing else, so there is
/// still exactly ONE place a rainbow colour is decided.
#[test]
fn rainbow_bands_are_the_spectrum_stops() {
    for (i, &band) in RAINBOW_BANDS.iter().enumerate() {
        assert_eq!(
            band,
            spectrum_stop(i),
            "RAINBOW_BANDS[{i}] is not the spectrum's stop {i}"
        );
    }
    // Red is the anchor the arc's luminance was chosen to preserve: this
    // family's red does not move, and the design says so in §2.2.
    assert_eq!(RAINBOW_BANDS[0], 0x00FF_0000);
    // The names are ordered and distinct — a vocabulary, not a ramp with a
    // repeat in it.
    let distinct: std::collections::BTreeSet<u32> = RAINBOW_BANDS.iter().copied().collect();
    assert_eq!(distinct.len(), SPECTRUM_STOPS);
}

/// THE LIGHT-THEME LEGIBILITY BOUND: at [`LIGHT_INK_ALPHA_CAP`] — the
/// ceiling every light-theme transient's centre over-alpha is clamped to —
/// near-black ink stays dark and a white counter survives, for every band.
/// This is the property the raised cap (150 -> 190) has to keep.
#[test]
fn light_ink_cap_keeps_glyphs_legible() {
    let cap = LIGHT_INK_ALPHA_CAP as u8;
    let max_c = |p: u32| ((p >> 16) & 0xff).max((p >> 8) & 0xff).max(p & 0xff);
    let min_c = |p: u32| ((p >> 16) & 0xff).min((p >> 8) & 0xff).min(p & 0xff);
    for &band in &RAINBOW_BANDS {
        let ink = light_ink(band) | (u32::from(cap) << 24);
        let over_ink = aterm_render::over_rgb(0x000A_0A0A, ink, cap);
        let over_white = aterm_render::over_rgb(0x00FF_FFFF, ink, cap);
        assert!(
            max_c(over_ink) <= 0x80,
            "band {band:06X} over near-black ink stays dark ({over_ink:06X})"
        );
        assert!(
            min_c(over_white) < 0xF0,
            "band {band:06X} actually darkens a white ground ({over_white:06X})"
        );
    }
}

// ---- FRESH-INK POP (the typed-glyph highlight over the rainbow) --------

/// A crown-free, ring-free rainbow kitty config so the halo stream carries ONLY
/// fresh-ink pops (plus, on a hot run, nothing: stars/particles emit into
/// `out`, and the dark theme emits no rail veils).
fn rainbow_pop_cfg() -> GlowConfig {
    let mut c = cfg(GlowStyle::RainbowKitty, true);
    c.radius = 0.0;
    c.ring = false;
    c
}

// ---- the TYPING WAKE (the continuous plume under the typed row) --------

/// A crown-free, ring-free rainbow kitty config on a LIGHT theme — the light-arm twin
/// of [`rainbow_pop_cfg`], so the halo stream carries the fresh-ink DARKEN
/// veils (isolated from other halos by their exact veil colour below).
fn rainbow_pop_light_cfg() -> GlowConfig {
    let mut c = rainbow_pop_cfg();
    c.dark_theme = false;
    c
}
