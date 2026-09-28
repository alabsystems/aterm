// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The water style.

use super::*;

/// Water must stay in ocean hues: every emitted quad (comet, crown, ripple
/// ring, droplets) keeps blue at least as strong as red, across spawn and
/// mid-flight frames.
#[test]
fn water_stays_in_ocean_hues() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((1, 38)), t0, &c, g, &mut out); // jump: comet + crown + ring + droplets
    let mut all: Vec<GlowQuad> = out.clone();
    all.extend_from_slice(glow.under_quads());
    for ms in [40u64, 120, 240] {
        glow.tick(
            Some((1, 38)),
            t0 + Duration::from_millis(ms),
            &c,
            g,
            &mut out,
        );
        all.extend(out.iter().copied());
        all.extend_from_slice(glow.under_quads());
    }
    assert!(!all.is_empty(), "water jump must emit light");
    for q in &all {
        let (r, b) = ((q.color >> 16) & 0xff, q.color & 0xff);
        assert!(
            b >= r,
            "water quad drifted out of ocean hues: {:06x}",
            q.color
        );
    }
}

/// The water dynamic range: relaxed typing is a TRICKLE, a sustained fast
/// run is the TORPEDO — far more light and far more droplet quads at the
/// same config.
#[test]
fn water_scales_from_trickle_to_torpedo() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // Relaxed: one single-cell advance every 500 ms.
    let mut slow = CursorGlow::default();
    let mut t = t0;
    slow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=12u16 {
        t += Duration::from_millis(500);
        slow.note_synthetic_typed(t, 1);
        slow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    let slow_quads = out.len() + slow.under_quads().len();
    let slow_lum = lum(&out) + lum(slow.under_quads());
    assert!(slow_lum > 0, "a trickle still glows");

    // Sustained fast: one advance every 50 ms.
    let mut fast = CursorGlow::default();
    let mut t = t0;
    fast.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=12u16 {
        t += Duration::from_millis(50);
        fast.note_synthetic_typed(t, 1);
        fast.tick(Some((2, i)), t, &c, g, &mut out);
    }
    let fast_quads = out.len() + fast.under_quads().len();
    let fast_lum = lum(&out) + lum(fast.under_quads());
    assert!(
        fast_quads > slow_quads * 2,
        "the torpedo must shed far more droplets: fast {fast_quads} vs slow {slow_quads} quads"
    );
    assert!(
        fast_lum > slow_lum * 3,
        "the torpedo must vastly outshine the trickle: fast {fast_lum} vs slow {slow_lum}"
    );
}

/// Slow typing must DRIP: after the crown and the typing wake have decayed
/// (350 ms > CROWN_MS and any typing spark's life), the only light left is
/// droplets — and at least one must have fallen BELOW the typed row.
#[test]
fn water_drips_fall_below_the_typed_row() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=4u16 {
        t += Duration::from_millis(600);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    glow.tick(
        Some((2, 4)),
        t + Duration::from_millis(350),
        &c,
        g,
        &mut out,
    );
    assert!(!out.is_empty(), "a drip should still be falling");
    assert!(
        out.iter().any(|q| q.row > 2),
        "a droplet must fall below the typed row (drip): rows {:?}",
        out.iter().map(|q| q.row).collect::<Vec<_>>()
    );
}

/// Water owns a connected, travelling curve even though its shared laser beam
/// stays disabled. Sustained typing raises heat, bends the wake, and keeps a
/// long contiguous pixel run without turning it into a full-cell slab.
#[test]
fn water_emits_a_smooth_wavy_wake() {
    let g = geom();
    let mut c = cfg(GlowStyle::Water, true);
    c.intensity = 1.0;
    c.radius = 0.0;
    c.ring = false;
    assert!(!c.beam, "water must keep the shared laser beam disabled");

    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((2, 1)), t, &c, g, &mut out);
    for col in 2..=18u16 {
        t += Duration::from_millis(20);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    assert!(glow.heat > 0.9, "fast run reaches torpedo momentum");
    // Isolate the wake geometry from its supporting droplets.
    glow.particles.clear();
    glow.tick(Some((2, 18)), t, &c, g, &mut out);
    let wake_out = glow.under_quads();
    assert_invariants(wake_out, g);
    assert!(wake_out.len() <= CursorGlow::MAX_QUADS);

    let wake: Vec<&GlowQuad> = wake_out.iter().filter(|q| q.row == 2).collect();
    assert!(
        wake.len() >= 20,
        "hot water run emits a resolved curved wake"
    );
    let ys: std::collections::BTreeSet<u16> = wake.iter().map(|q| q.y).collect();
    assert!(
        ys.len() >= 3,
        "wake must visibly travel vertically instead of forming a rigid beam: {ys:?}"
    );
    let xs: std::collections::BTreeSet<u16> = wake
        .iter()
        .flat_map(|q| q.x..q.x.saturating_add(q.w))
        .collect();
    let mut longest = 0usize;
    let mut run = 0usize;
    let mut previous = None;
    for x in xs {
        run = if previous.is_some_and(|p| x == p + 1) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        previous = Some(x);
    }
    assert!(
        longest >= g.cw * 4,
        "wake must contain a coherent connected run, got {longest}px"
    );
    for q in wake {
        let (r, b) = ((q.color >> 16) & 0xff, q.color & 0xff);
        assert!(b >= r, "water wake remains in ocean hues: {q:?}");
    }
}

