// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fire style: meteors, the Emberforge thermal laws, the forge field.

use super::*;

/// Flare law: a real cursor jump slams Fire's flare to full, and it cools on its
/// own tau, gone within about a second, so the burst reads as an event.
#[test]
fn fire_jump_flares_and_cools() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((3, 30)), t0, &c, g, &mut out);
    assert!(glow.flare < 0.5, "launch does not erupt ({})", glow.flare);
    assert_eq!(glow.fire_meteors.len(), 1, "the jump flies one meteor");
    assert!(!out.is_empty(), "the meteor emits light in flight");
    assert_invariants(&out, g);
    let life = glow.fire_meteors[0].life;
    glow.tick(
        Some((3, 30)),
        t0 + Duration::from_secs_f32(life * 0.8),
        &c,
        g,
        &mut out,
    );
    assert!(glow.flare > 0.99, "the strike ignites the flare");
    glow.tick(
        Some((3, 30)),
        t0 + Duration::from_secs_f32(life * 0.8 + 1.0),
        &c,
        g,
        &mut out,
    );
    assert!(
        glow.flare < 0.15,
        "flare cools after the burst: {}",
        glow.flare
    );
}

/// A move down one row anchors no fire beyond the single wake spark at the landing. A typing wrap
/// (from the last column) flies no inter-line meteor, which would sweep the line on every wrap; a
/// deliberate jump from a short column still flies one (the positive control).
#[test]
fn typing_wrap_no_meteor_deliberate_enter_still_meteors() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    let mut wrapper = CursorGlow::default();
    wrapper.tick(Some((4, 39)), t0, &c, g, &mut out);
    wrapper.note_synthetic_typed(t0, 1);
    wrapper.tick(Some((5, 0)), t0, &c, g, &mut out);
    assert!(
        wrapper.fire_meteors.is_empty(),
        "a typing wrap flies no meteor (got {})",
        wrapper.fire_meteors.len()
    );
    assert!(
        !wrapper.sparks.iter().any(|s| s.row == 5 && s.col >= 1),
        "no wrap smear right of the landing: {:?}",
        wrapper
            .sparks
            .iter()
            .filter(|s| s.row == 5)
            .map(|s| s.col)
            .collect::<Vec<_>>()
    );
    wrapper.tick(
        Some((5, 0)),
        t0 + Duration::from_millis(1000),
        &c,
        g,
        &mut out,
    );
    let right_lum: u64 = out
        .iter()
        .filter(|q| q.row == 5 && q.x as usize > 3 * g.cw)
        .map(|q| (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff)) as u64)
        .sum();
    assert_eq!(right_lum, 0, "landing row right of the cursor is quiet");
    assert_invariants(&out, g);

    let mut enterer = CursorGlow::default();
    enterer.tick(Some((2, 5)), t0, &c, g, &mut out);
    enterer.note_synthetic_move(t0);
    enterer.tick(Some((3, 0)), t0, &c, g, &mut out);
    assert_eq!(
        enterer.fire_meteors.len(),
        1,
        "a synthetic short-line jump still flies a meteor"
    );
    assert!(
        enterer.sparks.is_empty(),
        "a deliberate row-change anchors no per-cell fire"
    );
}

/// One-shot causality: a scripted jump may fly one meteor, and an unlicensed
/// second repaint hop neither retargets it, mints a second, nor kills it.
#[test]
fn fire_meteor_survives_an_unlicensed_second_hop_without_retargeting() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((6, 11)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0 + Duration::from_millis(10));
    glow.tick(
        Some((6, 0)),
        t0 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert_eq!(glow.fire_meteors.len(), 1, "the admitted hop flies once");
    let flying = (glow.fire_meteors[0].x0, glow.fire_meteors[0].y0);
    let spawned = glow.spawns();
    glow.tick(
        Some((5, 44)),
        t0 + Duration::from_millis(30),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.spawns(),
        spawned,
        "an unlicensed repaint hop mints nothing"
    );
    assert_eq!(
        glow.fire_meteors.len(),
        1,
        "…and mints no second meteor either"
    );
    assert_eq!(
        (glow.fire_meteors[0].x0, glow.fire_meteors[0].y0),
        flying,
        "an unlicensed repaint cannot RETARGET admitted light…"
    );
    assert!(
        !glow.fire_meteors.is_empty(),
        "…and it cannot destroy it either — the meteor flies the vector it earned"
    );
}

/// Window-space effects layer: with a real head band (origin_y = pad 8 + head 40
/// = 48) a row-0 blaze climbs above the grid into the chrome band (patches with
/// `y < origin_y`, tagged damage row 0) while the flame keeps its natural
/// root in the row's ascender band (`base_y == origin_y + ch ·
/// FIRE_DARK_ROOT`), and the engulf gate keys only real
/// grid rows (no u16 wrap from a negative band).
#[test]
fn fire_rises_into_the_chrome_band() {
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 8,
        origin_y: 48,
        win_w: 336,
        win_h: 152,
        head: 40,
    };
    let c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((0, 0)), t, &c, g, &mut out);
    for i in 1..=14u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((0, i)), t, &c, g, &mut out);
    }
    assert!(!glow.patches().is_empty(), "a row-0 blaze emits fire");
    let in_band: Vec<_> = glow
        .patches()
        .iter()
        .filter(|q| (q.y as i32) < g.origin_y as i32)
        .collect();
    assert!(
        !in_band.is_empty(),
        "full-heat row-0 fire rises into the chrome band (y < origin_y)"
    );
    for q in &in_band {
        assert_eq!(q.row, 0, "above-grid bands carry damage-hint row 0: {q:?}");
    }
    let root = g.origin_y + (g.ch as f32 * CursorGlow::FIRE_DARK_ROOT).max(2.0) as u16;
    for q in glow.patches() {
        assert!((q.row as usize) < g.rows, "row tag in grid: {q:?}");
        assert_eq!(q.base_y, root, "natural root under a real band: {q:?}");
    }
    assert!(
        glow.charred().iter().all(|cf| (cf.row as usize) < g.rows),
        "the engulf gate never keys a wrapped/above-grid charred row"
    );
    assert!(
        glow.halo_cells().iter().all(|c| (c.row as usize) < g.rows),
        "the engulf gate never keys a wrapped/above-grid halo row"
    );
}

/// The identity law at a real host origin: with a nonzero origin but `head == 0`
/// (no chrome band: Linux, `ATERM_NO_FULLSIZE_CONTENT`, fullscreen) the fire
/// reproduces the pad-relative behaviour translated by origin: the row-0 root is
/// forced one cell down (`origin_y + ch`) and no patch pixel rises above the
/// grid top (`y >= origin_y`).
#[test]
fn fire_identity_at_origin_without_head() {
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 12,
        origin_y: 12,
        win_w: 344,
        win_h: 120,
        head: 0,
    };
    let c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((0, 0)), t, &c, g, &mut out);
    for i in 1..=14u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((0, i)), t, &c, g, &mut out);
    }
    assert!(!glow.patches().is_empty(), "a row-0 blaze emits fire");
    for q in glow.patches() {
        assert!(
            q.y as i32 >= g.origin_y as i32,
            "head 0: no pixel above the grid top (the historical clamp): {q:?}"
        );
        assert_eq!(
            q.base_y as i32,
            g.origin_y as i32 + g.ch as i32,
            "head 0: the row-0 root stays forced one cell down: {q:?}"
        );
    }
}

/// The bounded thermal-tier machine (cold, warm, hot, smoking as tiers 0..=3):
/// stoking climbs one tier at a time; quenching and cooling only descend.
fn emberforge_thermal_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        EmberforgeThermal {
            const Tiers = 3;
            const Buggy = 0;
            var tier = 0;
            action Stoke when (tier <= Tiers - 1) { tier = tier + 1; }
            action Quench when (tier > 0) {
                tier = if Buggy == 1 { tier + 1 } else { tier - 1 };
            }
            invariant Bounded: tier <= Tiers;
        }
    }
}

#[test]
fn emberforge_thermal_model_proves_and_catches_heating_quench() {
    let model = emberforge_thermal_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

/// Tier-1 conformance: drive the real fire integrators through a stoke burst, a
/// backspace quench run and an idle cool-down, project `disp_t` onto the model's
/// tier, and check every observed transition against the derived machine (no
/// frame moves more than one tier).
#[test]
fn emberforge_real_thermal_conforms_to_model() {
    let model = emberforge_thermal_model();
    let mut state = model.init_state();
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let tier = |d: f32| ((d / 0.25) as i64).min(3);

    let mut t = t0;
    let mut row = 2u16;
    let mut col = 0u16;
    let prev_tier = std::cell::Cell::new(0i64);
    let check = |glow: &CursorGlow, state: &mut aterm_spec::interp::State| {
        let cur_tier = tier(glow.disp_t);
        let delta = cur_tier - prev_tier.get();
        assert!(
            delta.abs() <= 1,
            "a single frame moved {delta} tiers (disp_t {})",
            glow.disp_t
        );
        if delta == 1 {
            assert!(model.fire("Stoke", state), "Stoke rejected at {cur_tier}");
        } else if delta == -1 {
            assert!(model.fire("Quench", state), "Quench rejected at {cur_tier}");
        }
        assert!(model.check_invariant("Bounded", state));
        assert_eq!(state["tier"], cur_tier, "projection diverged");
        prev_tier.set(cur_tier);
    };

    glow.tick(Some((row, col)), t, &c, g, &mut out);
    for _ in 0..60 {
        t += Duration::from_millis(40);
        col += 1;
        if col > 30 {
            row += 1;
            col = 1;
        }
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((row, col)), t, &c, g, &mut out);
        check(&glow, &mut state);
    }
    assert!(
        prev_tier.get() >= 2,
        "a sustained burst reaches HOT+ ({})",
        prev_tier.get()
    );
    for _ in 0..8 {
        t += Duration::from_millis(60);
        arm_exact_backspace(&mut glow, t, (row, col), (row, col - 1));
        col -= 1;
        glow.tick(Some((row, col)), t, &c, g, &mut out);
        check(&glow, &mut state);
    }
    for _ in 0..2000 {
        t += Duration::from_millis(16);
        glow.tick(Some((row, col)), t, &c, g, &mut out);
        check(&glow, &mut state);
    }
    assert_eq!(prev_tier.get(), 0, "idle returns to COLD");
    assert_eq!(glow.disp_t, 0.0, "display temperature snaps to exactly 0");
}

/// Backspace quenches, never stokes: a delete run monotonically cools the
/// display temperature, the quench meter escalates with consecutive deletes,
/// and arrow-left navigation (no key hint) earns no heat.
#[test]
fn fire_backspace_quenches_and_navigation_never_heats() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    let mut glow = CursorGlow::default();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=30u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    let peak = glow.disp_t;
    assert!(peak > 0.5, "burst warmed up ({peak})");

    let mut col = 30u16;
    let mut prev = peak;
    let q1 = {
        t += Duration::from_millis(60);
        arm_exact_backspace(&mut glow, t, (2, col), (2, col - 1));
        col -= 1;
        glow.tick(Some((2, col)), t, &c, g, &mut out);
        glow.quench
    };
    for _ in 0..5 {
        t += Duration::from_millis(60);
        arm_exact_backspace(&mut glow, t, (2, col), (2, col - 1));
        col -= 1;
        glow.tick(Some((2, col)), t, &c, g, &mut out);
        assert!(
            glow.disp_t <= prev + 1e-4,
            "deleting must never heat the fire ({prev} -> {})",
            glow.disp_t
        );
        prev = glow.disp_t;
    }
    assert!(glow.quench > q1, "the quench meter escalates over a run");
    assert!(
        glow.disp_t < peak * 0.5,
        "a delete run visibly douses the fire ({peak} -> {})",
        glow.disp_t
    );

    let mut nav = CursorGlow::default();
    let mut t = t0;
    let mut col = 30u16;
    nav.tick(Some((2, col)), t, &c, g, &mut out);
    for _ in 0..15 {
        t += Duration::from_millis(50);
        col -= 1;
        nav.tick(Some((2, col)), t, &c, g, &mut out);
    }
    assert_eq!(nav.heat, 0.0, "backward navigation earns no heat");
    assert!(nav.disp_t < 0.05, "backward navigation stays cold");
}

/// The forge cursor: metal heats under sustained blaze, holds heat longer than
/// the flames (hysteresis), yields a black-body fill (red dominant), and cools
/// back to its resting ember and inactive (never the theme's cursor colour).
#[test]
fn fire_forge_cursor_heats_holds_and_cools_to_ember() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    let cold_fill = glow.forge_fill().expect("fire cursor is always warm metal");
    let (cr, cg, cb) = (
        (cold_fill >> 16) & 0xff,
        (cold_fill >> 8) & 0xff,
        cold_fill & 0xff,
    );
    assert!(
        cr >= cg && cg >= cb,
        "cold ember is warm-ordered: {cold_fill:#08x}"
    );
    assert_ne!(cold_fill, c.color, "the ember is not the theme's colour");
    let mut col = 0u16;
    for i in 1..=180 {
        t += Duration::from_millis(16);
        if i % 3 == 0 {
            col += 1;
            glow.note_synthetic_typed(t, 1);
        }
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    let hot = glow.cursor_temp;
    assert!(hot > 0.3, "sustained typing forges the cursor hot ({hot})");
    let fill = glow.forge_fill().expect("hot metal overrides the fill");
    let (r, g_, b) = ((fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff);
    assert!(
        r >= g_ && g_ >= b,
        "black-body fill: R ≥ G ≥ B ({fill:06x})"
    );
    glow.tick(
        Some((2, col)),
        t + Duration::from_millis(2500),
        &c,
        g,
        &mut out,
    );
    assert!(
        glow.cursor_temp > hot * 0.45,
        "metal holds heat across a pause ({hot} -> {})",
        glow.cursor_temp
    );
    assert!(
        glow.cursor_temp < hot,
        "…but it is cooling ({hot} -> {})",
        glow.cursor_temp
    );
    assert!(
        glow.is_active(),
        "a visibly hot cursor keeps the timer armed"
    );
    let fp = glow.tick(Some((2, col)), t + Duration::from_secs(40), &c, g, &mut out);
    assert_eq!(glow.cursor_temp, 0.0, "metal cools to exactly 0");
    assert_eq!(
        glow.forge_fill(),
        Some(cold_fill),
        "the fill cools back to the resting ember"
    );
    assert_eq!(
        fp, 0,
        "idle fingerprint is 0 (constant rest ember not folded)"
    );
    assert!(!glow.is_active(), "the animation timer disarms");
}

/// The forge caret reads on its page at rest and at every heat: the block
/// draws the glyph beneath it in the page colour, so the fill keeps
/// [`ForgeBand::PAGE_CONTRAST`] against the page on the shipped dark default,
/// GitHub Light and the other common grounds, resting and heating (breath
/// included), as a warm point of the black-body ramp.
#[test]
fn the_forge_caret_reads_on_its_page_at_rest_and_hot() {
    let g = geom();
    let ratio = wcag_ratio;
    for bg in [
        0x0011_1318u32,
        0x00FF_FFFF,
        0x001A_1B26,
        0x0028_2C34,
        0x002E_3440,
        0x0000_0000,
        0x00FD_F6E3,
    ] {
        let mut c = cfg(GlowStyle::Fire, true);
        c.theme_bg = bg;
        c.dark_theme = aterm_render::theme_is_dark(bg);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((2, 0)), Instant::now(), &c, g, &mut out);
        let rest = glow.forge_fill().expect("the forge always owns its fill");
        let band = glow.forge_band.expect("a tick solves the band");
        assert_eq!(
            rest,
            fire_ramp(band.lo),
            "{bg:06x}: the ember is on the ramp"
        );
        assert!(
            ratio(rest, bg) >= ForgeBand::PAGE_CONTRAST,
            "{bg:06x}: resting ember {rest:06x} at {:.2}:1",
            ratio(rest, bg)
        );
        for k in 0..=20 {
            glow.cursor_temp = k as f32 / 20.0;
            for phase in [0.0f32, 0.9, 1.8, 2.7, 3.6] {
                glow.flame_phase = phase;
                let fill = glow.forge_fill().expect("always warm metal");
                let (r, g_, b) = ((fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff);
                assert!(
                    r >= g_ && g_ >= b && r > b,
                    "{bg:06x}: {fill:06x} is a warm ramp colour"
                );
                assert!(
                    ratio(fill, bg) >= ForgeBand::PAGE_CONTRAST,
                    "{bg:06x}: heat {} fill {fill:06x} at {:.2}:1",
                    glow.cursor_temp,
                    ratio(fill, bg)
                );
            }
        }
        glow.cursor_temp = 1.0;
        glow.flame_phase = 0.0;
        assert_ne!(glow.forge_fill(), Some(rest), "{bg:06x}: the metal heats");
    }
    assert!(
        ratio(fire_ramp(ForgeBand::COLD), 0x0011_1318) < 3.0,
        "the control must sink into the dark page"
    );
}

/// The budget law on a window-space layout (a real head band and padding): full
/// blaze plus the monster jump stays under MAX_QUADS with `origin > 0` and
/// window extents wider than the grid.
#[test]
fn fire_field_full_blaze_respects_quad_budget_with_head_band() {
    let g = Geom {
        cw: 22,
        ch: 44,
        rows: 60,
        cols: 200,
        origin_x: 8,
        origin_y: 56,
        win_w: 4416,
        win_h: 2704,
        head: 48,
    };
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=60u16 {
        t += Duration::from_millis(30);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    t += Duration::from_millis(30);
    glow.note_synthetic_move(t);
    glow.tick(Some((30, 10)), t, &c, g, &mut out);
    t += Duration::from_millis(120);
    glow.tick(Some((30, 10)), t, &c, g, &mut out);
    assert!(out.len() <= CursorGlow::MAX_QUADS);
    for q in &out {
        assert!((q.row as usize) < g.rows, "row tag in grid: {q:?}");
        assert!(
            (q.x + q.w) as u32 <= g.win_w as u32 && (q.y + q.h) as u32 <= g.win_h as u32,
            "quad inside the window: {q:?}"
        );
    }
}

/// Round light: fire's embers, crown and impact flash are radial halos, and the
/// halo stream obeys the quads' invariants (per-row bands, grid-interior, radii
/// ≥ 1, decays to empty, folded into the fingerprint).
#[test]
fn fire_light_is_radial_halos_not_squares() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=12u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    assert!(
        !glow.halos().is_empty(),
        "typing fire emits radial halos (embers + crown)"
    );
    for h in glow.halos() {
        assert!(h.rx >= 1 && h.ry >= 1, "falloff radii floored: {h:?}");
        assert!((h.row as usize) < g.rows, "halo row in grid: {h:?}");
        let band = h.row as u32 * g.ch as u32;
        assert!(
            h.y as u32 >= band && (h.y + h.h) as u32 <= band + g.ch as u32,
            "halo quad stays in its row band: {h:?}"
        );
        assert!(
            (h.x + h.w) as usize <= g.cols * g.cw,
            "halo quad inside grid: {h:?}"
        );
    }
    t += Duration::from_millis(300);
    glow.note_synthetic_move(t);
    glow.tick(Some((5, 30)), t, &c, g, &mut out);
    let m_life = glow.fire_meteors[0].life;
    t += Duration::from_secs_f32(m_life * 0.8);
    glow.tick(Some((5, 30)), t, &c, g, &mut out);
    let landing_x = (30.5 * g.cw as f32) as i32;
    assert!(
        glow.halos()
            .iter()
            .any(|h| (h.cx as i32 - landing_x).abs() < 2 * g.cw as i32),
        "the strike flashes a radial halo at the landing"
    );
    t += Duration::from_secs(40);
    let fp = glow.tick(Some((5, 30)), t, &c, g, &mut out);
    assert!(glow.halos().is_empty(), "halos decay to empty");
    assert_eq!(fp, 0);
}

/// The halo stream is folded into the tick fingerprint: an ember-only change (same quads) still
/// changes the fp, so the frame presents.
#[test]
fn halo_changes_change_the_fingerprint() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    glow.note_synthetic_typed(t0, 1);
    glow.tick(Some((2, 1)), t0, &c, g, &mut out);
    let fp1 = glow.tick(
        Some((2, 1)),
        t0 + Duration::from_millis(30),
        &c,
        g,
        &mut out,
    );
    let fp2 = glow.tick(
        Some((2, 1)),
        t0 + Duration::from_millis(60),
        &c,
        g,
        &mut out,
    );
    assert_ne!(fp1, fp2, "animating halos re-present frame to frame");
}

