// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Landing rings and halo culling.

use super::*;

/// LANDING RING — LIGHT-THEME fork (P0, the clearest additive gap): on dark
/// the ring is an additive square shockwave (GlowQuads); on light (invisible
/// on white) it inverts to an expanding DARKENED source-over veil ring in the
/// halo stream, capped for legibility — and the additive stream stays empty.
#[test]
fn rainbow_landing_ring_light_theme_forks_to_source_over() {
    let g = geom();
    let born = Instant::now();
    let at = born + Duration::from_millis(60);
    let ring = Ring {
        cx: 100.0,
        cy: 40.0,
        born,
        life: 0.30,
        scale: 1.0,
    };

    // DARK: additive square outline, never a source-over veil.
    let c_dark = cfg(GlowStyle::RainbowKitty, true);
    let dark = CursorGlow {
        ring: Some(ring),
        ..Default::default()
    };
    let (mut d_out, mut d_h) = (Vec::new(), Vec::new());
    dark.emit_ring(at, &c_dark, g, &mut d_out, &mut d_h);
    assert!(
        !d_out.is_empty(),
        "dark draws the additive square shockwave"
    );
    assert!(
        d_h.iter().all(|h| h.mode != HaloMode::Over),
        "the dark ring is additive, never source-over"
    );

    // LIGHT: source-over veil ring, no additive quads.
    let mut c_light = cfg(GlowStyle::RainbowKitty, true);
    c_light.dark_theme = false;
    let light = CursorGlow {
        ring: Some(ring),
        ..Default::default()
    };
    let (mut l_out, mut l_h) = (Vec::new(), Vec::new());
    light.emit_ring(at, &c_light, g, &mut l_out, &mut l_h);
    assert!(
        l_out.is_empty(),
        "the additive square shockwave is suppressed on light"
    );
    assert!(
        !l_h.is_empty() && l_h.iter().all(|h| h.mode == HaloMode::Over),
        "the light ring is a source-over darken veil"
    );
    assert!(
        l_h.iter().all(|h| {
            let cap = (h.color >> 24) & 0xff;
            (1..=120).contains(&cap)
        }),
        "the light ring veil is capped for legibility"
    );
}

/// The landing ring's four bars PARTITION the square outline: no pixel is
/// owned by two bars. The full-height verticals used to re-cover the th×th
/// corner blocks the horizontals already own — an additive double-add that
/// lit every corner of the expanding square as a ~2× "rivet" (the
/// rainbow-comb ownership bug's sibling, 2026-09-01 audit). The outline
/// must also stay CLOSED: every edge of the square keeps its light.
#[test]
fn classic_ring_grade_is_floored_and_capped_like_the_kittys() {
    // Impact 1 (an 8-cell hop or less) is exactly what it was.
    assert_eq!(classic_ring_radius_factor(1.0), 1.0);
    assert_eq!(classic_ring_life(0.18, 1.0), 0.18);
    // Below the floor and non-finite never SHRINK or explode a ring.
    assert_eq!(classic_ring_radius_factor(0.2), 1.0);
    assert_eq!(classic_ring_radius_factor(f32::NAN), 1.0);
    assert_eq!(classic_ring_life(0.18, f32::INFINITY), 0.18);
    // The cap: twice as wide, 300 ms — Rainbow Kitty's own numbers.
    assert!((classic_ring_radius_factor(3.5) - 2.0).abs() < 1e-5);
    assert!((classic_ring_life(0.18, 3.5) - 0.30).abs() < 1e-3);
    assert_eq!(
        classic_ring_radius_factor(9.0),
        classic_ring_radius_factor(3.5),
        "past the cap is the cap"
    );
    // Monotone across the graded range.
    let mut last = 0.0f32;
    for i in 0..=25 {
        let f = classic_ring_radius_factor(1.0 + i as f32 * 0.1);
        assert!(
            f >= last,
            "radius factor dips at impact {}",
            1.0 + i as f32 * 0.1
        );
        last = f;
    }
}