/// A causally licensed Home/End-sized move gets Water's own full-vector
/// crest even when the configured typing tail is one cell. Navigation is
/// still calm: no heat, splash particles, or landing ring; a one-cell arrow
/// remains dark because this is jump feedback rather than an arrow smear.
#[test]
fn water_navigation_jump_spans_the_vector_without_celebration() {
    let g = geom();
    let mut c = cfg(GlowStyle::Water, true);
    c.length = 1;
    c.radius = 0.0;
    c.ring = true;
    let t0 = Instant::now();
    let mut out = Vec::new();

    let mut jump = CursorGlow::default();
    jump.tick(Some((2, 3)), t0, &c, g, &mut out);
    let at = t0 + Duration::from_millis(5);
    jump.note_navigation(at);
    jump.tick(Some((2, 36)), at, &c, g, &mut out);
    assert_eq!(jump.heat, 0.0, "navigation earns no Water heat");
    assert!(
        jump.ring.is_none(),
        "navigation lands without a ripple ring"
    );
    assert!(
        jump.particles.is_empty(),
        "navigation lays no splash population"
    );
    assert!(
        jump.sparks.len() >= 30,
        "the jump keeps its vector despite length=1: {} samples",
        jump.sparks.len()
    );
    let middle_x = 20 * g.cw + g.cw / 2;
    assert!(
        jump.under_quads().iter().any(|q| {
            q.row == 2
                && usize::from(q.x) <= middle_x
                && usize::from(q.x) + usize::from(q.w) > middle_x
        }),
        "the fluid streak must cross an intermediate cell"
    );

    let mut step = CursorGlow::default();
    step.tick(Some((2, 3)), t0, &c, g, &mut out);
    step.note_navigation(at);
    step.tick(Some((2, 4)), at, &c, g, &mut out);
    assert!(
        step.under_quads().is_empty(),
        "one-cell arrow navigation is not a jump streak"
    );

    let mut raw = CursorGlow::default();
    raw.tick(Some((2, 3)), t0, &c, g, &mut out);
    raw.tick(Some((2, 36)), at, &c, g, &mut out);
    assert!(
        raw.under_quads().is_empty(),
        "an unlicensed program relocation remains dark"
    );
}

/// Codex-style whole-row repaint choreography can revisit the same corridor
/// many times before Water's previous jump life expires. The renderer must
/// see one newest owner per cell, not hundreds of additive reflections, and
/// the broad wake must remain below opaque glyph ink in the real paint order.
#[test]
fn water_repaint_corridor_deduplicates_and_cannot_cover_text() {
    let g = geom();
    let mut c = cfg(GlowStyle::Water, true);
    c.duration = Duration::from_secs(2);
    c.intensity = 1.0;
    c.radius = 0.0;
    c.ring = false;
    let t0 = Instant::now();
    let mut t = t0;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let mut cursor = (2, 4);
    glow.tick(Some(cursor), t, &c, g, &mut out);
    for i in 0usize..24 {
        t += Duration::from_millis(3);
        cursor = if i.is_multiple_of(2) { (2, 35) } else { (2, 4) };
        glow.note_navigation(t);
        glow.tick(Some(cursor), t, &c, g, &mut out);
    }

    let unique: std::collections::BTreeSet<_> =
        glow.water_samples.iter().map(|s| (s.row, s.col)).collect();
    assert_eq!(
        unique.len(),
        glow.water_samples.len(),
        "each visible coordinate has exactly one Water owner"
    );
    // Uniqueness alone would also accept an oldest-owner traversal. Pick a
    // coordinate whose alternating passes carry visibly different path
    // positions and prove the selected color belongs to its NEWEST resident.
    let owner_cell = (2, 10);
    let selected = glow
        .water_samples
        .iter()
        .find(|s| (s.row, s.col) == owner_cell)
        .expect("the repaint corridor contains the ownership witness");
    let mut residents = glow
        .sparks
        .iter()
        .rev()
        .filter(|s| (s.row, s.col) == owner_cell);
    let newest = residents.next().expect("newest resident exists");
    let previous = residents.next().expect("a duplicate resident exists");
    let heat = glow.heat.clamp(0.0, 1.0);
    let newest_color = water_ramp(0.58 + 0.30 * newest.pos + 0.12 * heat);
    let previous_color = water_ramp(0.58 + 0.30 * previous.pos + 0.12 * heat);
    assert_ne!(
        newest_color, previous_color,
        "fixture must distinguish newest from the previous crossing"
    );
    assert_eq!(
        selected.surface.color, newest_color,
        "the newest visible resident must own the rendered cell"
    );
    assert!(
        glow.sparks.len() > glow.water_samples.len() * 8,
        "fixture must contain a real repaint pile: {} residents vs {} owners",
        glow.sparks.len(),
        glow.water_samples.len()
    );
    assert!(
        (25..=33).contains(&glow.water_samples.len()),
        "the visible set is corridor-sized, not repaint-count-sized: {}",
        glow.water_samples.len()
    );
    assert!(
        !glow.under_quads().is_empty() && glow.under_quads().len() < 2_048,
        "deduplicated corridor stays a bounded visible wake: {} quads",
        glow.under_quads().len()
    );

    // Non-vacuous overlap: undertow + crest AA really do hit common pixels.
    let mut hits = vec![0u16; usize::from(g.win_w) * usize::from(g.win_h)];
    for q in glow.under_quads() {
        for y in usize::from(q.y)..usize::from(q.y + q.h) {
            for x in usize::from(q.x)..usize::from(q.x + q.w) {
                hits[y * usize::from(g.win_w) + x] += 1;
            }
        }
    }
    assert!(hits.into_iter().max().unwrap_or(0) >= 2);

    // Renderer-exact order: under light first, then ink replacement. Every
    // stamped glyph byte must therefore remain exactly the theme's byte.
    let mut frame = Glass::new(g, c.theme_bg);
    let mut bare = Glass::new(g, c.theme_bg);
    frame.add_quads(glow.under_quads());
    for col in 4..=35u16 {
        frame.stamp_ink(2, col, c.theme_fg);
        bare.stamp_ink(2, col, c.theme_fg);
    }
    let mut lit_ground = 0usize;
    for y in 0..i32::from(g.win_h) {
        for x in 0..i32::from(g.win_w) {
            let on = frame.at(y, x);
            if bare.at(y, x) == c.theme_fg {
                assert_eq!(on, c.theme_fg, "Water changed glyph ink at ({y}, {x})");
            } else if on != c.theme_bg {
                lit_ground += 1;
                assert!(frame.level(y, x) < 255, "Water clipped at ({y}, {x})");
            }
        }
    }
    assert!(lit_ground > 100, "the protected frame still visibly glows");
}