/// Cursor-private streams retain their plane/order identity, their private
/// record coverage, and the scheduler's exact idle sentinel.
#[test]
fn frame_fingerprint_preserves_private_stream_identity_and_idle_zero() {
    fn glow_hash(tag: u64, q: GlowQuad) -> u64 {
        let mut fp = FrameFingerprint::default();
        fp.glow_quads(tag, &[q]);
        fp.finish()
    }
    fn char_hash(c: CharFg) -> u64 {
        let mut fp = FrameFingerprint::default();
        fp.chars(&[c]);
        fp.finish()
    }
    fn fire_halo_hash(c: FireHaloCell) -> u64 {
        let mut fp = FrameFingerprint::default();
        fp.fire_halos(&[c]);
        fp.finish()
    }

    assert_eq!(FrameFingerprint::default().finish(), 0, "empty is idle");

    let mut zero_preimage = FrameFingerprint::default();
    zero_preimage.chars(&[CharFg {
        row: 0xb73a,
        col: 0x2ea5,
        fg: 0x6a72_d51f,
    }]);
    assert_eq!(zero_preimage.0, Some(0), "regression preimage stays exact");
    assert_eq!(zero_preimage.finish(), 1, "nonempty can never report idle");

    let glow = GlowQuad {
        row: 1,
        x: 2,
        y: 3,
        w: 4,
        h: 5,
        color: 0x12_3456,
        alpha: 7,
        color2: 0x12_3456,
        alpha2: 7,
    };
    let glow_base = glow_hash(FrameFingerprint::GLOW_OUT, glow);
    assert_ne!(
        glow_hash(FrameFingerprint::GLOW_UNDER, glow),
        glow_base,
        "the same quad in glow_out and glow_under is not the same frame"
    );
    let second = GlowQuad { x: 9, ..glow };
    let mut forward = FrameFingerprint::default();
    forward.glow_quads(FrameFingerprint::GLOW_OUT, &[glow, second]);
    let mut reversed = FrameFingerprint::default();
    reversed.glow_quads(FrameFingerprint::GLOW_OUT, &[second, glow]);
    assert_ne!(
        forward.finish(),
        reversed.finish(),
        "record order is content"
    );

    let char_fg = CharFg {
        row: 1,
        col: 2,
        fg: 0x12_3456,
    };
    let char_base = char_hash(char_fg);
    for (field, changed) in [
        ("row", CharFg { row: 3, ..char_fg }),
        ("col", CharFg { col: 3, ..char_fg }),
        (
            "fg",
            CharFg {
                fg: 0x65_4321,
                ..char_fg
            },
        ),
    ] {
        assert_ne!(char_hash(changed), char_base, "CharFg::{field}");
    }

    let fire_halo = FireHaloCell {
        row: 1,
        col: 2,
        strength: 3,
    };
    let fire_halo_base = fire_halo_hash(fire_halo);
    for (field, changed) in [
        (
            "row",
            FireHaloCell {
                row: 4,
                ..fire_halo
            },
        ),
        (
            "col",
            FireHaloCell {
                col: 4,
                ..fire_halo
            },
        ),
        (
            "strength",
            FireHaloCell {
                strength: 4,
                ..fire_halo
            },
        ),
    ] {
        assert_ne!(
            fire_halo_hash(changed),
            fire_halo_base,
            "FireHaloCell::{field}"
        );
    }

    let mut forge_a = FrameFingerprint::default();
    forge_a.forge_fill(0x12_3456);
    let mut forge_b = FrameFingerprint::default();
    forge_b.forge_fill(0x65_4321);
    assert_ne!(forge_a.finish(), forge_b.finish(), "forge fill");
}

/// Smoke law: a cursor forged hot sheds smoke wisps continuously while the metal
/// is above the smoking point, and the smoke stops (vapor drains, timer
/// disarms) once it cools.
#[test]
fn hot_cursor_smokes_and_cool_cursor_stops() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    let mut col = 0u16;
    glow.tick(Some((2, col)), t, &c, g, &mut out);
    for i in 1..=300 {
        t += Duration::from_millis(16);
        if i % 3 == 0 && col < 38 {
            col += 1;
            glow.note_synthetic_typed(t, 1);
        } else if i % 60 == 0 {
            col = 2;
            glow.note_synthetic_move(t);
        }
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    assert!(
        glow.cursor_temp > CursorGlow::SMOKE_TEMP,
        "sustained typing crosses the smoking point ({})",
        glow.cursor_temp
    );
    assert!(
        glow.vapor.iter().any(|v| v.kind == VaporKind::Smoke),
        "a hot cursor sheds smoke wisps"
    );
    for _ in 0..40 {
        t += Duration::from_secs(1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    assert!(glow.vapor.is_empty(), "smoke ceases once the metal cools");
    assert!(!glow.is_active(), "idle returns to zero work");
}

/// Dark-core law: at a towering blaze the flame body is emitted as per-pixel
/// [`FirePatch`]es (drawn at the under-ink seam), and everything decays back to
/// exactly empty.
#[test]
fn towering_blaze_chars_the_engulfed_rows_and_decays() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    glow.tick(Some((4, 0)), t, &c, g, &mut out);
    for i in 1..=39u16 {
        t += Duration::from_millis(30);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((4, i)), t, &c, g, &mut out);
    }
    assert!(
        !glow.patches().is_empty(),
        "the flame body renders as per-pixel fire patches"
    );
    assert!(
        glow.patches().iter().all(|q| (q.row as usize) < g.rows),
        "fire patches stay in the grid"
    );
    assert!(
        glow.patches().iter().all(|q| q.mode == FireMode::Add),
        "dark-theme fire is additive"
    );
    assert!(
        glow.charred().is_empty(),
        "a blaze never recolors ink (the owner's no-char verdict)"
    );
    assert!(
        glow.halos().iter().all(|h| h.mode == HaloMode::Add),
        "no Over-mode veils ride the fire (the cool-pocket experiment is retired)"
    );
    assert!(
        !glow.halo_cells().is_empty(),
        "a towering blaze emits contrast-halo strengths for its engulfed cells"
    );
    let fp = glow.tick(Some((4, 39)), t + Duration::from_secs(30), &c, g, &mut out);
    assert!(glow.patches().is_empty(), "fire patches decay");
    assert!(glow.halo_cells().is_empty(), "contrast-halo cells decay");
    assert_eq!(fp, 0);
}

/// The contrast-halo law: a hot blaze emits colour-free halo strengths for the
/// cells the flame engulfs, rising toward the head where the fire is brightest,
/// while ink is never recoloured (no charred entries).
#[test]
fn hot_blaze_emits_halo_strengths_rising_toward_the_head_and_never_recolors() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((0, 0)), t, &c, g, &mut out);
    for i in 1..=34u16 {
        t += Duration::from_millis(33);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((0, i)), t, &c, g, &mut out);
    }
    let cells = glow.halo_cells();
    assert!(!cells.is_empty(), "a hot blaze emits contrast-halo cells");
    assert!(
        glow.charred().is_empty(),
        "the halo never recolors ink — charred stays empty (the no-recolor law)"
    );
    assert!(
        cells
            .windows(2)
            .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col)),
        "halo cells are per-(row, col) sorted with unique cells: {cells:?}"
    );
    let row0: Vec<_> = cells.iter().filter(|c| c.row == 0).collect();
    assert!(
        row0.len() >= 2,
        "the typed row carries several engulfed cells: {cells:?}"
    );
    let (tail, head) = (row0[0], row0[row0.len() - 1]);
    assert!(
        tail.col < head.col,
        "sorted row-0 cells span tail→head ({} .. {})",
        tail.col,
        head.col
    );
    assert!(
        head.strength > tail.strength,
        "halo strength rises toward the head (head {} > tail {})",
        head.strength,
        tail.strength
    );
}

/// On a LIGHT theme the fire is ink tongues beneath the letters: no field
/// patch, only source-over paint in the under-ink stream, in warm colours
/// (orange at the root to pale yellow at the tips), rooted in the typed row
/// and never more than two rows tall above it, its opacity melting toward
/// the tips and the sides of the run rather than ending on a hard edge.
#[test]
fn light_theme_fire_is_ink_tongues_beneath_the_letters() {
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Fire, true);
    c.dark_theme = false;
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
    let row = 4u16;
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    glow.tick(Some((row, 2)), t, &c, g, &mut out);
    for i in 1..=14u16 {
        t += Duration::from_millis(45);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((row, 2 + i)), t, &c, g, &mut out);
    }
    assert!(glow.patches().is_empty(), "no field patch on white");
    let paint: Vec<GlowQuad> = glow
        .under_quads()
        .iter()
        .copied()
        .filter(|q| q.alpha > 0)
        .collect();
    assert!(!paint.is_empty(), "light-theme fire still paints");
    assert!(
        glow.under_quads()
            .iter()
            .all(|q| q.alpha > 0 && q.alpha2 > 0),
        "every piece of it is source-over ink"
    );
    for q in &paint {
        let a = f32::from(q.alpha.max(1));
        let ch = |sh: u32| ((q.color >> sh) & 0xff) as f32 * 255.0 / a;
        assert!(
            ch(16) >= ch(8) - 3.0 && ch(8) >= ch(0) - 3.0 && ch(16) >= 200.0,
            "a warm stop: {q:?}"
        );
    }
    let row_top = i32::from(g.origin_y) + i32::from(row) * g.ch as i32;
    let lowest = paint
        .iter()
        .map(|q| i32::from(q.y + q.h))
        .max()
        .unwrap_or(0);
    let highest = paint.iter().map(|q| i32::from(q.y)).min().unwrap_or(0);
    assert!(
        lowest > row_top + g.ch as i32 / 2 && lowest <= row_top + g.ch as i32,
        "rooted under the typed row's letters ({lowest} vs row {row_top})"
    );
    assert!(
        highest >= row_top - 2 * g.ch as i32,
        "no taller than two rows above it ({highest} vs row {row_top})"
    );
    let probe_y = (row_top + highest) / 2 + g.ch as i32 / 3;
    let profile = |quads: &[GlowQuad]| -> Vec<f32> {
        let mut a = vec![0.0f32; g.win_w as usize];
        for q in quads
            .iter()
            .filter(|q| i32::from(q.y) <= probe_y && probe_y < i32::from(q.y + q.h))
        {
            let w = f32::from(q.w.max(1));
            for i in 0..q.w {
                let k = (f32::from(i) + 0.5) / w;
                let v = f32::from(q.alpha) * (1.0 - k) + f32::from(q.alpha2) * k;
                let x = usize::from(q.x + i);
                a[x] = 1.0 - (1.0 - a[x]) * (1.0 - v / 255.0);
            }
        }
        a
    };
    let steepest = |a: &[f32]| {
        a.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    };
    let shipped = profile(&paint);
    assert!(
        shipped.iter().any(|&v| v > 0.3),
        "the probe line crosses the flames"
    );
    assert!(
        steepest(&shipped) < 0.22,
        "a hard edge in the flames: {:.2} in one pixel",
        steepest(&shipped)
    );
    let flat: Vec<GlowQuad> = paint
        .iter()
        .map(|q| {
            let a = q.alpha.max(q.alpha2);
            GlowQuad {
                alpha: a,
                alpha2: a,
                ..*q
            }
        })
        .collect();
    assert!(
        steepest(&profile(&flat)) >= 0.22,
        "control: a flat block of the same flame shows its edge"
    );
}

/// Fire embers keep off the letters: an ember over a cell the row probe shows
/// as a glyph is not drawn (light over a letter is a smear on it), while one
/// over a blank cell (a gap, the airspace) keeps full punch.
#[test]
fn fire_embers_keep_off_probed_glyph_cells() {
    let g = geom();
    let mut c = cfg(GlowStyle::Fire, true);
    c.radius = 0.0;
    c.ring = false;
    let at = Instant::now();
    let mut line = vec![' '; 40];
    for slot in &mut line[..10] {
        *slot = 'a';
    }
    let light_at = |col: u16| -> u64 {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.observe_row(2, 30, &line, at);
        glow.tick(Some((2, 30)), at, &c, g, &mut out);
        glow.particles.clear();
        let (x, y) = g.cell_center(2, col);
        glow.particles.push(Particle {
            x0: x,
            y0: y,
            vx: 0.0,
            vy: 0.0,
            gy: 0.0,
            life: 5.0,
            hue: 0.6,
            cov_scale: 1.0,
            born: at,
        });
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_particles(at, &c, g, &mut out, &mut halos, &mut Vec::new());
        halos
            .iter()
            .filter(|h| h.mode == HaloMode::Add)
            .map(|h| {
                u64::from(((h.color >> 16) & 0xff) + ((h.color >> 8) & 0xff) + (h.color & 0xff))
            })
            .max()
            .unwrap_or(0)
    };
    let (over_glyph, over_blank) = (light_at(4), light_at(20));
    assert!(over_blank > 0, "the ember over a blank cell must be lit");
    assert_eq!(over_glyph, 0, "an ember lands on a probed glyph cell");
}

/// Fire drops its additive "mouth of fire" crown on a light theme, where it
/// would lift trailing glyphs toward the white ground.
#[test]
fn fire_light_drops_the_additive_crown() {
    let g = geom();
    let crown_rx = 2 * g.ch as u16;
    let has_crown = |dark: bool| -> bool {
        let mut c = cfg(GlowStyle::Fire, true);
        c.dark_theme = dark;
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let mut t = t0;
        glow.tick(Some((2, 0)), t, &c, g, &mut out);
        for i in 1..=10u16 {
            t += Duration::from_millis(30);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((2, i)), t, &c, g, &mut out);
        }
        glow.halos()
            .iter()
            .any(|h| h.mode == HaloMode::Add && h.rx > crown_rx)
    };
    assert!(
        has_crown(true),
        "the additive mouth-of-fire crown lights on dark"
    );
    assert!(
        !has_crown(false),
        "the additive crown is dropped on a light theme"
    );
}