/// The same ring at the same age: a full-line jump's outline reaches
/// further from the landing cell than a hop's, on every square-outline
/// theme arm and for the fire flash — the grade is not a no-op.
#[test]
fn classic_landing_ring_expands_wider_with_the_jump() {
    let g = geom();
    let born = Instant::now();
    let extent = |style: GlowStyle, dark: bool, scale: f32| -> i32 {
        let c = cfg(style, dark);
        let glow = CursorGlow {
            ring: Some(Ring {
                cx: 100.0,
                cy: 40.0,
                born,
                life: 0.30,
                scale,
            }),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(120),
            &c,
            g,
            &mut out,
            &mut halos,
        );
        let quads = out
            .iter()
            .map(|q| ((q.x as i32 + q.w as i32 - 1) - 100).max(100 - q.x as i32))
            .max()
            .unwrap_or(0);
        let halos = halos
            .iter()
            .map(|h| ((h.x as i32 + h.w as i32 - 1) - 100).max(100 - h.x as i32))
            .max()
            .unwrap_or(0);
        quads.max(halos)
    };
    for (style, dark) in [
        (GlowStyle::Phaser, true),
        (GlowStyle::Phaser, false),
        (GlowStyle::Fire, true),
    ] {
        let hop = extent(style, dark, 1.0);
        let leap = extent(style, dark, 3.5);
        assert!(hop > 0, "{style:?} dark={dark}: the hop ring draws");
        assert!(
            leap as f32 >= hop as f32 * 1.8,
            "{style:?} dark={dark}: a capped-impact ring reaches {leap} px, a hop {hop} px"
        );
    }
}

#[test]
fn landing_ring_bars_partition_the_square_outline() {
    let g = geom();
    let born = Instant::now();
    let c = cfg(GlowStyle::Phaser, true);
    // Sweep the ring's life so every expansion size s is exercised.
    for ms in [10u64, 60, 120, 170] {
        let ring = Ring {
            cx: 100.0,
            cy: 40.0,
            born,
            life: 0.30,
            scale: 1.0,
        };
        let glow = CursorGlow {
            ring: Some(ring),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(ms),
            &c,
            g,
            &mut out,
            &mut halos,
        );
        assert!(!out.is_empty(), "the dark square outline draws at {ms}ms");
        let mut owners = std::collections::HashMap::new();
        for q in &out {
            for y in q.y..q.y + q.h {
                for x in q.x..q.x + q.w {
                    *owners.entry((y, x)).or_insert(0u32) += 1;
                }
            }
        }
        assert!(
            owners.values().all(|&n| n == 1),
            "a ring pixel is owned by two bars at {ms}ms (the corner rivet): {:?}",
            owners
                .iter()
                .filter(|&(_, &n)| n > 1)
                .take(4)
                .collect::<Vec<_>>()
        );
        // Closure: the outline's extreme rows and columns are still lit
        // (clamped to the effects box, which the fixture's centre avoids).
        let min_y = out.iter().map(|q| q.y).min().unwrap();
        let max_y = out.iter().map(|q| q.y + q.h - 1).max().unwrap();
        let min_x = out.iter().map(|q| q.x).min().unwrap();
        let max_x = out.iter().map(|q| q.x + q.w - 1).max().unwrap();
        for (y, x) in [
            (min_y, min_x),
            (min_y, max_x),
            (max_y, min_x),
            (max_y, max_x),
        ] {
            assert!(
                owners.contains_key(&(y, x)),
                "the outline's {y},{x} corner went dark at {ms}ms — a bar gap"
            );
        }
    }
}