/// A jump's reflection does not remain a rigid strip until one mass expiry:
/// it transfers into sparse beads, those beads accelerate downward, and the
/// remaining light is already faint before exact idle-zero cleanup.
#[test]
fn water_jump_reflection_drips_then_fades_smoothly_to_zero() {
    let g = geom();
    let mut c = cfg(GlowStyle::Water, true);
    c.duration = Duration::from_millis(700);
    c.intensity = 1.0;
    c.radius = 0.0;
    c.ring = false;
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((1, 3)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((1, 36)), t0, &c, g, &mut out);
    glow.particles.clear();

    let light_mass = |glow: &CursorGlow| {
        glow.water_samples
            .iter()
            .map(|s| u64::from(s.surface.cov) + u64::from(s.drip_cov))
            .sum::<u64>()
    };
    let bead_bottoms = |glow: &CursorGlow| {
        glow.water_samples
            .iter()
            .filter(|s| s.drip_cov > 0)
            .map(|s| ((s.row, s.col), s.drip_y + s.drip_h * 0.5))
            .collect::<std::collections::BTreeMap<_, _>>()
    };

    glow.tick(
        Some((1, 36)),
        t0 + Duration::from_millis(250),
        &c,
        g,
        &mut out,
    );
    let early_mass = light_mass(&glow);
    let early_beads = bead_bottoms(&glow);
    let early_quads = glow.under_quads().len();

    glow.tick(
        Some((1, 36)),
        t0 + Duration::from_millis(420),
        &c,
        g,
        &mut out,
    );
    let mid_mass = light_mass(&glow);
    let mid_beads = bead_bottoms(&glow);
    let mid_quads = glow.under_quads().len();

    glow.tick(
        Some((1, 36)),
        t0 + Duration::from_millis(630),
        &c,
        g,
        &mut out,
    );
    let late_mass = light_mass(&glow);
    let late_beads = bead_bottoms(&glow);
    let late_quads = glow.under_quads().len();
    assert!(early_quads > 0 && mid_quads > 0 && late_quads > 0);
    assert!(
        glow.under_quads().iter().any(|q| q.row > 1),
        "a late bead must actually render below the source row"
    );
    let (tracked_cell, early_bottom, mid_bottom, late_bottom) = late_beads
        .iter()
        .find_map(|(&cell, &late)| {
            Some((cell, *early_beads.get(&cell)?, *mid_beads.get(&cell)?, late))
        })
        .expect("one dealt bead remains visible across all three samples");
    assert!(
        early_bottom < mid_bottom && mid_bottom < late_bottom,
        "gravity must carry bead {tracked_cell:?} down: \
         {early_bottom} -> {mid_bottom} -> {late_bottom}"
    );
    assert!(
        early_mass > mid_mass && mid_mass > late_mass && late_mass * 8 < early_mass,
        "light must ease down before expiry: {early_mass} -> {mid_mass} -> {late_mass}"
    );

    glow.tick(
        Some((1, 36)),
        t0 + Duration::from_millis(701),
        &c,
        g,
        &mut out,
    );
    assert!(
        glow.under_quads().is_empty() && glow.water_samples.is_empty(),
        "the eased residue reaches exact idle zero"
    );
}