/// A line change snuffs the typed trail: type hot on one row (long chained spark
/// lives), press Enter, and within a beat the old row emits nothing.
#[test]
fn fire_row_change_snuffs_the_old_lines_trail() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=12u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    assert!(
        glow.sparks.iter().any(|s| s.row == 2),
        "the typed row is burning before the line change"
    );
    t += Duration::from_millis(60);
    glow.note_synthetic_move(t);
    glow.tick(Some((3, 0)), t, &c, g, &mut out);
    t += Duration::from_millis(250);
    glow.tick(Some((3, 0)), t, &c, g, &mut out);
    assert!(
        glow.sparks.iter().all(|s| s.row == 3),
        "the old line's trail is snuffed after a line change"
    );
    assert!(
        out.iter().all(|q| q.row != 2 || (q.x as usize) < 8 * g.cw),
        "the old row is dark away from the landing after a line change"
    );
}

/// One composited light frame of a fire engine: the white page, the under-ink
/// stream and the ink-fire `patches` (the engine's own, or a control's), the
/// glyph ink of `ink` (cells and their colours), then the over-ink streams, in
/// the renderer's painter order.
fn fire_light_glass(
    glow: &CursorGlow,
    out: &[GlowQuad],
    patches: &[FirePatch],
    g: Geom,
    cells: &[Vec<aterm_core::terminal::RenderCell>],
    ink: &[(u16, u16, u32)],
) -> Glass {
    let mut f = Glass::new(g, 0x00FF_FFFF);
    for (row, line) in cells.iter().enumerate() {
        for (col, cell) in line.iter().enumerate() {
            let [red, green, blue] = cell.bg;
            let bg = (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue);
            let row = u16::try_from(row).expect("captured row fits the cell coordinate");
            let col = u16::try_from(col).expect("captured column fits the cell coordinate");
            let (x0, y0, x1, y1) = f.cell_rect(row, col);
            for y in y0..y1 {
                for x in x0..x1 {
                    f.px[y as usize * f.w + x as usize] = bg;
                }
            }
        }
    }
    f.add_quads(glow.under_quads());
    f.add_fire_patches(patches);
    for &(row, col, fg) in ink {
        for (y, x) in glyph(g, row, col).0 {
            f.px[y as usize * f.w + x as usize] = fg;
        }
    }
    f.add_quads(out);
    f.add_halos(glow.halos());
    f
}

/// Whether a fire frame lays any additive light: an Add halo, an alpha-0
/// (additive) quad in either quad stream, or an additive field patch.
fn fire_frame_is_additive(glow: &CursorGlow, out: &[GlowQuad]) -> bool {
    glow.halos().iter().any(|h| h.mode == HaloMode::Add)
        || out.iter().chain(glow.under_quads()).any(|q| q.alpha == 0)
        || glow.patches().iter().any(|p| p.mode == FireMode::Add)
}

/// Fire on a light ground is ink, never light: a hot typed run and a jump emit
/// no additive halo, quad or field (additive light only lifts the letters
/// toward a white page), and still show fire.
#[test]
fn fire_on_a_light_ground_is_ink_never_light() {
    let g = tall_geom();
    let (fg, red) = (0x001F_2328u32, 0x00CF_222Eu32);
    let (row, start) = (4u16, 2u16);
    let config = |dark: bool| {
        let mut c = cfg(GlowStyle::Fire, true);
        c.intensity = 1.0;
        c.dark_theme = dark;
        (c.theme_fg, c.theme_bg) = if dark {
            (0x00D0_D0D0, 0x0011_1318)
        } else {
            (fg, 0x00FF_FFFF)
        };
        c
    };
    let blank = vec![' '; g.cols];
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    let cells_for = |caret: u16, reds: (u16, u16), mixed: bool| {
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(fg), rgb(0x00FF_FFFF));
            }
        }
        for col in start..caret {
            let cell = &mut rows[usize::from(row)][usize::from(col)];
            cell.ch = 'm';
            if (caret.saturating_sub(reds.0)..caret.saturating_sub(reds.1)).contains(&col) {
                cell.fg = rgb(red);
            }
        }
        for above in [row - 1, row - 2] {
            for (col, cell) in rows[usize::from(above)].iter_mut().enumerate() {
                cell.ch = 'm';
                if col % 2 == 1 {
                    cell.fg = rgb(red);
                }
            }
        }
        if mixed {
            for cell in &mut rows[usize::from(row)][usize::from(start)..usize::from(caret)] {
                (cell.fg, cell.bg) = (rgb(0x00FF_FFFF), rgb(0));
            }
        }
        rows
    };
    let key = |glow: &mut CursorGlow,
               text: &mut Vec<char>,
               c: &GlowConfig,
               t,
               k: u16,
               out: &mut Vec<GlowQuad>,
               frame: bool,
               air_above: bool,
               reds: (u16, u16),
               mixed: bool| {
        text[usize::from(start + k - 1)] = 'm';
        glow.observe_row(row, start + k, text, t);
        if air_above {
            glow.observe_neighbor_rows(Some(&blank), Some(&blank));
        }
        if frame {
            glow.observe_frame_text(
                Some((row, start + k)),
                &cells_for(start + k, reds, mixed),
                t,
            );
        }
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((row, start + k)), t, c, g, out);
    };

    {
        let c = config(true);
        let (mut glow, mut out, mut text) = (CursorGlow::default(), Vec::new(), vec![' '; g.cols]);
        let mut t = Instant::now();
        glow.tick(Some((row, start)), t, &c, g, &mut out);
        let mut seen = false;
        for k in 1..=36u16 {
            t += Duration::from_millis(45);
            key(
                &mut glow,
                &mut text,
                &c,
                t,
                k,
                &mut out,
                false,
                false,
                (6, 3),
                false,
            );
            seen |= fire_frame_is_additive(&glow, &out);
        }
        assert!(
            seen,
            "control: the dark run lays additive light the probe can see"
        );
    }

    let c = config(false);
    let run = |frame: bool,
               air_above: bool,
               uncapped: bool,
               reds: (u16, u16),
               mixed: bool,
               each: &mut dyn FnMut(&CursorGlow, &[GlowQuad], u16)| {
        let (mut glow, mut out, mut text) = (CursorGlow::default(), Vec::new(), vec![' '; g.cols]);
        glow.light_body_unsolved = uncapped;
        let mut t = Instant::now();
        glow.observe_row(row, start, &text, t);
        glow.tick(Some((row, start)), t, &c, g, &mut out);
        for k in 1..=36u16 {
            t += Duration::from_millis(45);
            key(
                &mut glow, &mut text, &c, t, k, &mut out, frame, air_above, reds, mixed,
            );
            assert!(
                !fire_frame_is_additive(&glow, &out),
                "key {k}: no additive light on white"
            );
            if k >= 8 {
                each(&glow, &out, start + k);
            }
        }
        (glow, out, t)
    };
    let composite =
        |glow: &CursorGlow, out: &[GlowQuad], caret: u16, reds: (u16, u16), mixed: bool| {
            let mut ink: Vec<(u16, u16, u32)> = (start..caret)
                .map(|col| {
                    let is_red = (caret - reds.0..caret - reds.1).contains(&col);
                    (row, col, if is_red { red } else { fg })
                })
                .collect();
            for above in [row - 1, row - 2] {
                ink.extend(
                    (0..g.cols as u16).map(|col| (above, col, if col % 2 == 0 { fg } else { red })),
                );
            }
            if mixed {
                for (r, _, color) in &mut ink {
                    if *r == row {
                        *color = 0x00FF_FFFF;
                    }
                }
            }
            fire_light_glass(glow, out, &[], g, &cells_for(caret, reds, mixed), &ink)
        };
    let worst = |f: &Glass, r: u16, cols: &mut dyn Iterator<Item = u16>| {
        cols.map(|col| {
            let (i, cnt) = glyph(g, r, col);
            ratio(f, &i, &cnt)
        })
        .fold(f32::INFINITY, f32::min)
    };
    let judge = |frame: bool, uncapped: bool, reds: (u16, u16)| {
        let mut w = [f32::INFINITY; 4];
        let (glow, out, t) = run(
            frame,
            false,
            uncapped,
            reds,
            false,
            &mut |glow, out, caret| {
                let f = composite(glow, out, caret, reds, false);
                w[0] = w[0].min(worst(
                    &f,
                    row,
                    &mut (caret - 3..caret)
                        .filter(|c| !(caret - reds.0..caret - reds.1).contains(c)),
                ));
                w[1] = w[1].min(worst(&f, row, &mut (caret - reds.0..caret - reds.1)));
                for above in [row - 1, row - 2] {
                    let cols = || caret.saturating_sub(16)..caret;
                    w[2] = w[2].min(worst(&f, above, &mut cols().filter(|c| c % 2 == 0)));
                    w[3] = w[3].min(worst(&f, above, &mut cols().filter(|c| c % 2 == 1)));
                }
            },
        );
        (w, glow, out, t)
    };
    let (handed, _, _, _) = judge(true, false, (6, 3));
    assert!(
        handed[0] >= 4.5,
        "the newest letters read at {:.2}:1",
        handed[0]
    );
    assert!(
        handed[1] >= 3.0,
        "a red letter on the typed line reads at {:.2}:1",
        handed[1]
    );
    assert!(
        handed[2] >= 4.5,
        "the old output above reads at {:.2}:1",
        handed[2]
    );
    assert!(
        handed[3] >= 3.0,
        "a red letter of the old output above reads at {:.2}:1",
        handed[3]
    );
    let (probed, mut glow, mut out, t) = judge(false, false, (6, 3));
    assert!(
        probed[0] >= 4.5 && probed[2] >= 4.5 && probed[3] >= 3.0,
        "with the rows above unread: newest {:.2}:1, old output {:.2}:1, old red {:.2}:1",
        probed[0],
        probed[2],
        probed[3]
    );
    let (red_head, _, _, _) = judge(true, false, (3, 0));
    assert!(
        red_head[1] >= 3.0,
        "a red letter at the caret reads at {:.2}:1",
        red_head[1]
    );
    let (uncapped, _, _, _) = judge(true, true, (3, 0));
    assert!(
        uncapped[1] < 4.5 && uncapped[1] < red_head[1],
        "control: with no ceilings a red letter at the caret reads at {:.2}:1 ({:.2}:1 held)",
        uncapped[1],
        red_head[1]
    );

    let mixed_run = |uncapped: bool| {
        let mut minimum = f32::INFINITY;
        let mut frames = 0;
        let mut scanlines = 0;
        run(
            true,
            false,
            uncapped,
            (6, 3),
            true,
            &mut |glow, out, caret| {
                let f = composite(glow, out, caret, (6, 3), true);
                for col in caret - 8..caret {
                    let (ink, counters) = glyph(g, row, col);
                    let first = ink.first().expect("the captured glyph has stems").0;
                    let last = ink.last().expect("the captured glyph has stems").0;
                    for y in first..=last {
                        let stems: Vec<_> = ink.iter().copied().filter(|p| p.0 == y).collect();
                        let ground: Vec<_> =
                            counters.iter().copied().filter(|p| p.0 == y).collect();
                        assert!(
                            !stems.is_empty() && !ground.is_empty(),
                            "each captured glyph scanline has stems and adjacent counters"
                        );
                        minimum = minimum.min(ratio(&f, &stems, &ground));
                        scanlines += 1;
                    }
                }
                frames += 1;
            },
        );
        assert!(
            frames > 0 && scanlines >= 8 * 13,
            "the mixed-colour run judges actual painted frames and glyph scanlines"
        );
        minimum
    };
    let mixed = mixed_run(false);
    assert!(
        (4.5..20.0).contains(&mixed),
        "the light ink stays readable over visible flame on its dark cell ({mixed:.2}:1)"
    );
    let uncapped_mixed = mixed_run(true);
    assert!(
        uncapped_mixed < 4.5,
        "control: the same uncapped ramp obscures the light ink ({uncapped_mixed:.2}:1)"
    );

    let (mut flame_above, mut flame_faint) = (1.0f32, 1.0f32);
    run(false, true, false, (6, 3), false, &mut |glow, _, _| {
        let faint: Vec<GlowQuad> = glow
            .under_quads()
            .iter()
            .map(|q| {
                let k = |a: u8| ((f32::from(a) / 5.0) as u8).max(1);
                GlowQuad {
                    color: aterm_render::premul_rgb(unpremul(q.color, q.alpha), k(q.alpha)),
                    alpha: k(q.alpha),
                    color2: aterm_render::premul_rgb(unpremul(q.color2, q.alpha2), k(q.alpha2)),
                    alpha2: k(q.alpha2),
                    ..*q
                }
            })
            .collect();
        for (quads, darkest) in [
            (glow.under_quads(), &mut flame_above),
            (&faint[..], &mut flame_faint),
        ] {
            let mut bare = Glass::new(g, 0x00FF_FFFF);
            bare.add_quads(quads);
            let (_, y_top, _, _) = bare.cell_rect(row - 1, 0);
            let (_, y_line, _, _) = bare.cell_rect(row, 0);
            for y in y_top..y_line {
                for x in 0..bare.w as i32 {
                    *darkest = darkest.max(wcag_ratio(bare.at(y, x) & 0x00FF_FFFF, 0x00FF_FFFF));
                }
            }
        }
    });
    assert!(
        flame_above >= 1.35,
        "the flames over the blank row above read on white ({flame_above:.2}:1)"
    );
    assert!(
        flame_faint < 1.25,
        "control: at a fifth of their opacity the same flames read at {flame_faint:.2}:1"
    );

    glow.note_synthetic_move(t);
    glow.tick(Some((row + 2, 2)), t, &c, g, &mut out);
    assert_eq!(
        glow.fire_meteors.len(),
        1,
        "the jump flies a meteor on white"
    );
    let mut darkest = 0x00FF_FFFFu32;
    for k in 1..=8u64 {
        glow.tick(
            Some((row + 2, 2)),
            t + Duration::from_millis(30 * k),
            &c,
            g,
            &mut out,
        );
        assert!(
            !fire_frame_is_additive(&glow, &out),
            "jump frame {k}: no additive light on white"
        );
        let mut f = Glass::new(g, 0x00FF_FFFF);
        f.add_quads(glow.under_quads());
        for &p in &f.px {
            if rel_luminance(p & 0x00FF_FFFF) < rel_luminance(darkest) {
                darkest = p & 0x00FF_FFFF;
            }
        }
    }
    assert!(
        wcag_ratio(darkest, 0x00FF_FFFF) >= 2.0,
        "the meteor shows on white (darkest {darkest:06x})"
    );
    assert!(
        wcag_ratio(fg, darkest) >= 4.5,
        "a letter over the meteor's darkest paint keeps {:.2}:1",
        wcag_ratio(fg, darkest)
    );
}

