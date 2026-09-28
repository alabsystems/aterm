// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The comet style.

use super::*;

// `the_supernova_rolls_only_for_a_real_leap` STOOD HERE. It pinned the
// three properties that made a random escalation safe: never below the
// distance floor, never under reduced motion, and an observed rate that
// tracked the declared 1-in-6 chance. Its subject is deleted (step 5) and
// the properties are now unrepresentable rather than checked — there is
// no roll, so there is no floor to fall through and no rate to honour.

// `a_supernova_landing_throws_a_rainbow_glitter_shell` STOOD HERE, pinning
// that a nova landing threw all 28 grains, that they left in every
// direction, and that an ordinary landing grew none. The shell is deleted
// with the tier that owned it (step 5). What the landing must still do —
// mark the arrival with one expanding spectral stroke — is pinned by
// `every_landing_rings_a_rainbow_past_its_own_debris` below.

/// `comet` and `beam` each graduated to their own variant; `lumen` stays
/// the plain Lumen tracer (raw-string routing in the GUI resolver untouched).
#[test]
fn comet_parses_apart_from_beam_and_lumen() {
    assert_eq!(GlowStyle::parse("comet"), GlowStyle::Comet);
    assert_eq!(GlowStyle::parse(" Comet "), GlowStyle::Comet);
    assert_eq!(GlowStyle::parse("beam"), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse("lumen"), GlowStyle::Lumen);
    assert!(
        GlowStyle::Comet.has_particles(),
        "comet sheds debris glitter"
    );
    assert!(style_has_beam("comet"), "the comet keeps its beam");
}

/// The icy tail ramp: a dusty dim tail brightening along the path, the head
/// flash-freezing toward white — but never reaching pure white (the nucleus
/// cursor's additive coma must stay the brightest point).
#[test]
fn comet_tail_freezes_white_toward_the_head() {
    let (color, accent) = (0x0050_FA7B, 0x007A_A2F7);
    let bright = |c: u32| ((c >> 16) & 0xff) as i32 + ((c >> 8) & 0xff) as i32 + (c & 0xff) as i32;
    let tail = style_comet_color(GlowStyle::Comet, color, accent, 0.0, 0.0);
    let mid = style_comet_color(GlowStyle::Comet, color, accent, 0.0, 0.6);
    let head = style_comet_color(GlowStyle::Comet, color, accent, 0.0, 1.0);
    assert!(
        bright(tail) < bright(mid) && bright(mid) < bright(head),
        "monotonic dust→ice brightening ({tail:#08x} < {mid:#08x} < {head:#08x})"
    );
    assert!(
        bright(tail) < bright(accent),
        "the far tail is DUSTY — dimmer than the raw accent"
    );
    // The head is whiter than the base hue (channels converge toward each
    // other) yet stays short of pure white.
    let spread = |c: u32| {
        let (r, g, b) = (
            ((c >> 16) & 0xff) as i32,
            ((c >> 8) & 0xff) as i32,
            (c & 0xff) as i32,
        );
        r.max(g).max(b) - r.min(g).min(b)
    };
    assert!(
        spread(head) < spread(color),
        "head whitens off the base hue"
    );
    assert!(bright(head) < 3 * 255, "head stays below pure white");
}

/// A comet move sheds debris grains; the plain Lumen tracer sheds none.
/// Grains HANG (near-zero gravity) and drift BEHIND the motion — on a
/// rightward jump every grain's x-velocity points back along the tail.
#[test]
fn comet_sheds_hanging_debris_behind_the_motion() {
    let g = geom();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut lumen = CursorGlow::default();
    lumen.tick(Some((2, 0)), t0, &cfg(GlowStyle::Lumen, true), g, &mut out);
    lumen.note_synthetic_move(t0);
    lumen.tick(Some((2, 30)), t0, &cfg(GlowStyle::Lumen, true), g, &mut out);
    assert!(lumen.particles.is_empty(), "lumen stays debris-free");

    let mut comet = CursorGlow::default();
    comet.tick(Some((2, 0)), t0, &cfg(GlowStyle::Comet, true), g, &mut out);
    comet.note_synthetic_move(t0);
    comet.tick(Some((2, 30)), t0, &cfg(GlowStyle::Comet, true), g, &mut out);
    assert!(
        !comet.particles.is_empty(),
        "a comet jump sheds a meteor train"
    );
    let cell = g.ch as f32;
    let (x0, x1) = ((0.5) * g.cw as f32, (30.5) * g.cw as f32);
    for p in &comet.particles {
        assert!(
            p.gy <= 0.3 * cell,
            "dust settles, it doesn't plummet: {}",
            p.gy
        );
        assert!(
            p.vx < 0.0,
            "debris drifts BEHIND a rightward jump: {}",
            p.vx
        );
        assert!(
            p.x0 >= x0 - g.cw as f32 && p.x0 <= x1 + g.cw as f32,
            "grains are strewn along the swept vector, got x0={}",
            p.x0
        );
    }
}