/// THE HALO-CENTRE LAW: a halo whose centre lies past the effects box
/// keeps its TRUE centre in the emitted bands (the renderer's falloff must
/// read ~0 on the surviving sliver), and a centre that cannot be
/// represented (negative) culls the halo. The old `.clamp(0, br)` moved an
/// off-right centre ONTO the edge, so a particle sliding off the grid
/// flashed at up to ~96% of peak on its last visible pixels (2026-09-01
/// audit).
#[test]
fn halo_centres_past_the_box_stay_true_and_negative_centres_cull() {
    let g = geom(); // box [0,320) x [0,96)
    let mut out = Vec::new();
    push_halo(&mut out, g, 322.0, 48.0, 5.0, 5.0, 0x0020_2020);
    assert!(!out.is_empty(), "the sliver band still draws");
    assert!(
        out.iter().all(|h| h.cx == 322),
        "the falloff centre is the TRUE centre, not the clamped edge: {:?}",
        out.iter().map(|h| h.cx).collect::<Vec<_>>()
    );
    let mut neg = Vec::new();
    push_halo(&mut neg, g, -2.0, 48.0, 5.0, 5.0, 0x0020_2020);
    assert!(neg.is_empty(), "an unrepresentable centre culls the halo");

    let mut over = Vec::new();
    push_halo_over(&mut over, g, 322.0, 48.0, 5.0, 5.0, 0x0033_3333, 255);
    assert!(
        over.iter().all(|h| h.cx == 322) && !over.is_empty(),
        "the Over veil follows the same centre law"
    );
    let mut over_neg = Vec::new();
    push_halo_over(&mut over_neg, g, 48.0, -2.0, 5.0, 5.0, 0x0033_3333, 255);
    assert!(over_neg.is_empty(), "…including the cull side");
}

// `glide_star_forks_dark_additive_and_light_source_over` STOOD HERE and is
// deleted (step 13). The fork it pinned — additive rainbow light on dark,
// a darkened source-over streak on white — is the STREAK's law, not the
// glide's, and the glide draws the streak now. It is pinned once, on the one
// mark, by `rainbow_jump_zoom_light_theme_forks_to_source_over` and
// `the_light_ground_grows_the_jump_without_breaching_its_ink_cap`.

/// A cold Ctrl-A timestamp is not a jump witness. With no candidate and no
/// ribbon, the relocation births neither terminus scatter nor a landing
/// starburst — the audited stray-rainbow family.
#[test]
fn cold_navigation_timestamp_scatters_nothing() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 30)), t0, &c, g, &mut out); // seed — cursor stays COLD
    // (rainbow.disp left at its default 0.0 — no typing momentum, no ribbon.)
    glow.note_navigation(t0);
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // Ctrl-A leap on a cold cursor
    assert!(
        glow.particles.is_empty(),
        "a cold-cursor jump dissolves no phantom terminus (finding 3)"
    );
    // THE STARBURST FORKS ON DISTANCE (2026-08-29). The old law here was
    // "a cold jump throws no stars — nothing was typed", and its docstring
    // named the defect it guarded: at a 2-cell floor every arrow-key
    // HISTORY RECALL on an untouched prompt threw stars and rang the
    // chime — the owner's "stray pieces of rainbow". That was never about
    // jumps; it was about the FLOOR. The owner then asked for the jump
    // effects back ("I LOVED the jump effects … BRING THEM BACK"), so a
    // cold Home/End — a REAL jump, past `RAINBOW_JUMP_MIN` — celebrates,
    // and this 30-cell Ctrl-A is one. The recall case is pinned right
    // after: a one-row cold move still throws nothing.
    assert!(
        glow.v2_status().is_some_and(|s| s.meteors >= 1),
        "a cold-cursor JUMP (30 cells) flies its meteor — punctuation, \
         not stray rainbow"
    );
    let mut recall = CursorGlow::default();
    recall.tick(Some((2, 3)), t0, &c, g, &mut out);
    recall.note_navigation(t0);
    recall.tick(Some((2, 0)), t0, &c, g, &mut out); // a 3-cell cold nudge
    assert!(
        recall.v2_status().is_none_or(|s| s.meteors == 0),
        "a cold one-row RECALL throws no landing stars — this is the stray \
         rainbow the floor exists to stop"
    );
}