/// A premultiplied colour at opacity `a` back to its straight colour.
fn unpremul(c: u32, a: u8) -> u32 {
    if a == 0 {
        return 0;
    }
    let ch = |sh: u32| ((((c >> sh) & 0xff) * 255 + u32::from(a) / 2) / u32::from(a)).min(255);
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// The shipped default dark pair the over-ink budgets are solved on: the
/// default dark theme's foreground on its background.
const DARK_PAIR: (u32, u32) = (0x00D0_D0D0, 0x0011_1318);

/// One composited dark frame of a fire engine's own light: the page, the
/// under-ink stream and the flame field, the glyph ink of `ink` cells, then
/// the over-ink quads `out` and the halos, in the renderer's painter order.
fn fire_dark_glass(glow: &CursorGlow, out: &[GlowQuad], g: Geom, ink: &[(u16, u16)]) -> Glass {
    let mut f = Glass::new(g, DARK_PAIR.1);
    f.add_quads(glow.under_quads());
    f.add_fire_patches(glow.patches());
    for &(row, col) in ink {
        for (y, x) in glyph(g, row, col).0 {
            f.px[y as usize * f.w + x as usize] = DARK_PAIR.0;
        }
    }
    f.add_quads(out);
    f.add_halos(glow.halos());
    f
}

/// The host's frame step for a fire run: [`CursorFx`] with the typing-momentum
/// glow switched as `momentum_glow`, a 13 × 26 cell, the caret's row probed
/// every frame (the GUI captures no neighbour rows for fire).
struct FireHost {
    fx: crate::cursor_fx::CursorFx,
    cfg: GlowConfig,
    momentum_glow: bool,
    text: Vec<char>,
    /// The frame's cells the host hands over each frame (the caret's row
    /// takes the typed glyphs); `None` hands none.
    rows: Option<Vec<Vec<aterm_core::terminal::RenderCell>>>,
}

impl FireHost {
    fn new(momentum_glow: bool, unbudgeted: bool) -> Self {
        Self::styled(GlowStyle::Fire, DARK_PAIR, momentum_glow, unbudgeted)
    }

    /// The host for `style` on the page `pair` (`(text, page)`).
    fn styled(style: GlowStyle, pair: (u32, u32), momentum_glow: bool, unbudgeted: bool) -> Self {
        let dark = aterm_render::theme_is_dark(pair.1);
        let mut cfg = cfg(style, true);
        cfg.dark_theme = dark;
        cfg.intensity = 1.0;
        (cfg.theme_fg, cfg.theme_bg) = pair;
        let mut fx = crate::cursor_fx::CursorFx::default();
        fx.glow.streak_unbudgeted = unbudgeted;
        Self {
            fx,
            cfg,
            momentum_glow,
            text: vec![' '; tall_geom().cols],
            rows: None,
        }
    }

    /// One frame at `now` with the caret at `cur`; `key` types a glyph first.
    fn frame(&mut self, now: Instant, cur: (u16, u16), key: bool) {
        let g = tall_geom();
        self.fx.begin(true, true);
        if key {
            self.fx.cadence.on_keystroke(now);
            self.fx
                .momentum
                .on_key(now, crate::cursor_momentum::MOMENTUM_GLOW_TAU_S);
            self.fx.glow.note_typed_cells(now, 1);
            self.text[usize::from(cur.1) - 1] = 'm';
        }
        self.fx.glow.observe_row(cur.0, cur.1, &self.text, now);
        if let Some(rows) = &self.rows {
            let mut rows = rows.clone();
            for (cell, &ch) in rows[usize::from(cur.0)].iter_mut().zip(&self.text) {
                cell.ch = ch;
            }
            self.fx.glow.observe_frame_text(Some(cur), &rows, now);
        }
        let input = crate::cursor_fx::CursorFxInput {
            now,
            cur: Some(cur),
            live_viewport: true,
            cursor_style: aterm_core::terminal::CursorStyle::SteadyBlock,
            blink_phase: true,
            live_cursor: self.cfg.color,
            cursor_color_pinned: false,
            default_bg: self.cfg.theme_bg,
            cursor_cell_bg: None,
            geom: g,
            focused: true,
            trail_brightness: 1.0,
            shed_envelope: 1.0,
            body_allowed: true,
            momentum_glow: self.momentum_glow,
            user_tinted: false,
        };
        let trail = TrailConfig {
            enabled: false,
            duration: Duration::from_millis(240),
            max_len: 18,
            color: self.cfg.color,
            intensity: 0.0,
            warmth: 0.0,
        };
        self.fx.tick(&input, &self.cfg, trail);
    }
}

/// THE TYPING GLOW KEEPS OFF THE PROMPT'S BAR AND THE LINE BELOW: on a dark
/// page the momentum glow lights the caret's line and the page round it,
/// never a band of warm fog over the inked rows next to it (round 1: over a
/// powerline prompt's bar above the caret it lifted the end of `main +2` and
/// laid a grey-beige haze over the bar; under it, over a selected line).
/// Typed hot under a bar (a coloured segment of letters, then a grey bar to
/// the edge) with a selection under the caret's line, the glow's own light
/// (the host's frame less its twin's without the glow) over those rows is a
/// small share of what it lays when the rows are air, while it still lights
/// the caret's own row. NEGATIVE CONTROL: with the frame's cells withheld
/// (rows nobody read are air to the glow, as it always drew them) the glow
/// lays its whole light over the same rows.
#[test]
fn the_typing_glow_keeps_off_the_prompt_bar_and_the_line_below() {
    let g = tall_geom();
    let (row, start) = (4u16, 2u16);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let page_rows = || {
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(DARK_PAIR.0), rgb(DARK_PAIR.1));
            }
        }
        rows
    };
    let inked = || {
        let mut rows = page_rows();
        for (col, cell) in rows[usize::from(row - 1)].iter_mut().enumerate() {
            if col < 22 {
                (cell.ch, cell.fg, cell.bg) = ('m', [0, 0, 0], rgb(0x00D7_D700));
            } else {
                cell.bg = rgb(0x0033_3333);
            }
        }
        for cell in &mut rows[usize::from(row + 1)] {
            (cell.ch, cell.bg) = ('m', rgb(0x0034_4766));
        }
        rows
    };
    // The glow's own light over rows `row - 1` and `row + 1` past the hold's
    // feather off the caret row's edges ([`CursorGlow::NEIGHBOUR_HOLD_Y`]),
    // and over the caret's row, summed (premultiplied channels × pixels).
    let (top, bot) = (
        i32::from(row) * g.ch as i32,
        i32::from(row + 1) * g.ch as i32,
    );
    let feather = (g.ch as f32 * CursorGlow::NEIGHBOUR_HOLD_Y).ceil() as i32;
    let light = |rows: Option<Vec<Vec<aterm_core::terminal::RenderCell>>>| -> (u64, u64) {
        let mut host = FireHost::new(true, false);
        let mut twin = FireHost::new(false, false);
        host.rows = rows.clone();
        twin.rows = rows;
        let mut t = Instant::now();
        host.frame(t, (row, start), false);
        twin.frame(t, (row, start), false);
        for k in 1..=24u16 {
            t += Duration::from_millis(57);
            host.frame(t, (row, start + k), true);
            twin.frame(t, (row, start + k), true);
        }
        let n = twin.fx.glow_scratch.len();
        assert!(
            host.fx.glow_scratch.starts_with(&twin.fx.glow_scratch),
            "fixture: the aurora's quads lead the frame, momentum glow or not"
        );
        let (mut beside, mut own) = (0u64, 0u64);
        for q in &host.fx.glow_scratch[n..] {
            let band = i32::from(q.y) / g.ch as i32;
            let sum =
                u64::from(((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff))
                    + u64::from(
                        ((q.color2 >> 16) & 0xff) + ((q.color2 >> 8) & 0xff) + (q.color2 & 0xff),
                    );
            let lit = sum * u64::from(q.w) * u64::from(q.h);
            let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
            if band == i32::from(row) {
                own += lit;
            } else if (band == i32::from(row) - 1 && y1 <= top - feather)
                || (band == i32::from(row) + 1 && y0 >= bot + feather)
            {
                beside += lit;
            }
        }
        (beside, own)
    };
    let (held, own) = light(Some(inked()));
    let (air, _) = light(Some(page_rows()));
    let (withheld, _) = light(None);
    assert!(own > 0, "the glow lights the caret's own row");
    assert!(
        air > 0,
        "fixture: over air the glow spreads into the rows round the caret"
    );
    assert!(
        held * 50 < air,
        "the glow lays {held} over the bar and the selection, against {air} over air"
    );
    assert!(
        withheld * 10 >= air * 9,
        "control: with no cells handed over the glow lays {withheld}, against {air} over air"
    );
}

/// The worst contrast the three newest letters keep over a hot run typed
/// through the host's frame step at `gap_ms` a key (the gallery's 57 ms and
/// a 40 ms burst), judged at every key from the tenth on and through 120 ms
/// after the last, with the momentum glow on as shipped: `(own, combined)`.
fn newest_letters_under_fire(gap_ms: u64, unbudgeted: bool) -> (f32, f32) {
    let g = tall_geom();
    let (row, start) = (3u16, 2u16);
    let mut host = FireHost::new(true, unbudgeted);
    let mut twin = FireHost::new(false, unbudgeted);
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    twin.frame(t, (row, start), false);
    let (mut own, mut combined) = (f32::INFINITY, f32::INFINITY);
    let mut judge = |host: &FireHost, twin: &FireHost, caret: u16| {
        assert_eq!(
            host.fx.glow.halos(),
            twin.fx.glow.halos(),
            "the aurora does not see the momentum glow"
        );
        assert!(
            host.fx.glow_scratch.starts_with(&twin.fx.glow_scratch),
            "the aurora's quads lead the frame, momentum glow or not"
        );
        let ink: Vec<(u16, u16)> = (start..caret).map(|col| (row, col)).collect();
        let alone = fire_dark_glass(&twin.fx.glow, &twin.fx.glow_scratch, g, &ink);
        let whole = fire_dark_glass(&host.fx.glow, &host.fx.glow_scratch, g, &ink);
        for col in caret - 3..caret {
            let (i, cnt) = glyph(g, row, col);
            own = own.min(ratio(&alone, &i, &cnt));
            combined = combined.min(ratio(&whole, &i, &cnt));
        }
    };
    let keys = 40u16;
    for k in 1..=keys {
        t += Duration::from_millis(gap_ms);
        host.frame(t, (row, start + k), true);
        twin.frame(t, (row, start + k), true);
        if k >= 10 {
            judge(&host, &twin, start + k);
        }
    }
    for dt in [16u64, 33, 60, 120] {
        let at = t + Duration::from_millis(dt);
        host.frame(at, (row, start + keys), false);
        twin.frame(at, (row, start + keys), false);
        judge(&host, &twin, start + keys);
    }
    (own, combined)
}

/// The head of a hot fire keeps the newest letters readable on a dark ground:
/// the streak, the crown, the forge and the ember shower spend one over-ink
/// account per text cell ([`InkLedger`]), so their sum over the letters just
/// typed leaves them 4.5:1 or better, at the gallery's cadence and in a fast
/// burst, driven through the host's own frame step with the momentum glow on.
#[test]
fn a_hot_fire_keeps_the_newest_letters_readable_on_dark() {
    for gap_ms in [57u64, 40] {
        let (held, combined) = newest_letters_under_fire(gap_ms, false);
        let (washed, _) = newest_letters_under_fire(gap_ms, true);
        assert!(
            held >= InkLedger::FLOOR,
            "{gap_ms} ms a key: the newest letters read at {held:.2}:1"
        );
        assert!(
            combined >= InkLedger::FLOOR,
            "{gap_ms} ms a key: under fire and the momentum glow together the newest \
             letters read at {combined:.2}:1"
        );
        assert!(
            washed < 2.0,
            "{gap_ms} ms a key: control: unaccounted, the fire leaves {washed:.2}:1"
        );
    }
}

/// A held Backspace through a hot fire on a dark ground keeps its steam off
/// the line above: twelve deletes at 50 ms leave every pixel of the row above
/// (which the host did not probe) under the clip, and its letters 3:1 or
/// better through the run and the cloud's fade.
#[test]
fn a_backspace_run_keeps_its_steam_off_the_line_above() {
    let run = |unbudgeted: bool| {
        let g = tall_geom();
        let mut c = cfg(GlowStyle::Fire, true);
        c.intensity = 1.0;
        (c.theme_fg, c.theme_bg) = DARK_PAIR;
        let mut glow = CursorGlow {
            streak_unbudgeted: unbudgeted,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let row = 3u16;
        let mut t = Instant::now();
        glow.tick(Some((row, 0)), t, &c, g, &mut out);
        for col in 1..=24u16 {
            t += Duration::from_millis(45);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((row, col)), t, &c, g, &mut out);
        }
        let (mut peak, mut worst) = (0, f32::INFINITY);
        let mut col = 24u16;
        let above: Vec<(u16, u16)> = (0..g.cols as u16).map(|c| (row - 1, c)).collect();
        for k in 0..20u64 {
            if k < 12 {
                t += Duration::from_millis(50);
                arm_exact_backspace(&mut glow, t, (row, col), (row, col - 1));
                col -= 1;
            } else {
                t += Duration::from_millis(50);
            }
            glow.tick(Some((row, col)), t, &c, g, &mut out);
            let mut lit = Glass::new(g, 0);
            lit.add_halos(glow.halos());
            let (x0, y0, _, y1) = lit.cell_rect(row - 1, 0);
            for y in y0..y1 {
                for x in x0..lit.w as i32 {
                    peak = peak.max(lit.level(y, x));
                }
            }
            let f = fire_dark_glass(&glow, &out, g, &above);
            for c in 0..g.cols as u16 {
                let (i, cnt) = glyph(g, row - 1, c);
                worst = worst.min(ratio(&f, &i, &cnt));
            }
        }
        (peak, worst)
    };
    let (peak, worst) = run(false);
    assert!(
        peak < 255,
        "the light over the line above stays under the clip ({peak})"
    );
    assert!(
        worst >= 3.0,
        "the line above reads at {worst:.2}:1 through the run"
    );
    let (control, _) = run(true);
    assert_eq!(
        control, 255,
        "control: unbudgeted steam clips the line above"
    );
}

/// Wide glyphs typed into fire on a dark ground: each CJK key's echo is a
/// two-cell typed hop, and it heats the fire as a Latin key does (the forward
/// momentum law counts the typed echo run, not only a one-cell step), flies
/// no meteor, lights both cells of every glyph, and the newest glyph keeps
/// 4.5:1 under the over-ink account.
#[test]
fn fire_typed_in_wide_glyphs_heats_and_keeps_them_readable() {
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Fire, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let row = 3u16;
    let run = |width: u16| {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let mut t = Instant::now();
        let mut col = 1u16;
        glow.observe_row(row, col, &text, t);
        glow.tick(Some((row, col)), t, &c, g, &mut out);
        for _ in 0..16 {
            t += Duration::from_millis(57);
            text[usize::from(col)] = if width == 2 { '\u{6f22}' } else { 'm' };
            if width == 2 {
                text[usize::from(col) + 1] = '\0';
            }
            col += width;
            glow.note_synthetic_typed(t, width);
            glow.observe_row(row, col, &text, t);
            glow.tick(Some((row, col)), t, &c, g, &mut out);
        }
        (glow, out, col)
    };
    let (latin, _, _) = run(1);
    let (wide, out, caret) = run(2);
    assert!(
        latin.disp_t > 0.3,
        "reference: the Latin run heats ({})",
        latin.disp_t
    );
    assert!(
        wide.disp_t >= 0.8 * latin.disp_t,
        "wide glyphs heat the fire as Latin keys do ({} vs {})",
        wide.disp_t,
        latin.disp_t
    );
    assert!(
        wide.fire_meteors.is_empty(),
        "a wide glyph's hop is typing, not a jump"
    );
    for cell in [caret - 2, caret - 1, caret - 4, caret - 3] {
        assert!(
            wide.sparks.iter().any(|s| s.row == row && s.col == cell),
            "both cells of each newest glyph are lit (col {cell})"
        );
    }
    let ink: Vec<(u16, u16)> = (1..caret).map(|col| (row, col)).collect();
    let f = fire_dark_glass(&wide, &out, g, &ink);
    for col in caret - 2..caret {
        let (i, cnt) = glyph(g, row, col);
        let r = ratio(&f, &i, &cnt);
        assert!(
            r >= InkLedger::FLOOR,
            "the newest wide glyph's col {col} reads at {r:.2}:1"
        );
    }
}

/// Reduce Motion leaves fire nothing to draw, so the GPU heat shimmer (which
/// runs only while a frame carries a fire field and glow quads,
/// `GpuRenderer::shimmer_live`) cannot arm: the motion policy hands the
/// cursor family a zero amplitude, and at zero intensity a hot fire emits no
/// field, no quads and no halos.
#[test]
fn reduced_motion_leaves_fire_no_field_for_the_heat_shimmer() {
    let g = geom();
    let mut c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for col in 1..=30u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    assert!(
        !glow.patches().is_empty() && !out.is_empty(),
        "control: a hot fire carries the field and the quads the shimmer reads"
    );
    c.intensity = 0.0;
    t += Duration::from_millis(16);
    glow.tick(Some((2, 30)), t, &c, g, &mut out);
    assert!(
        glow.patches().is_empty(),
        "no fire field under reduced motion"
    );
    assert!(out.is_empty(), "no glow quads under reduced motion");
    assert!(glow.halos().is_empty(), "no halos under reduced motion");
}

/// A dark burst of fire on the tall cell: `keys` keys `gap_ms` apart along
/// row 4 from column 2, the host handing each frame's cells (`above` fills
/// the row over the typed line, `None` leaves it blank); returns the engine.
fn dark_fire_burst(
    keys: u16,
    gap_ms: u64,
    above: Option<&dyn Fn(usize) -> aterm_core::terminal::RenderCell>,
    frame_text: bool,
) -> CursorGlow {
    dark_fire_burst_with(keys, gap_ms, above, frame_text, None, &|_| {}).0
}