/// A real-window geometry: pad + tab strip/titlebar chrome put the grid
/// interior at origin (56, 56). The identity-law geoms all use origin 0
/// (where grid-relative and window-absolute coincide), so only a chrome
/// origin can expose a producer mixing the two coordinate spaces.
fn chrome_geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 56,
        origin_y: 56,
        win_w: 384, // 56 + 40·8 + 8
        win_h: 160, // 56 + 6·16 + 8
        head: 0,
    }
}

/// The live-cursor bridge vertex is WINDOW-ABSOLUTE like every other
/// comet sample (the `center` closure): with real chrome the bridge must
/// attach at `origin_x + (col + head_dx)·cw`. The audited regression
/// rebuilt the bridge x grid-relative, which at this origin fails the
/// ±1.5-cell adjacency gate outright — the beam silently detaches from
/// the cursor for every beam style in every real window.
#[test]
fn comet_bridge_attaches_window_absolute() {
    let g = chrome_geom();
    let mut c = cfg(GlowStyle::Comet, true);
    c.head_dx = 0.9; // an attach point clearly right of the cell centre
    let mut glow = CursorGlow::default();
    let mut t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 10)), t, &c, g, &mut out);
    for col in 11..=14u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    // Re-emit the beam stream alone (no crown/ring/particle quads).
    let mut beam = Vec::new();
    let mut halos = Vec::new();
    glow.emit_comet(t, &c, g, Some((2, 14)), &mut beam, &mut halos);
    assert!(!beam.is_empty(), "a typing run draws a beam");
    let reach = beam
        .iter()
        .map(|q| i32::from(q.x) + i32::from(q.w))
        .max()
        .unwrap();
    // Window-absolute attach: 56 + (14 + 0.9)·8 = 175.2 px. The head
    // SAMPLE (cell 13's centre) reaches only 56 + 13.5·8 = 164, so a
    // reach ≥ 172 proves the bridge segment exists AND lands at head_dx;
    // the grid-relative x (119.2) sat 44.8 px off and emitted nothing.
    assert!(
        reach >= 172,
        "bridge reaches the window-absolute head_dx attach point: {reach}"
    );
    assert!(
        beam.iter().all(|q| i32::from(q.x) >= i32::from(g.origin_x)),
        "every beam quad stays inside the window-absolute effects box"
    );
}

/// Comet debris is born WINDOW-ABSOLUTE: jump grains sit ON the true leap
/// vector — never on a phantom vector starting `origin` px up-left — and
/// typing debris drifts BEHIND the one-cell motion with jitter-only vy.
/// The audited grid-relative fx/fy skewed the drift by (origin_x,
/// origin_y) on every spawn, so in a real window the shed dust always
/// streamed toward the window's top-left instead of trailing the motion.
#[test]
fn comet_debris_born_window_absolute_with_chrome_origin() {
    let g = chrome_geom();
    let c = cfg(GlowStyle::Comet, true);
    // A leap: grains strewn along the WINDOW-ABSOLUTE vector.
    let mut glow = CursorGlow::default();
    let t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((0, 2)), t, &c, g, &mut out);
    glow.note_synthetic_move(t + Duration::from_millis(16));
    glow.tick(
        Some((5, 20)),
        t + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert!(!glow.particles.is_empty(), "a leap sheds a meteor train");
    let (fx, fy) = (56.0 + 2.5 * 8.0, 56.0 + 0.5 * 16.0);
    let (ox, oy) = (56.0 + 20.5 * 8.0, 56.0 + 5.5 * 16.0);
    for p in &glow.particles {
        let u = (p.x0 - fx) / (ox - fx);
        assert!(
            (-1e-3..=1.0 + 1e-3).contains(&u) && (p.y0 - (fy + (oy - fy) * u)).abs() < 1e-2,
            "grain ON the true leap vector, got ({}, {})",
            p.x0,
            p.y0
        );
    }
    // Typing: the drift vector is the true one-cell motion — leftward
    // along the row, vy pure dispersion jitter (±0.175·cell). The origin
    // skew instead pointed it up-left and made the stationary ml≈0
    // dispersion branch unreachable.
    let mut glow = CursorGlow::default();
    let mut t = Instant::now();
    glow.tick(Some((2, 10)), t, &c, g, &mut out);
    for col in 11..=16u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    let cell = g.ch as f32;
    assert!(!glow.particles.is_empty(), "typing outgasses grains");
    for p in &glow.particles {
        assert!(p.vx < 0.0, "dust falls back along the tail: vx {}", p.vx);
        assert!(
            p.vy.abs() <= 0.175 * cell + 1e-3,
            "vy is dispersion jitter only, no origin skew: {}",
            p.vy
        );
    }
}