/// [`dark_fire_burst`] with the burning row's frame cells coloured by
/// `on_row` (its glyphs stay the typed ones) and `setup` run on the fresh
/// engine first; returns the engine and its last frame's over-ink quads.
fn dark_fire_burst_with(
    keys: u16,
    gap_ms: u64,
    above: Option<&dyn Fn(usize) -> aterm_core::terminal::RenderCell>,
    frame_text: bool,
    on_row: Option<&dyn Fn(usize) -> aterm_core::terminal::RenderCell>,
    setup: &dyn Fn(&mut CursorGlow),
) -> (CursorGlow, Vec<GlowQuad>) {
    let g = tall_geom();
    let (row, start) = (4u16, 2u16);
    let mut c = cfg(GlowStyle::Fire, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut glow = CursorGlow::default();
    setup(&mut glow);
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    let mut t = Instant::now();
    for k in 0..=keys {
        if k > 0 {
            t += Duration::from_millis(gap_ms);
            text[usize::from(start + k - 1)] = 'm';
            glow.note_synthetic_typed(t, 1);
        }
        let caret = start + k;
        glow.observe_row(row, caret, &text, t);
        if frame_text {
            let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for line in &mut rows {
                for cell in line.iter_mut() {
                    (cell.fg, cell.bg) = (rgb(DARK_PAIR.0), rgb(DARK_PAIR.1));
                }
            }
            for (col, &ch) in text.iter().enumerate() {
                if let Some(cell) = on_row {
                    rows[usize::from(row)][col] = cell(col);
                }
                rows[usize::from(row)][col].ch = ch;
            }
            if let Some(cell) = above {
                for (col, slot) in rows[usize::from(row - 1)].iter_mut().enumerate() {
                    *slot = cell(col);
                }
            }
            glow.observe_frame_text(Some((row, caret)), &rows, t);
        }
        glow.tick(Some((row, caret)), t, &c, g, &mut out);
    }
    (glow, out)
}

/// On a dark ground the fire rises in tongues, never as a one-cell slab:
/// over a hot typed run the flame's height climbs and falls off along the
/// line a little at a time from column to column (no square side on a cell
/// edge, where the newest cell's flame once stood alone as a dim maroon
/// column); over the blank row above it burns brighter than the translucent
/// tint it keeps over a line of text; and it dies toward its tips.
#[test]
fn the_dark_fire_rises_in_tongues_not_a_slab() {
    let g = tall_geom();
    let row = 4u16;
    let glow = dark_fire_burst(24, 60, None, true);
    let patches = glow.patches().to_vec();
    assert!(!patches.is_empty(), "the hot run burns");
    let page = rel_luminance(DARK_PAIR.1);
    let light_of = |patches: &[FirePatch], y: i32| -> Vec<f32> {
        let mut glass = Glass::new(g, DARK_PAIR.1);
        glass.add_fire_patches(patches);
        (0..i32::from(g.win_w))
            .map(|x| rel_luminance(glass.at(y, x) & 0x00FF_FFFF) - page)
            .collect()
    };
    let above_mid = i32::from(row - 1) * g.ch as i32 + g.ch as i32 / 2;
    let peak_of = |l: &[f32]| l.iter().copied().fold(0.0f32, f32::max);
    let shipped = light_of(&patches, above_mid);
    let peak = peak_of(&shipped);
    assert!(
        peak > 0.02,
        "the flames rise over the blank row ({peak:.4})"
    );
    let heights = |patches: &[FirePatch]| -> Vec<(u16, f32)> {
        let mut cols: Vec<(u16, f32)> = Vec::new();
        for p in patches {
            let h = f32::from(p.peak_h) * f32::from(p.strength) / 255.0;
            match cols.iter_mut().find(|c| c.0 == p.x) {
                Some(c) => c.1 = c.1.max(h),
                None => cols.push((p.x, h)),
            }
        }
        cols.sort_by_key(|c| c.0);
        cols
    };
    let jump = |cols: &[(u16, f32)]| {
        let tallest = cols.iter().map(|c| c.1).fold(0.0f32, f32::max).max(1.0);
        cols.windows(2)
            .map(|w| (w[1].1 - w[0].1).abs())
            .fold(0.0f32, f32::max)
            / tallest
    };
    let cw = g.cw as i32;
    let cell_of = |p: &FirePatch| (i32::from(p.x) + cw / 2).div_euclid(cw);
    let slab: Vec<FirePatch> = patches
        .iter()
        .map(|p| {
            let best = patches
                .iter()
                .filter(|o| cell_of(o) == cell_of(p))
                .max_by_key(|o| o.peak_h)
                .copied()
                .unwrap_or(*p);
            FirePatch {
                peak_h: best.peak_h,
                ..*p
            }
        })
        .collect();
    let (soft, hard) = (jump(&heights(&patches)), jump(&heights(&slab)));
    assert!(
        soft <= 0.4,
        "a square side in the flames: their height steps {soft:.2} of the tallest between columns"
    );
    assert!(
        hard > 0.4,
        "control: the slab's height steps only {hard:.2} of the tallest"
    );
    let typed_mid = i32::from(row) * g.ch as i32 + g.ch as i32 / 5;
    let tint = peak_of(&light_of(&patches, typed_mid));
    assert!(
        peak >= 2.0 * tint,
        "over air {peak:.4}, over the letters {tint:.4}"
    );
    let mut glass = Glass::new(g, DARK_PAIR.1);
    glass.add_fire_patches(&patches);
    let row_light: Vec<(i32, f32)> = (0..i32::from(row) * g.ch as i32)
        .map(|y| {
            let sum: f32 = (0..i32::from(g.win_w))
                .map(|x| rel_luminance(glass.at(y, x) & 0x00FF_FFFF) - page)
                .sum();
            (y, sum)
        })
        .filter(|&(_, l)| l > 1e-4)
        .collect();
    let n = row_light.len();
    assert!(n >= 6, "the flame stands several pixels tall ({n})");
    let mean = |s: &[(i32, f32)]| s.iter().map(|v| v.1).sum::<f32>() / s.len() as f32;
    let (top, low) = (mean(&row_light[..n / 3]), mean(&row_light[n / 3..]));
    assert!(
        top < 0.6 * low,
        "the tips {top:.4} against the body {low:.4}"
    );
    let text_above = |_: usize| aterm_core::terminal::RenderCell {
        ch: 'm',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        ..Default::default()
    };
    let over_text = dark_fire_burst(24, 60, Some(&text_above), true);
    let held = peak_of(&light_of(over_text.patches(), above_mid));
    assert!(
        held < 0.5 * peak,
        "control: over a line of text the flame burns at {held:.4} of the air's {peak:.4}"
    );
}

/// The contrast halo (the dark ring stamped around a letter the blaze
/// engulfs) goes only around letters lighter than their ground: in a hot
/// run's own row (kept there under the line of text above), a powerline
/// cell of dark ink on a yellow ground gets no ring (it only smeared the
/// stroke), a light letter on the page beside it does.
/// Negative control: with no frame cells handed over (the colours unknown)
/// the yellow cells are ringed as before.
#[test]
fn the_fire_halo_spares_a_dark_letter_on_a_bright_ground() {
    let row = 4u16;
    let yellow_at = |col: usize| col >= 12 && col.is_multiple_of(2);
    let mixed = |col: usize| {
        if yellow_at(col) {
            aterm_core::terminal::RenderCell {
                ch: 'm',
                fg: [0, 0, 0],
                bg: [0xCD, 0xCD, 0x00],
                ..Default::default()
            }
        } else {
            aterm_core::terminal::RenderCell {
                ch: 'm',
                fg: [0xD0, 0xD0, 0xD0],
                bg: [0x11, 0x13, 0x18],
                ..Default::default()
            }
        }
    };
    let ringed = |glow: &CursorGlow, yellow: bool| {
        glow.halo_cells()
            .iter()
            .any(|h| h.row == row && yellow_at(usize::from(h.col)) == yellow)
    };
    let text_above = |_: usize| aterm_core::terminal::RenderCell {
        ch: 'm',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        ..Default::default()
    };
    // A long hot run, so the blaze engulfs its row on both sides of the
    // split.
    let (glow, _) = dark_fire_burst_with(22, 35, Some(&text_above), true, Some(&mixed), &|_| {});
    assert!(
        ringed(&glow, false),
        "a light letter in the blaze is ringed"
    );
    assert!(
        !ringed(&glow, true),
        "a dark letter on the yellow ground is ringed"
    );
    let (blind, _) = dark_fire_burst_with(22, 35, Some(&text_above), false, Some(&mixed), &|_| {});
    assert!(
        ringed(&blind, true),
        "control: with the colours unknown the yellow cells are ringed"
    );
}

/// OVER A BLANK ROW A COOL FIRE STILL RAISES A TONGUE OVER EACH OF THE LAST
/// LETTERS: ten keys at the gallery's cadence (the fire barely warm) light
/// the blank row above each of the four newest letters, the newest the
/// tallest, with a core that reaches orange or yellow rather than a deep red
/// ember, and a flame stands on the typed row with no unlit line between
/// the two (the flame roots on the burning row's top edge and its foot fades
/// in from nothing there; a lit slab under a dim foot leaves a dark seam).
#[test]
fn a_cool_fire_raises_a_tongue_over_each_of_the_last_letters() {
    let g = tall_geom();
    let (row, start, keys) = (4u16, 2u16, 10u16);
    let glow = dark_fire_burst(keys, 60, None, true);
    assert!(
        glow.fire_t() < 0.4,
        "fixture: a cool fire ({:.2})",
        glow.fire_t()
    );
    let patches = glow.patches().to_vec();
    let lit = |patches: &[FirePatch]| {
        let mut glass = Glass::new(g, DARK_PAIR.1);
        glass.add_fire_patches(patches);
        glass
    };
    let glass = lit(&patches);
    let page = rel_luminance(DARK_PAIR.1);
    let (ch, cw) = (g.ch as i32, g.cw as i32);
    let above = (i32::from(row) - 1) * ch..i32::from(row) * ch;
    let tongue = |glass: &Glass, col: u16| {
        let xs = i32::from(col) * cw..(i32::from(col) + 1) * cw;
        let mut best = (0.0f32, 0u32);
        for y in above.clone() {
            for x in xs.clone() {
                let px = glass.at(y, x) & 0x00FF_FFFF;
                let lift = rel_luminance(px) - page;
                if lift > best.0 {
                    best = (lift, px);
                }
            }
        }
        best
    };
    let newest = start + keys - 1;
    let lifts: Vec<(f32, u32)> = (0..4).map(|back| tongue(&glass, newest - back)).collect();
    for back in 0..4u16 {
        let x0 = i32::from(newest - back) * cw;
        let envelope = patches
            .iter()
            .filter(|p| (x0..x0 + cw).contains(&i32::from(p.x)))
            .map(|p| p.peak_h)
            .max()
            .unwrap_or(0);
        assert!(
            i32::from(envelope) >= ch / 2,
            "{back} letters behind the head: the flame's envelope is {envelope} px"
        );
    }
    let shown = lifts.iter().filter(|l| l.0 > 0.03).count();
    assert!(
        shown >= 3 && lifts[0].0 > 0.03 && lifts[1].0 > 0.03,
        "tongues over the four newest letters: {lifts:?}"
    );
    for (back, &(_, px)) in lifts.iter().enumerate().take(2) {
        let (r, gr) = ((px >> 16) & 0xff, (px >> 8) & 0xff);
        assert!(
            gr * 100 >= r * 30,
            "{back} behind the head: the core {px:06x} is a deep red, not orange"
        );
    }
    let seam = |glass: &Glass| {
        let mut worst = 0i32;
        for x in (i32::from(newest) - 3) * cw..(i32::from(newest) + 1) * cw {
            let col: Vec<i32> = (above.start..i32::from(row) * ch + ch / 3)
                .map(|y| glass.level(y, x))
                .collect();
            let Some(top) = (0..col.len()).max_by_key(|&i| col[i]) else {
                continue;
            };
            for i in top + 1..col.len().saturating_sub(1) {
                let over = col[top..i].iter().copied().max().unwrap_or(0);
                let below = col[i + 1..].iter().copied().max().unwrap_or(0);
                worst = worst.max(below.min(over) - col[i]);
            }
        }
        worst
    };
    let shipped = seam(&glass);
    assert!(
        shipped <= 8,
        "an unlit line at the flame's root ({shipped} levels)"
    );
    let body_cap = patches.iter().map(|p| p.cov_cap).max().unwrap_or(0);
    let skirted: Vec<FirePatch> = patches
        .iter()
        .copied()
        .chain(
            patches
                .iter()
                .filter(|p| i32::from(p.y + p.h) == i32::from(p.base_y))
                .map(|p| FirePatch {
                    y: p.base_y,
                    h: 6,
                    row: (p.base_y / g.ch as u16),
                    cov_cap: body_cap,
                    ..*p
                }),
        )
        .collect();
    let control = seam(&lit(&skirted));
    assert!(
        control > shipped + 6,
        "control: a skirt below the root must show the seam ({control} levels)"
    );
    let text_above = |_: usize| aterm_core::terminal::RenderCell {
        ch: 'm',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        ..Default::default()
    };
    let held = dark_fire_burst(keys, 60, Some(&text_above), true);
    let tint = tongue(&lit(held.patches()), newest).0;
    assert!(
        tint < 0.5 * lifts[0].0,
        "control: over a line of text the cool fire keeps to the tint ({tint:.4})"
    );
}

/// THE DARK FIRE BURNS ABOVE THE LETTERS, NOT THROUGH THEM: typed under a
/// blank row, every flame patch lies in the air over the burning row (rooted
/// on its top edge, no dim slab over the letters' ascenders), and the
/// fire's streak lays no light over the typed letters (it read as a warm
/// smear through their x-height). Under a line of text the flame keeps to
/// its own row instead
/// (`under_a_line_of_text_the_dark_fire_burns_in_its_own_row`). Negative
/// control: the streak with its over-ink budget off still lights the
/// letters.
#[test]
fn the_dark_fire_burns_above_the_letters_not_through_them() {
    let g = tall_geom();
    let (row, start, keys) = (4u16, 2u16, 10u16);
    let row_top = i32::from(row) * g.ch as i32;
    let glow = dark_fire_burst(keys, 60, None, true);
    let patches = glow.patches();
    assert!(!patches.is_empty(), "fixture: the fire burns");
    let below = patches
        .iter()
        .filter(|p| i32::from(p.y) + i32::from(p.h) > row_top)
        .count();
    assert_eq!(below, 0, "{below} flame patches lie in the burning row");
    // The streak: the additive light over the typed letters (short of the
    // caret's cell) in the last frame of a hot burst.
    let streak_over_letters = |unbudgeted: bool| -> u64 {
        let mut c = cfg(GlowStyle::Fire, true);
        c.intensity = 1.0;
        c.radius = 0.0;
        (c.theme_fg, c.theme_bg) = DARK_PAIR;
        let mut glow = CursorGlow {
            streak_unbudgeted: unbudgeted,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let mut t = Instant::now();
        for k in 0..=keys {
            if k > 0 {
                t += Duration::from_millis(40);
                text[usize::from(start + k - 1)] = 'm';
                glow.note_synthetic_typed(t, 1);
            }
            glow.observe_row(row, start + k, &text, t);
            glow.tick(Some((row, start + k)), t, &c, g, &mut out);
        }
        let (x0, x1) = (
            i32::from(start) * g.cw as i32,
            i32::from(start + keys) * g.cw as i32,
        );
        let band = row_top..row_top + g.ch as i32;
        out.iter()
            .filter(|q| {
                (x0..x1).contains(&i32::from(q.x)) && band.contains(&i32::from(q.y)) && q.alpha == 0
            })
            .map(|q| {
                u64::from(q.w)
                    * u64::from(q.h)
                    * u64::from(
                        ((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff),
                    )
            })
            .sum()
    };
    let shipped = streak_over_letters(false);
    let unbudgeted = streak_over_letters(true);
    assert!(
        unbudgeted > 0,
        "control: the streak with no budget lights the letters"
    );
    assert!(
        shipped * 20 < unbudgeted,
        "the streak lights the letters: {shipped} against {unbudgeted} unbudgeted"
    );
}

/// The fire's own light (its patches and the cores in the under-ink stream)
/// over a black page in pixel rows `ys`, classed by hue as the judges' census
/// classes it (a pixel of saturation 0.3 and value 0.25 or more): `(orange,
/// yellow, green or blue)` pixels (18–45°, 45–75°, 75° and up), and the light
/// summed along each pixel row.
fn fire_hues(
    glow: &CursorGlow,
    g: Geom,
    ys: std::ops::Range<i32>,
) -> (usize, usize, usize, Vec<u32>) {
    let mut light = Glass::new(g, 0);
    light.add_quads(glow.under_quads());
    light.add_fire_patches(glow.patches());
    let (mut yellow, mut orange, mut green) = (0usize, 0usize, 0usize);
    let mut by_y = vec![0u32; ys.len()];
    for y in ys.clone() {
        for x in 0..i32::from(g.win_w) {
            let px = light.at(y, x) & 0x00FF_FFFF;
            let (r, gr, b) = ((px >> 16) & 0xff, (px >> 8) & 0xff, px & 0xff);
            by_y[(y - ys.start) as usize] += r + gr + b;
            let (h, sat, v) = crate::color_math::rgb2hsv(px);
            if sat < 0.3 || v < 0.25 {
                continue;
            }
            if (75.0..330.0).contains(&h) {
                green += 1;
            } else if (45.0..75.0).contains(&h) {
                yellow += 1;
            } else if (18.0..45.0).contains(&h) {
                orange += 1;
            }
        }
    }
    (orange, yellow, green, by_y)
}

/// UNDER A LINE OF TEXT THE DARK FIRE BURNS IN ITS OWN ROW: typed under a
/// line of text, every flame patch lies in the burning row (rooted under the
/// letters' baseline, dying before the line above), so that line keeps its
/// contrast; the tongues burn orange from a cold start, and at a blaze their
/// cores burn yellow (hue 45° or more, as the judges' census reads yellow)
/// in the band under the letters' baseline, never green, and fall off
/// smoothly toward the row's top (no lid); and every letter they
/// burn under, the newest among them, keeps 4.5:1 or better in the composed
/// frame. Negative controls: the flame let rise into the line above as it
/// once did lays patches there; and with no letter held to the row bar the
/// flame under the typed letters washes one below 4.5:1.
#[test]
fn under_a_line_of_text_the_dark_fire_burns_in_its_own_row() {
    let g = tall_geom();
    let (row, start, keys) = (4u16, 2u16, 10u16);
    let (ch, row_top) = (g.ch as i32, i32::from(row) * g.ch as i32);
    let text_above = |_: usize| aterm_core::terminal::RenderCell {
        ch: 'm',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        ..Default::default()
    };
    let burst = |setup: &dyn Fn(&mut CursorGlow)| {
        dark_fire_burst_with(keys, 60, Some(&text_above), true, None, setup)
    };
    let (glow, out) = burst(&|_| {});
    let patches = glow.patches();
    assert!(!patches.is_empty(), "the fire burns under the line of text");
    let outside = patches
        .iter()
        .filter(|p| i32::from(p.y) < row_top || i32::from(p.y + p.h) > row_top + ch)
        .count();
    assert_eq!(outside, 0, "{outside} flame patches leave the burning row");
    let colours = |glow: &CursorGlow| fire_hues(glow, g, row_top - ch..row_top + ch);
    let (orange, _, green, _) = colours(&glow);
    let (_, _, _, by_y) = fire_hues(&glow, g, row_top..row_top + ch);
    assert_eq!(green, 0, "{green} flame pixels lean green or blue");
    assert!(orange > 0, "a cool fire's cores burn orange ({orange} px)");
    let hot = |setup: &dyn Fn(&mut CursorGlow)| {
        dark_fire_burst_with(30, 57, Some(&text_above), true, None, setup)
    };
    let (blaze, blaze_out) = hot(&|_| {});
    assert!(
        blaze.fire_t() > 0.7,
        "fixture: a blaze ({:.2})",
        blaze.fire_t()
    );
    let (_, yellow, green, _) = colours(&blaze);
    assert_eq!(green, 0, "{green} blaze pixels lean green or blue");
    assert!(yellow > 0, "a blaze's cores burn yellow ({yellow} px)");
    let (coreless, _) = hot(&|glow| glow.fire_coreless = true);
    let (_, field_yellow, _, _) = colours(&coreless);
    assert_eq!(
        field_yellow, 0,
        "control: the field alone burns {field_yellow} yellow px"
    );
    // A smooth falloff: the top quarter of the row carries a small share of
    // the light of its middle half.
    let q = (ch / 4) as usize;
    let (top, middle) = (
        by_y[..q].iter().sum::<u32>() as f32 / q as f32,
        by_y[q..3 * q].iter().sum::<u32>() as f32 / (2 * q) as f32,
    );
    assert!(
        top < 0.35 * middle,
        "the flame stands to the row's top: {top:.0} against {middle:.0} a pixel row"
    );
    // Every letter, the typed ones and the line above's, in the composed
    // frame, of a run `n` keys long.
    let worst = |glow: &CursorGlow, out: &[GlowQuad], n: u16| {
        let mut ink: Vec<(u16, u16)> = (start..start + n).map(|c| (row, c)).collect();
        ink.extend((0..g.cols as u16).map(|c| (row - 1, c)));
        let glass = fire_dark_glass(glow, out, g, &ink);
        let of = |r: u16, cols: std::ops::Range<u16>| {
            cols.map(|c| {
                let (i, cnt) = glyph(g, r, c);
                ratio(&glass, &i, &cnt)
            })
            .fold(f32::INFINITY, f32::min)
        };
        (of(row, start..start + n), of(row - 1, 0..g.cols as u16))
    };
    for (what, glow, out, n) in [
        ("cool", &glow, &out, keys),
        ("blaze", &blaze, &blaze_out, 30),
    ] {
        let (typed, above) = worst(glow, out, n);
        assert!(
            typed >= 4.5,
            "{what}: a letter the flame burns under reads at {typed:.2}:1"
        );
        assert!(above >= 4.5, "{what}: the line above reads at {above:.2}:1");
    }
    let (rises, _) = burst(&|glow| glow.fire_rises_into_text = true);
    let up = rises
        .patches()
        .iter()
        .filter(|p| i32::from(p.y) < row_top)
        .count();
    assert!(
        up > 0,
        "control: the flame let rise lays no patch in the line above"
    );
    let (unheld, unheld_out) = hot(&|glow| glow.fire_row_unheld = true);
    let (washed, _) = worst(&unheld, &unheld_out, 30);
    assert!(
        washed < 4.5,
        "control: unheld, the flame leaves the letters {washed:.2}:1"
    );
}

/// THE DARK FIRE HOLDS ONLY THE TONGUES UNDER INK, AND RISES IN THE AIR
/// WITH YELLOW CORES: with a lone `40` at the far left of the row above
/// (round 1 held the whole row's flame in it, and the band of flames over
/// `fox` was gone), the tongues under the clear cells rise into the row
/// above while none stands over the `40`; and over the blank row a blaze's
/// tongues burn with cores the judges' census reads yellow (hue 45° or
/// more), never green. Negative controls: with letters all along the row
/// above no tongue rises into it; and the field alone (no cores) burns no
/// yellow.
#[test]
fn the_dark_fire_holds_only_the_tongues_under_ink() {
    let g = tall_geom();
    let (row, cw) = (4u16, g.cw as i32);
    let row_top = i32::from(row) * g.ch as i32;
    let forty = |col: usize| aterm_core::terminal::RenderCell {
        ch: match col {
            0 => '4',
            1 => '0',
            _ => ' ',
        },
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        ..Default::default()
    };
    let lettered = |_: usize| aterm_core::terminal::RenderCell {
        ch: 'm',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        ..Default::default()
    };
    let risen = |glow: &CursorGlow| -> (usize, usize) {
        let up: Vec<_> = glow
            .patches()
            .iter()
            .filter(|p| i32::from(p.y) < row_top)
            .collect();
        let over_forty = up.iter().filter(|p| i32::from(p.x) < 2 * cw).count();
        (up.len(), over_forty)
    };
    let (glow, _) = dark_fire_burst_with(30, 57, Some(&forty), true, None, &|_| {});
    let (up, over_forty) = risen(&glow);
    assert!(
        up > 0,
        "the tongues under the clear cells rise into the row above"
    );
    assert_eq!(
        over_forty, 0,
        "{over_forty} flame patches stand over the `40`"
    );
    let (_, yellow, green, _) = fire_hues(&glow, g, row_top - g.ch as i32..row_top + g.ch as i32);
    assert_eq!(green, 0, "{green} flame pixels lean green or blue");
    assert!(
        yellow > 0,
        "a blaze's tongues in the air burn yellow cores ({yellow} px)"
    );
    let (held, _) = dark_fire_burst_with(30, 57, Some(&lettered), true, None, &|_| {});
    assert_eq!(
        risen(&held).0,
        0,
        "control: under a line of letters a tongue rises into it"
    );
    let (coreless, _) = dark_fire_burst_with(30, 57, Some(&forty), true, None, &|glow| {
        glow.fire_coreless = true
    });
    let (_, field_yellow, _, _) =
        fire_hues(&coreless, g, row_top - g.ch as i32..row_top + g.ch as i32);
    assert_eq!(
        field_yellow, 0,
        "control: the field alone burns {field_yellow} yellow px"
    );
}

/// THE FIRE'S SMOKE KEEPS OFF A LINE OF TEXT: a puff whose centre lies over
/// a line the frame shows (a prompt bar, a status line over the burning
/// row) is not drawn, and the same puff over a blank row is.
#[test]
fn the_fire_smoke_keeps_off_a_line_of_text() {
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Fire, true);
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let at = Instant::now();
    let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for cell in rows[3].iter_mut().take(20) {
        cell.ch = 'm';
    }
    let draws = |kind: VaporKind, row: u16| {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.observe_row(4, 30, &vec![' '; g.cols], at);
        glow.observe_frame_text(Some((4, 30)), &rows, at);
        glow.tick(Some((4, 30)), at, &c, g, &mut out);
        glow.vapor.clear();
        let (x, y) = g.cell_center(row, 8);
        glow.vapor.push(Vapor {
            x0: x,
            y0: y,
            vx: 0.0,
            vy: 0.0,
            gy: 0.0,
            life: 2.0,
            seed: 0.0,
            kind,
            born: at,
        });
        let mut halos = Vec::new();
        glow.emit_vapor(at, &c, g, &mut halos);
        !halos.is_empty()
    };
    assert!(!draws(VaporKind::Smoke, 3), "smoke over the line draws");
    assert!(
        draws(VaporKind::Smoke, 1),
        "smoke over a blank row is not drawn"
    );
    assert!(
        draws(VaporKind::Steam, 3),
        "control: steam over the line still draws"
    );
}

/// ON PAPER THE FIRE STANDS UP OUT OF THE LINE: typed under a blank row on
/// GitHub Light, the ink-fire's tongues rise from the letters' x-height
/// well into the row above (paint of a visible opacity reaches more than
/// half a row over the burning row's top), and an ember is a round bead of
/// ember ink, its middle row wider than its top and bottom and its rim the
/// deep red of a smoulder.
#[test]
fn the_light_fire_stands_up_out_of_the_line_and_its_embers_are_beads() {
    let g = tall_geom();
    let (row, start, keys) = (4u16, 2u16, 10u16);
    let mut c = cfg(GlowStyle::Fire, true);
    c.dark_theme = false;
    (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
    c.intensity = 1.0;
    let row_top = i32::from(row) * g.ch as i32;
    let above_paint = |linger_ms: u64| -> (i32, u64) {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let blank = vec![' '; g.cols];
        let mut t = Instant::now();
        for k in 0..=keys {
            if k > 0 {
                t += Duration::from_millis(60);
                text[usize::from(start + k - 1)] = 'm';
                glow.note_synthetic_typed(t, 1);
            }
            glow.observe_row(row, start + k, &text, t);
            glow.observe_neighbor_rows(Some(&blank), Some(&blank));
            glow.tick(Some((row, start + k)), t, &c, g, &mut out);
        }
        if linger_ms > 0 {
            t += Duration::from_millis(linger_ms);
            glow.observe_row(row, start + keys, &text, t);
            glow.observe_neighbor_rows(Some(&blank), Some(&blank));
            glow.tick(Some((row, start + keys)), t, &c, g, &mut out);
        }
        let mut top = row_top;
        let mut sum = 0u64;
        for q in glow.under_quads() {
            let a = q.alpha.max(q.alpha2);
            if i32::from(q.y) < row_top && a >= 24 {
                top = top.min(i32::from(q.y));
                sum += u64::from(a) * u64::from(q.w) * u64::from(q.h);
            }
        }
        (row_top - top, sum)
    };
    let (rise, _) = above_paint(0);
    assert!(
        rise * 2 > g.ch as i32,
        "the tongues reach only {rise} px over the line ({} px a row)",
        g.ch
    );
    let (_, out_cold) = above_paint(2000);
    assert_eq!(
        out_cold, 0,
        "control: the fire out, paint stands over the line"
    );
    let at = Instant::now();
    let mut glow = CursorGlow::default();
    let (x, y) = g.cell_center(row - 2, 10);
    glow.particles.push(Particle {
        x0: x,
        y0: y,
        vx: 0.0,
        vy: 0.0,
        gy: 0.0,
        life: 5.0,
        hue: 0.8,
        cov_scale: 1.0,
        born: at,
    });
    let (mut out, mut halos) = (Vec::new(), Vec::new());
    glow.emit_particles(at, &c, g, &mut out, &mut halos, &mut Vec::new());
    assert!(halos.is_empty(), "a light ember is a bead, not a veil");
    assert!(
        !out.is_empty() && out.iter().all(|q| q.alpha > 0),
        "the ember is ink"
    );
    let rows: std::collections::BTreeMap<u16, u32> =
        out.iter().fold(Default::default(), |mut m, q| {
            *m.entry(q.y).or_default() += u32::from(q.w);
            m
        });
    let widths: Vec<u32> = rows.values().copied().collect();
    let mid = widths[widths.len() / 2];
    assert!(
        widths.len() >= 3 && mid > widths[0] && mid > widths[widths.len() - 1],
        "the ember is round: row widths {widths:?}"
    );
    let rim = out
        .iter()
        .min_by_key(|q| q.x)
        .map(|q| {
            let a = u32::from(q.alpha.max(1));
            let un = |sh: u32| (((q.color >> sh) & 0xff) * 255 / a).min(255);
            (un(16) << 16) | (un(8) << 8) | un(0)
        })
        .unwrap_or(0);
    let (hue, sat, _) = crate::color_math::rgb2hsv(rim);
    assert!(
        hue <= 25.0 && sat >= 0.6,
        "the ember's rim is a smoulder's deep red: {rim:06x}"
    );
}

/// AN ENTER AT THE BOTTOM LAYS NO SPARK ON THE LINES IT PRINTED: a hot run
/// typed on the pane's last row, then Enter: the pane scrolls the command up
/// two rows under its output and the caret leaps home.
#[test]
fn an_enter_at_the_bottom_lays_no_spark_on_the_lines_it_printed() {
    use aterm_core::terminal::RenderCell;
    let g = tall_geom();
    let bottom = g.rows as u16 - 1;
    let (start, keys) = (2u16, 22u16);
    let mut c = cfg(GlowStyle::Fire, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let frame = |lines: &[(u16, &[char])]| {
        let mut rows = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(DARK_PAIR.0), rgb(DARK_PAIR.1));
            }
        }
        for &(row, text) in lines {
            for (col, &ch) in text.iter().enumerate() {
                rows[usize::from(row)][col].ch = ch;
            }
        }
        rows
    };
    let run = |unbudgeted: bool, enter: bool| -> (usize, usize) {
        let mut glow = CursorGlow {
            streak_unbudgeted: unbudgeted,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        text[0] = '$';
        let mut t = Instant::now();
        for k in 0..=keys {
            if k > 0 {
                t += Duration::from_millis(40);
                text[usize::from(start + k - 1)] = 'm';
                glow.note_synthetic_typed(t, 1);
            }
            let caret = start + k;
            glow.observe_row(bottom, caret, &text, t);
            glow.observe_frame_text(Some((bottom, caret)), &frame(&[(bottom, &text)]), t);
            glow.tick(Some((bottom, caret)), t, &c, g, &mut out);
        }
        t += Duration::from_millis(30);
        let enter_at = t;
        let mut output = vec![' '; g.cols];
        output[..19].fill('m');
        let mut prompt = vec![' '; g.cols];
        prompt[0] = '$';
        let (rows, landing) = if enter {
            glow.note_scroll(2);
            (
                frame(&[
                    (bottom - 2, &text),
                    (bottom - 1, &output),
                    (bottom, &prompt),
                ]),
                (bottom, 2u16),
            )
        } else {
            (frame(&[(bottom, &text)]), (bottom, start + keys + 16))
        };
        let line = if enter { &prompt } else { &text };
        glow.note_synthetic_move(t);
        glow.observe_row(landing.0, landing.1, line, t);
        glow.observe_frame_text(Some(landing), &rows, t);
        glow.tick(Some(landing), t, &c, g, &mut out);
        let printed = |row: i32, col: i32| {
            row >= 0
                && (row as usize) < g.rows
                && rows[row as usize]
                    .get(col as usize)
                    .is_some_and(|c| c.ch != ' ')
        };
        t += Duration::from_millis(40);
        glow.observe_row(landing.0, landing.1, line, t);
        glow.observe_frame_text(Some(landing), &rows, t);
        glow.tick(Some(landing), t, &c, g, &mut out);
        let on_letters = glow
            .halos()
            .iter()
            .filter(|h| h.rx <= g.ch as u16 && h.mode == HaloMode::Add)
            .filter(|h| {
                let (cx, cy) = (f32::from(h.cx), f32::from(h.cy));
                let (rx, ry) = (f32::from(h.rx) * 0.6, f32::from(h.ry) * 0.6);
                [(0.0, 0.0), (-rx, 0.0), (rx, 0.0), (0.0, -ry), (0.0, ry)]
                    .iter()
                    .any(|&(dx, dy)| {
                        let row = ((cy + dy) / g.ch as f32).floor() as i32;
                        let col = ((cx + dx) / g.cw as f32).floor() as i32;
                        printed(row, col)
                    })
            })
            .count();
        t += Duration::from_millis(60);
        glow.observe_row(landing.0, landing.1, line, t);
        glow.observe_frame_text(Some(landing), &rows, t);
        glow.tick(Some(landing), t, &c, g, &mut out);
        let alive = glow
            .particles
            .iter()
            .filter(|p| {
                p.born < enter_at && t.saturating_duration_since(p.born).as_secs_f32() < p.life
            })
            .count();
        (on_letters, alive)
    };
    let (on_letters, alive) = run(false, true);
    assert_eq!(
        on_letters, 0,
        "{on_letters} embers land on the printed letters"
    );
    assert_eq!(
        alive, 0,
        "{alive} of the run's sparks still fly 100 ms after Enter"
    );
    let (bare, _) = run(true, true);
    assert!(
        bare > 0,
        "control: with the over-ink laws off the carried sparks land on the letters"
    );
    let (_, kept) = run(false, false);
    assert!(
        kept > 0,
        "control: a leap forward along the line keeps its sparks"
    );
}

/// THE DARK TONGUES STAND ON CURLED FEET, NOT ONE FLAT BASE: over a blank
/// row the lowest lit pixel of the flame rises and falls along the burning
/// run (each tongue's foot curls up at its sides), so the flames never read
/// as bars on a common baseline.
#[test]
fn the_dark_tongues_stand_on_curled_feet() {
    let g = tall_geom();
    let (row, start, keys) = (4u16, 2u16, 24u16);
    let feet = |flat: bool| -> i32 {
        let mut c = cfg(GlowStyle::Fire, true);
        c.intensity = 1.0;
        (c.theme_fg, c.theme_bg) = DARK_PAIR;
        let mut glow = CursorGlow {
            fire_flat_feet: flat,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let mut t = Instant::now();
        for k in 0..=keys {
            if k > 0 {
                t += Duration::from_millis(60);
                text[usize::from(start + k - 1)] = 'm';
                glow.note_synthetic_typed(t, 1);
            }
            glow.observe_row(row, start + k, &text, t);
            let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for (col, &ch) in text.iter().enumerate() {
                rows[usize::from(row)][col].ch = ch;
            }
            glow.observe_frame_text(Some((row, start + k)), &rows, t);
            glow.tick(Some((row, start + k)), t, &c, g, &mut out);
        }
        let mut glass = Glass::new(g, DARK_PAIR.1);
        glass.add_fire_patches(glow.patches());
        let row_top = i32::from(row) * g.ch as i32;
        let newest = i32::from(start + keys - 1);
        let lows: Vec<i32> = ((newest - 3) * g.cw as i32..(newest + 1) * g.cw as i32)
            .filter_map(|x| {
                (row_top - g.ch as i32..row_top)
                    .rev()
                    .find(|&y| glass.level(y, x) > 0x13 + 12)
            })
            .collect();
        assert!(
            lows.len() > 8,
            "fixture: the flame is lit ({} columns)",
            lows.len()
        );
        lows.iter().max().unwrap_or(&0) - lows.iter().min().unwrap_or(&0)
    };
    let curled = feet(false);
    let flat = feet(true);
    assert!(
        curled >= g.ch as i32 / 6,
        "the feet stand on one line: they vary by {curled} px"
    );
    assert!(
        flat < curled,
        "control: on one flat base the feet vary by {flat} px (curled {curled})"
    );
}

/// A LIGHT TONGUE NEVER CLIMBS INTO ANOTHER LINE'S LETTERS: typed under a
/// line of text, the light fire's tongues stand only as tall as reaches that
/// line's foot (shorter, but there): no paint lands in its letters' cells,
/// while the burning row keeps its tongues.
#[test]
fn a_light_tongue_never_climbs_into_the_line_above() {
    let g = tall_geom();
    let (row, start, keys) = (4u16, 2u16, 12u16);
    let mut c = cfg(GlowStyle::Fire, true);
    c.dark_theme = false;
    (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
    c.intensity = 1.0;
    let row_top = i32::from(row) * g.ch as i32;
    let paint = |above: &[char]| -> (u64, u64) {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let blank = vec![' '; g.cols];
        let mut t = Instant::now();
        for k in 0..=keys {
            if k > 0 {
                t += Duration::from_millis(50);
                text[usize::from(start + k - 1)] = 'm';
                glow.note_synthetic_typed(t, 1);
            }
            glow.observe_row(row, start + k, &text, t);
            glow.observe_neighbor_rows(Some(above), Some(&blank));
            glow.tick(Some((row, start + k)), t, &c, g, &mut out);
        }
        let (mut in_letters, mut own) = (0u64, 0u64);
        for q in glow.under_quads() {
            let a = u64::from(q.alpha.max(q.alpha2));
            let (y, x) = (i32::from(q.y), i32::from(q.x));
            let col = x / g.cw as i32;
            if (row_top - g.ch as i32..row_top).contains(&y)
                && above.get(col as usize).is_some_and(|&ch| ch != ' ')
            {
                in_letters += a * u64::from(q.w) * u64::from(q.h);
            } else if (row_top..row_top + g.ch as i32).contains(&y) {
                own += a * u64::from(q.w) * u64::from(q.h);
            }
        }
        (in_letters, own)
    };
    let mut line = vec![' '; g.cols];
    line[..40].fill('m');
    let (in_letters, own) = paint(&line);
    assert_eq!(in_letters, 0, "the tongues climb into the letters above");
    assert!(own > 0, "under a line of text the fire still burns");
    let blank = vec![' '; g.cols];
    let mut glow_cells = 0u64;
    {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let mut t = Instant::now();
        for k in 0..=keys {
            if k > 0 {
                t += Duration::from_millis(50);
                text[usize::from(start + k - 1)] = 'm';
                glow.note_synthetic_typed(t, 1);
            }
            glow.observe_row(row, start + k, &text, t);
            glow.observe_neighbor_rows(Some(&blank), Some(&blank));
            glow.tick(Some((row, start + k)), t, &c, g, &mut out);
        }
        for q in glow.under_quads() {
            let y = i32::from(q.y);
            if (row_top - g.ch as i32..row_top).contains(&y)
                && (i32::from(q.x) / (g.cw as i32)) < 40
            {
                glow_cells += u64::from(q.alpha.max(q.alpha2)) * u64::from(q.w) * u64::from(q.h);
            }
        }
    }
    assert!(
        glow_cells > 0,
        "control: under a blank row the tongues climb into it"
    );
}

/// How many pixels of `glass` in window rows `ys` read olive: a dim yellow
/// over the dark (hue 45°-70°, value 0.15-0.55, saturation over 0.35), the
/// judges' census (round 2: `(95,91,42)` at the dark tongues' roots).
fn olive_px(glass: &Glass, g: Geom, ys: std::ops::Range<i32>) -> usize {
    let mut n = 0;
    for y in ys {
        for x in 0..i32::from(g.win_w) {
            let (h, s, v) = crate::color_math::rgb2hsv(glass.at(y, x) & 0x00FF_FFFF);
            if (45.0..70.0).contains(&h) && s > 0.35 && (0.15..0.55).contains(&v) {
                n += 1;
            }
        }
    }
    n
}

/// THE DARK FIRE IS NEVER OLIVE (round-2 judges, item 2: the tongues over a
/// blank row rooted in olive, `(95,91,42)`, and the cores of a kept flame
/// thinned to khaki). Over a blank row and kept under a line of text, cool
/// and in a blaze, no pixel of the composed frame round the burning row
/// reads olive: where a core's light thins (its foot, its sides, a thin
/// core) it burns amber or orange, and a tongue over a blank row fades in
/// no longer than the field's own cooled root. Negative control: the cores
/// burnt in their height's colour alone ([`CursorGlow::fire_core_uncooled`])
/// lay olive the census reads.
#[test]
fn the_dark_fire_is_never_olive() {
    let g = tall_geom();
    let (row, start) = (4u16, 2u16);
    let ch = g.ch as i32;
    let text_above = |_: usize| aterm_core::terminal::RenderCell {
        ch: 'm',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        ..Default::default()
    };
    let census = |uncooled: bool| -> usize {
        let mut n = 0;
        for (keys, gap) in [(10u16, 60u64), (30, 57)] {
            for above in [None, Some(&text_above as &dyn Fn(usize) -> _)] {
                let (glow, out) = dark_fire_burst_with(keys, gap, above, true, None, &|glow| {
                    glow.fire_core_uncooled = uncooled;
                });
                let mut ink: Vec<(u16, u16)> = (start..start + keys).map(|c| (row, c)).collect();
                if above.is_some() {
                    ink.extend((0..g.cols as u16).map(|c| (row - 1, c)));
                }
                let glass = fire_dark_glass(&glow, &out, g, &ink);
                n += olive_px(
                    &glass,
                    g,
                    (i32::from(row) - 3) * ch..(i32::from(row) + 1) * ch,
                );
            }
        }
        n
    };
    let shipped = census(false);
    let uncooled = census(true);
    assert_eq!(shipped, 0, "{shipped} pixels of the dark fire read olive");
    assert!(
        uncooled > 20,
        "control: uncooled cores lay only {uncooled} olive pixels"
    );
}

/// THE KEPT FLAME BURNS ITS BODY OVER THE LETTERS (round-2 judges, items 1
/// and 7: under a line of text the dark flame was a hot line at the baseline
/// under dim brown tongues, a stubby flat-topped strip). Kept in its row under
/// a line of text, cool and in a blaze, over the newest four letters: the
/// flame's silhouette fades to the row's top with no one-pixel step down any
/// letter-free column over 35% of that column's brightest; it lays at least
/// two thirds as much light in their counters as in the band under the
/// baseline; and every letter it burns under keeps 6.2:1 (the row's bar) in
/// the composed frame. Negative control: burnt as round 2 burnt it
/// ([`CursorGlow::fire_kept_strip`]) the strip's flat top steps down at
/// least 60% of its brightest in one pixel.
#[test]
fn the_kept_flame_burns_its_body_over_the_letters() {
    let g = tall_geom();
    let (row, start) = (4u16, 2u16);
    let text_above = |_: usize| aterm_core::terminal::RenderCell {
        ch: 'm',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        ..Default::default()
    };
    let page = DARK_PAIR.1;
    let lift = |p: u32| -> f32 {
        [16, 8, 0]
            .iter()
            .map(|&sh| ((p >> sh) & 0xff).saturating_sub((page >> sh) & 0xff) as f32)
            .sum()
    };
    let (top, ch) = (i32::from(row) * g.ch as i32, g.ch as i32);
    for (what, keys, gap) in [("cool", 10u16, 60u64), ("blaze", 30, 57)] {
        // `(hardest step, counter light, under-baseline light, worst
        // contrast)` over the newest four letters.
        let run = |strip: bool| -> (f32, f32, f32, f32) {
            let (glow, out) =
                dark_fire_burst_with(keys, gap, Some(&text_above), true, None, &|glow| {
                    glow.fire_kept_strip = strip;
                });
            let mut ink: Vec<(u16, u16)> = (start..start + keys).map(|c| (row, c)).collect();
            ink.extend((0..g.cols as u16).map(|c| (row - 1, c)));
            let glass = fire_dark_glass(&glow, &out, g, &ink);
            let mut worst = f32::INFINITY;
            let (mut cut, mut body, mut nb, mut low, mut nl) =
                (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for col in start..start + keys {
                let (i, cnt) = glyph(g, row, col);
                worst = worst.min(ratio(&glass, &i, &cnt));
                if col + 4 < start + keys {
                    continue;
                }
                for &(y, x) in &cnt {
                    body += lift(glass.at(y, x));
                    nb += 1.0;
                }
                let x0 = i32::from(col) * g.cw as i32;
                for y in top + ch * 8 / 10..top + ch {
                    for x in x0..x0 + g.cw as i32 {
                        low += lift(glass.at(y, x));
                        nl += 1.0;
                    }
                }
                // The columns no stroke of the glyph crosses.
                for dx in [0, 1, 4, 5, 8, 9, 12] {
                    let col_lift: Vec<f32> = (top..top + ch * 3 / 4)
                        .map(|y| lift(glass.at(y, x0 + dx)))
                        .collect();
                    let peak = col_lift.iter().copied().fold(0.0, f32::max);
                    if peak < 30.0 {
                        continue;
                    }
                    for w in col_lift.windows(2) {
                        cut = cut.max((w[1] - w[0]) / peak);
                    }
                }
            }
            (cut, body / nb.max(1.0), low / nl.max(1.0), worst)
        };
        let (cut, body, low, worst) = run(false);
        assert!(
            worst >= CursorGlow::FIRE_DARK_ROW_BAR,
            "{what}: a letter the kept flame burns under reads at {worst:.2}:1"
        );
        assert!(
            cut <= 0.35,
            "{what}: the kept flame's silhouette steps {cut:.3} of its brightest in one pixel"
        );
        assert!(
            body * 3.0 >= low * 2.0,
            "{what}: the kept flame lays {body:.1} in the counters against {low:.1} under the baseline"
        );
        let (s_cut, ..) = run(true);
        assert!(
            s_cut >= 0.6,
            "control, {what}: round 2's strip steps only {s_cut:.3} of its brightest in one pixel"
        );
    }
}

/// A FIRE METEOR STREAKS WITHOUT A TICK AT ITS JOINS (round-2 judges, item 7:
/// fire dark's Return leap stood a hard vertical tick in its root, where the
/// white head's wedge began). Over its flight a dark Return meteor's
/// brightest light per pixel column rises to its head and falls away from it
/// with no other column standing over both its neighbours by more than a
/// twentieth of the streak's peak, and no column-to-column step over half
/// of it (the head eases in from the orange behind it over most of a cell
/// and feathers into the landing over half of one). Negative control: its
/// pieces drawn each from its own colour and width to their joins unsnapped
/// and its head whole to the landing ([`CursorGlow::fire_meteor_joined`])
/// add twice in the column they share and stand cut at the landing: a tick
/// of at least a fifth of the peak and a step of at least four fifths.
#[test]
fn a_fire_meteor_streaks_without_a_tick_at_its_joins() {
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Fire, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let floor = rel_luminance(DARK_PAIR.1);
    // `(tick, step)` over the flight.
    let run = |joined: bool| -> (f32, f32) {
        let mut glow = CursorGlow {
            fire_meteor_joined: joined,
            ..CursorGlow::default()
        };
        let t0 = Instant::now();
        let mut out = Vec::new();
        glow.tick(Some((3, 30)), t0, &c, g, &mut out);
        glow.note_synthetic_move(t0);
        glow.tick(Some((4, 2)), t0, &c, g, &mut out);
        assert_eq!(glow.fire_meteors.len(), 1, "the Return flies one meteor");
        let life = glow.fire_meteors[0].life;
        let (mut tick, mut step) = (0.0f32, 0.0f32);
        // In flight, before the strike erupts at the landing.
        for k in 1..=5 {
            out.clear();
            let now = t0 + Duration::from_secs_f32(life * k as f32 / 10.0);
            glow.tick(Some((4, 2)), now, &c, g, &mut out);
            let glass = fire_dark_glass(&glow, &out, g, &[]);
            let col: Vec<f32> = (0..g.win_w as i32)
                .map(|x| {
                    (0..g.win_h as i32)
                        .map(|y| rel_luminance(glass.at(y, x) & 0x00FF_FFFF) - floor)
                        .fold(0.0, f32::max)
                })
                .collect();
            let (head, peak) = col
                .iter()
                .copied()
                .enumerate()
                .fold((0, 0.0f32), |a, (x, v)| if v > a.1 { (x, v) } else { a });
            assert!(peak > 0.1, "the meteor is lit in flight ({peak:.3})");
            for (i, w) in col.windows(3).enumerate() {
                if i + 1 != head {
                    tick = tick.max((w[1] - w[0].max(w[2])) / peak);
                }
                step = step.max((w[1] - w[0]).abs() / peak);
            }
        }
        (tick, step)
    };
    let (tick, step) = run(false);
    assert!(
        tick <= 0.05,
        "a column of the meteor stands {tick:.3} of its peak over its neighbours"
    );
    assert!(
        step <= 0.5,
        "the meteor steps {step:.3} of its peak from one column to the next"
    );
    let (joined, j_step) = run(true);
    assert!(
        joined >= 0.2 && j_step >= 0.8,
        "control: drawn as round 2 drew it its pieces stand only {joined:.3} over a neighbour \
         and step {j_step:.3}"
    );
}

/// The page's cells for a dark typing-halo run on row 4 of [`tall_geom`]:
/// all air, or (`inked`) a prompt's bar on the row above (a yellow segment
/// of letters, then a grey bar) and a selected line of letters below.
fn halo_rows(inked: bool) -> Vec<Vec<aterm_core::terminal::RenderCell>> {
    let g = tall_geom();
    let row = 4usize;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for line in &mut rows {
        for cell in line.iter_mut() {
            (cell.fg, cell.bg) = (rgb(DARK_PAIR.0), rgb(DARK_PAIR.1));
        }
    }
    if inked {
        for (col, cell) in rows[row - 1].iter_mut().enumerate() {
            if col < 22 {
                (cell.ch, cell.fg, cell.bg) = ('m', [0, 0, 0], rgb(0x00D7_D700));
            } else {
                cell.bg = rgb(0x0033_3333);
            }
        }
        for cell in &mut rows[row + 1] {
            (cell.ch, cell.bg) = ('m', rgb(0x0034_4766));
        }
    }
    rows
}

/// The momentum glow's own quads (the host's frame past its twin's without
/// that glow) after 24 keys at 57 ms on `row` of [`tall_geom`] from column
/// 2, over `rows`, the host set up by `setup` first.
fn fire_typing_halo(
    row: u16,
    rows: Vec<Vec<aterm_core::terminal::RenderCell>>,
    setup: &dyn Fn(&mut crate::cursor_fx::CursorFx),
) -> Vec<GlowQuad> {
    let start = 2u16;
    let mut host = FireHost::new(true, false);
    let mut twin = FireHost::new(false, false);
    setup(&mut host.fx);
    host.rows = Some(rows.clone());
    twin.rows = Some(rows);
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    twin.frame(t, (row, start), false);
    for k in 1..=24u16 {
        t += Duration::from_millis(57);
        host.frame(t, (row, start + k), true);
        twin.frame(t, (row, start + k), true);
    }
    let n = twin.fx.glow_scratch.len();
    assert!(
        host.fx.glow_scratch.starts_with(&twin.fx.glow_scratch),
        "fixture: the aurora's quads lead the frame, momentum glow or not"
    );
    host.fx.glow_scratch[n..].to_vec()
}

/// THE FIRE'S TYPING HALO IS THE FORGE'S WARM LIGHT (round-2 judges, item 6:
/// fire dark's caret halo read as a grey box). Typed hot on a dark page, the
/// momentum glow's additive light round a fire caret carries at least 40% of
/// its brightest channel as colour (warm: red over blue), and no channel of
/// it is brighter than the ramp laid it. Negative control: left as the
/// momentum ramp lays it ([`crate::cursor_fx::CursorFx::momentum_unwarmed`])
/// its white-hot grey carries under a quarter.
#[test]
fn the_fire_typing_halo_is_the_forges_warm_light() {
    let chroma = |quads: &[GlowQuad]| -> (f32, f32, u64) {
        let (mut sat, mut top, mut warm_minus_cool) = (0.0f32, 0.0f32, 0i64);
        for q in quads.iter().filter(|q| q.alpha == 0) {
            let area = f32::from(q.w) * f32::from(q.h);
            for c in [q.color, q.color2] {
                let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
                let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
                sat += (hi - lo) as f32 * area;
                top += hi as f32 * area;
                warm_minus_cool += (i64::from(r) - i64::from(b)) * area as i64;
            }
        }
        (sat / top.max(1.0), top, warm_minus_cool.max(0) as u64)
    };
    let shipped = fire_typing_halo(4, halo_rows(false), &|_| {});
    let unwarmed = fire_typing_halo(4, halo_rows(false), &|fx| fx.momentum_unwarmed = true);
    let (sat, top, warm) = chroma(&shipped);
    let (u_sat, u_top, _) = chroma(&unwarmed);
    assert!(top > 0.0, "fixture: the halo is lit");
    assert!(
        sat >= 0.4,
        "the fire's typing halo carries only {sat:.3} colour"
    );
    assert!(warm > 0, "the fire's typing halo leans cool");
    assert!(
        top <= u_top * 1.001,
        "warming brightened the halo: {top} against {u_top}"
    );
    assert!(
        u_sat < 0.25,
        "control: the momentum ramp's halo carries {u_sat:.3} colour"
    );
}

/// THE TYPING HALO EASES OFF A NEIGHBOUR ROW WITH NO EDGE (round-2 judges,
/// item 6: fire dark's caret halo stood a box with hard horizontal edges at
/// the caret row's edges, where the hold off the prompt's bar and the
/// selected line began). Typed hot between a bar and a selection, the
/// momentum glow's light, composed on the page and averaged across the
/// columns round the caret, falls across each edge of the caret's row with
/// no one-pixel step over three tenths of its level in the row (the halo's
/// bands are two pixels tall, so an even fall over a dozen pixels steps a
/// little under a quarter). Negative control: held off from the edge itself
/// ([`CursorGlow::neighbour_hold_hard`]) it steps half its level there.
#[test]
fn the_typing_halo_eases_off_a_neighbour_row_with_no_edge() {
    let g = tall_geom();
    let (row, caret) = (4i32, 26i32);
    let page = DARK_PAIR.1;
    let step = |quads: &[GlowQuad]| -> f32 {
        let mut glass = Glass::new(g, page);
        glass.add_quads(quads);
        let lift = |p: u32| -> f32 {
            [16, 8, 0]
                .iter()
                .map(|&sh| ((p >> sh) & 0xff).saturating_sub((page >> sh) & 0xff) as f32)
                .sum()
        };
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let prof: Vec<f32> = ((row - 1) * ch..(row + 2) * ch)
            .map(|y| {
                ((caret - 3) * cw..(caret + 1) * cw)
                    .map(|x| lift(glass.at(y, x)))
                    .sum::<f32>()
            })
            .collect();
        let level = prof[ch as usize..2 * ch as usize]
            .iter()
            .copied()
            .fold(0.0, f32::max);
        assert!(level > 0.0, "fixture: the halo lights the caret's row");
        prof.windows(2)
            .map(|w| (w[1] - w[0]).abs() / level)
            .fold(0.0, f32::max)
    };
    let eased = step(&fire_typing_halo(4, halo_rows(true), &|_| {}));
    let hard = step(&fire_typing_halo(4, halo_rows(true), &|fx| {
        fx.glow.neighbour_hold_hard = true;
    }));
    assert!(
        eased <= 0.3,
        "the halo steps {eased:.3} of its level in one pixel at the caret row's edge"
    );
    assert!(
        hard >= 0.45,
        "control: held off from the edge the halo steps only {hard:.3}"
    );
}

/// THE FIRE'S TYPING HALO MELTS INTO THE GRID'S BOTTOM (round-2 judges, item
/// 6: fire dark's caret halo stood cut along the grid's bottom). Typed hot
/// on the last row of a dark page, the momentum glow lays in the effects
/// box's last pixel row at most a fifth of what the same run on a row mid
/// grid lays at the same offset under its caret's row top. Negative control:
/// mid grid that offset is lit, so a straight cut at the box's edge would
/// have stood there.
#[test]
fn the_fire_typing_halo_melts_into_the_grid_bottom() {
    let g = tall_geom();
    let lit_at = |quads: &[GlowQuad], y: i32| -> u64 {
        quads
            .iter()
            .filter(|q| q.alpha == 0 && i32::from(q.y) <= y && y < i32::from(q.y) + i32::from(q.h))
            .map(|q| {
                let sum = |c: u32| u64::from(((c >> 16) & 0xff) + ((c >> 8) & 0xff) + (c & 0xff));
                (sum(q.color) + sum(q.color2)) * u64::from(q.w)
            })
            .sum()
    };
    let last_row = g.rows as u16 - 1;
    let bottom = fire_typing_halo(last_row, halo_rows(false), &|_| {});
    let mid = fire_typing_halo(4, halo_rows(false), &|_| {});
    let last = g.fx_bot() - 1;
    let offset = last - i32::from(last_row) * g.ch as i32;
    let edge = lit_at(&bottom, last);
    let twin = lit_at(&mid, 4 * g.ch as i32 + offset);
    assert!(
        edge * 5 <= twin,
        "the last row's halo lays {edge} along the box's edge, against {twin} mid-grid"
    );
    assert!(
        twin > 0,
        "control: mid-grid the halo lays nothing at that offset"
    );
}

/// SPARKLE'S TYPING HALO ON PAPER IS ITS GOLD (round-2 judges, item 9: the
/// glow round sparkle's caret on paper read caramel). Typed hot on white,
/// the momentum glow's ink round a sparkle caret (automatic colour) stands
/// within 2.5° of hue of the paper gold sparkle's body lays (its mean hue,
/// weighted by opacity and area). Negative control: worn in the caret's own
/// colour ([`crate::cursor_fx::CursorFx::momentum_unwarmed`]) its ink stands
/// at least 5° toward orange of that gold, the caramel the judges saw.
#[test]
fn sparkle_typing_halo_on_paper_is_its_gold() {
    const PAPER: (u32, u32) = (0x001F_2328, 0x00FF_FFFF);
    let (row, start) = (4u16, 2u16);
    let ink = |round2: bool| -> f32 {
        let host_of = |momentum| {
            let mut h = FireHost::styled(GlowStyle::Sparkle, PAPER, momentum, false);
            h.cfg.color = crate::cursor_glow::SPARKLE_DEFAULT_COLOR;
            h.fx.momentum_unwarmed = round2;
            h
        };
        let (mut host, mut twin) = (host_of(true), host_of(false));
        let mut t = Instant::now();
        host.frame(t, (row, start), false);
        twin.frame(t, (row, start), false);
        for k in 1..=24u16 {
            t += Duration::from_millis(57);
            host.frame(t, (row, start + k), true);
            twin.frame(t, (row, start + k), true);
        }
        // The halo's ink, wherever it was laid: the host's streams past its
        // twin's, weighted by opacity × area.
        let mut quads: Vec<GlowQuad> = host.fx.glow_scratch[twin.fx.glow_scratch.len()..].to_vec();
        let under = host.fx.glow.under_quads();
        quads.extend_from_slice(&under[twin.fx.glow.under_quads().len().min(under.len())..]);
        let (mut hx, mut hy) = (0.0f32, 0.0f32);
        for q in quads.iter().filter(|q| q.alpha > 0) {
            let (h, _, _) = crate::color_math::rgb2hsv(q.color);
            let k = f32::from(q.alpha) * f32::from(q.w) * f32::from(q.h);
            hx += k * h.to_radians().cos();
            hy += k * h.to_radians().sin();
        }
        assert!(hx.hypot(hy) > 0.0, "fixture: the halo lays ink on paper");
        hy.atan2(hx).to_degrees().rem_euclid(360.0)
    };
    let (gold, _, _) = crate::color_math::rgb2hsv(CursorGlow::SPARKLE_PAPER_CORE);
    let hue = ink(false);
    assert!(
        (hue - gold).abs() <= 2.5,
        "sparkle's paper halo stands at {hue:.1}°, its gold at {gold:.1}°"
    );
    let caramel = ink(true);
    assert!(
        gold - caramel >= 5.0,
        "control: worn in the caret's colour the halo stands at {caramel:.1}°, the gold at {gold:.1}°"
    );
}

/// THE KEPT FLAME HOLDS EACH HEIGHT AT ITS OWN HEAT (round-2 judges, item 1:
/// the kept flame's tongues were a dim brown over the letters). Kept in its
/// row under a line of text, a cool run's flame is held over each letter at
/// the light its field lays at each height, not at its hottest anywhere: in
/// the composed frame it lays at least a tenth more light in the counters of
/// the letters it burns under than the same flame held at its hottest, and
/// every letter keeps the row's bar. Negative control: held at its hottest
/// ([`CursorGlow::fire_held_hottest`]) it is the dimmer of the two.
#[test]
fn the_kept_flame_holds_each_height_at_its_own_heat() {
    let g = tall_geom();
    let (row, start, keys) = (4u16, 2u16, 10u16);
    let text_above = |_: usize| aterm_core::terminal::RenderCell {
        ch: 'm',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        ..Default::default()
    };
    let page = DARK_PAIR.1;
    let lift = |p: u32| -> f32 {
        [16, 8, 0]
            .iter()
            .map(|&sh| ((p >> sh) & 0xff).saturating_sub((page >> sh) & 0xff) as f32)
            .sum()
    };
    let run = |hottest: bool| -> (f32, f32) {
        let (glow, out) = dark_fire_burst_with(keys, 60, Some(&text_above), true, None, &|glow| {
            glow.fire_held_hottest = hottest;
        });
        let mut ink: Vec<(u16, u16)> = (start..start + keys).map(|c| (row, c)).collect();
        ink.extend((0..g.cols as u16).map(|c| (row - 1, c)));
        let glass = fire_dark_glass(&glow, &out, g, &ink);
        let (mut worst, mut body, mut n) = (f32::INFINITY, 0.0f32, 0.0f32);
        for col in start..start + keys {
            let (i, cnt) = glyph(g, row, col);
            worst = worst.min(ratio(&glass, &i, &cnt));
            for &(y, x) in &cnt {
                body += lift(glass.at(y, x));
                n += 1.0;
            }
        }
        (body / n.max(1.0), worst)
    };
    let (body, worst) = run(false);
    let (hot_body, _) = run(true);
    assert!(
        worst >= CursorGlow::FIRE_DARK_ROW_BAR,
        "a letter the kept flame burns under reads at {worst:.2}:1"
    );
    assert!(
        body >= hot_body * 1.1,
        "held at each height's heat the flame lays {body:.1} over the letters, at its hottest {hot_body:.1}"
    );
    assert!(
        hot_body > 0.0,
        "control: held at its hottest the flame lays nothing"
    );
}

/// THE KEPT FLAME'S CORES BURN GOLD (round-2 judges, item 1: a dark flame
/// kept in its row under a line of text wants yellow cores in its words'
/// gaps and under its baseline; round 3's first capture burnt them amber,
/// not a gold pixel among them). Typed as `echo the quick brown fox` under a
/// line of text, quick and slower, the newest six cells show gold (hue
/// 38°-58°, value 0.6 or more) in the word's gap and under the letters'
/// baseline, and nothing round the row reads olive. Negative control: each
/// core burnt as round 3's first capture burnt it
/// ([`CursorGlow::fire_kept_core_from_base`]) lays no gold in the gap.
#[test]
fn the_kept_flames_cores_burn_gold_in_the_gaps_and_under_the_line() {
    for gap_ms in [57u64, 90] {
        let ((under, blank), olive) = kept_phrase_gold(gap_ms, false);
        assert!(
            blank >= 6,
            "{gap_ms} ms apart: the word's gap shows {blank} gold pixels"
        );
        assert!(
            under >= 5,
            "{gap_ms} ms apart: under the baseline the letters show {under} gold pixels"
        );
        assert_eq!(olive, 0, "{gap_ms} ms apart: {olive} pixels read olive");
        let ((_, c_blank), _) = kept_phrase_gold(gap_ms, true);
        assert_eq!(
            c_blank, 0,
            "control, {gap_ms} ms apart: cores burnt from their root lay {c_blank} gold pixels in the gap"
        );
    }
}

/// A dark fire kept in its row under a line of text (a row of `m` above),
/// typed as `echo the quick brown fox` `gap_ms` apart: the gold pixels
/// (hue 38°-58°, saturation over 0.4, value 0.6 or more) its last six cells
/// show under their letters' baseline (`.0`) and in their blank cells
/// (`.1`), and the olive ones round the row.
fn kept_phrase_gold(gap_ms: u64, from_base: bool) -> ((usize, usize), usize) {
    let g = tall_geom();
    let (row, start) = (4u16, 2u16);
    let phrase: Vec<char> = "echo the quick brown fox".chars().collect();
    let mut c = cfg(GlowStyle::Fire, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut glow = CursorGlow {
        fire_kept_core_from_base: from_base,
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    let mut t = Instant::now();
    let caret = start + phrase.len() as u16;
    for k in 0..=phrase.len() as u16 {
        if k > 0 {
            t += Duration::from_millis(gap_ms);
            text[usize::from(start + k - 1)] = phrase[usize::from(k - 1)];
            glow.note_synthetic_typed(t, 1);
        }
        let at = start + k;
        glow.observe_row(row, at, &text, t);
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(DARK_PAIR.0), rgb(DARK_PAIR.1));
            }
        }
        for (col, &ch) in text.iter().enumerate() {
            rows[usize::from(row)][col].ch = ch;
        }
        for slot in rows[usize::from(row - 1)].iter_mut() {
            slot.ch = 'm';
        }
        glow.observe_frame_text(Some((row, at)), &rows, t);
        glow.tick(Some((row, at)), t, &c, g, &mut out);
    }
    let mut ink: Vec<(u16, u16)> = (start..caret)
        .filter(|&col| text[usize::from(col)] != ' ')
        .map(|col| (row, col))
        .collect();
    ink.extend((0..g.cols as u16).map(|c| (row - 1, c)));
    let glass = fire_dark_glass(&glow, &out, g, &ink);
    let (top, ch, cw) = (i32::from(row) * g.ch as i32, g.ch as i32, g.cw as i32);
    let gold = |p: u32| {
        let (h, s, v) = crate::color_math::rgb2hsv(p & 0x00FF_FFFF);
        (38.0..58.0).contains(&h) && s > 0.4 && v >= 0.6
    };
    let (mut under, mut blank) = (0usize, 0usize);
    for col in caret - 6..caret {
        let x0 = i32::from(col) * cw;
        let is_blank = text[usize::from(col)] == ' ';
        let ys = if is_blank {
            top..top + ch
        } else {
            top + ch * 8 / 10..top + ch
        };
        for y in ys {
            for x in x0..x0 + cw {
                if gold(glass.at(y, x)) {
                    if is_blank {
                        blank += 1;
                    } else {
                        under += 1;
                    }
                }
            }
        }
    }
    let olive = olive_px(&glass, g, top - 3 * ch..top + ch);
    ((under, blank), olive)
}