/// Beam stardust jump births are WINDOW-ABSOLUTE: every mote lies inside
/// the ±0.4-cell sleeve of the true leap vector. The grid-relative
/// departure point put early motes above/left of the effects box (silently
/// clipped by push_rect) and the rest on cells the rod never crossed.
#[test]
fn beam_stardust_strewn_along_window_absolute_leap() {
    let g = chrome_geom();
    let c = cfg(GlowStyle::Beam, true);
    let mut glow = CursorGlow::default();
    let t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((0, 2)), t, &c, g, &mut out);
    glow.note_synthetic_move(t + Duration::from_millis(16));
    glow.tick(
        Some((5, 20)),
        t + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert!(!glow.particles.is_empty(), "a warp leap stirs stardust");
    let (fx, fy) = (56.0 + 2.5 * 8.0, 56.0 + 0.5 * 16.0);
    let (ox, oy) = (56.0 + 20.5 * 8.0, 56.0 + 5.5 * 16.0);
    let sleeve = 0.4 * g.ch as f32 + 1e-3;
    for p in &glow.particles {
        let u = (p.x0 - fx) / (ox - fx);
        assert!(
            (-1e-3..=1.0 + 1e-3).contains(&u),
            "mote within the leap span: {}",
            p.x0
        );
        assert!(
            (p.y0 - (fy + (oy - fy) * u)).abs() <= sleeve,
            "mote inside the leap sleeve, got ({}, {})",
            p.x0,
            p.y0
        );
        assert!(
            p.x0 >= 56.0 && p.y0 >= 56.0,
            "no mote above/left of the grid interior: ({}, {})",
            p.x0,
            p.y0
        );
    }
}

// `the_glide_shooting_star_head_is_dealt_like_the_rest` STOOD HERE and is
// deleted (step 13). It governed the POPULATION SHAPE of the fast-glide
// star's white twinkle HEAD — one plus or one mote per star, dealt against
// the family's star kit so a fast sweep could not lay a row of full-size
// crosses. That head belonged to a mark that no longer exists: a glide
// draws the ZOOM streak now, whose leading light is the meteor NUCLEUS
// (`RAINBOW_METEOR_NUCLEUS_*`), a different mark with its own pins. Deleting
// a proof about a deleted subject is not the same as dropping a claim — the
// claim it made about the star kit's deal is still held, for the marks that
// still deal, by `stardust_is_the_body_the_plus_is_the_accent`.

/// The comet's emitted light (beam + coma crown + twinkling glitter) honours
/// the shared quad invariants and decays to exactly empty.
#[test]
fn comet_light_respects_invariants_and_decays() {
    let g = geom();
    let c = cfg(GlowStyle::Comet, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((2, 30)), t0, &c, g, &mut out);
    out.clear();
    let fp = glow.tick(
        Some((2, 30)),
        t0 + Duration::from_millis(60),
        &c,
        g,
        &mut out,
    );
    assert_ne!(fp, 0);
    assert!(!out.is_empty(), "beam + glitter render while alive");
    assert_invariants(&out, g);
    let fp = glow.tick(Some((2, 30)), t0 + Duration::from_secs(4), &c, g, &mut out);
    assert!(out.is_empty(), "comet light fully decayed");
    assert_eq!(fp, 0);
    assert!(!glow.is_active());
}
