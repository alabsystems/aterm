// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fire style: meteors, the Emberforge thermal laws, the forge field.

use super::*;

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

/// FIRE KEEPS UNDER THE BOX'S TOP: on a grid with no head band over it (the Settings card's,
/// a pane under a chrome strip) a blaze on the second row rises past the grid's top, and its
/// gold cores (dark) and paper tongues (light) are laid shorter inside the effects box, each
/// tip tapering out under [`RISE_CLEAR_PX`], so no light stands on the box's top pixel row.
/// The negative control (`set_rise_cut`): cut by the box's clip, both stood flat on that row
/// (settings round 2's three-row card, 2 to 6 px a frame).
#[test]
fn a_blaze_under_the_grids_top_keeps_inside_the_box() {
    let g = Geom {
        cw: 9,
        ch: 16,
        rows: 3,
        cols: 48,
        origin_x: 0,
        origin_y: 0,
        win_w: 432,
        win_h: 48,
        head: 0,
    };
    let on_top_row = |dark: bool, cut: bool| {
        let mut c = cfg(GlowStyle::Fire, true);
        c.intensity = 1.0;
        c.dark_theme = dark;
        (c.theme_fg, c.theme_bg) = if dark {
            DARK_PAIR
        } else {
            (0x001F_2328, 0x00FF_FFFF)
        };
        let mut glow = CursorGlow::default();
        glow.set_rise_cut(cut);
        let (mut out, mut text, blank) = (Vec::new(), vec![' '; g.cols], vec![' '; g.cols]);
        let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        let page = aterm_core::terminal::RenderCell {
            fg: rgb(c.theme_fg),
            bg: rgb(c.theme_bg),
            ..aterm_core::terminal::RenderCell::default()
        };
        let mut cells = vec![vec![page; g.cols]; g.rows];
        let mut t = Instant::now();
        glow.tick(Some((1, 2)), t, &c, g, &mut out);
        let (mut drawn, mut on_top) = (0, 0);
        for i in 1..=30u16 {
            t += Duration::from_millis(45);
            text[usize::from(1 + i)] = 'm';
            cells[1][usize::from(1 + i)].ch = 'm';
            glow.observe_row(1, 2 + i, &text, t);
            glow.observe_neighbor_rows(Some(&blank), Some(&blank));
            glow.observe_frame_text(Some((1, 2 + i)), &cells, t);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((1, 2 + i)), t, &c, g, &mut out);
            // Additive quads (opacity 0) light by their colour alone.
            let lit = glow
                .under_quads()
                .iter()
                .filter(|q| q.color | q.color2 != 0 || q.alpha > 0 || q.alpha2 > 0);
            for q in lit {
                drawn += 1;
                if f32::from(q.y) < g.rise_ceiling() {
                    on_top += 1;
                }
            }
        }
        (drawn, on_top)
    };
    for dark in [true, false] {
        let side = if dark { "dark" } else { "light" };
        let (drawn, on_top) = on_top_row(dark, false);
        assert!(drawn > 0, "{side}: the blaze draws");
        assert_eq!(
            on_top, 0,
            "{side}: {on_top} quads of the blaze on the box's top row"
        );
        let (_, cut) = on_top_row(dark, true);
        assert!(
            cut > 0,
            "negative control ({side}): the box's clip cuts the blaze's top"
        );
    }
}

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
            edge_cut: false,
        });
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_particles(
            at,
            &c,
            g,
            &mut out,
            &mut halos,
            &mut crate::cursor_glow::glow_sink(
                &mut Vec::new(),
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            CursorGlow::MAX_QUADS,
        );
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

fn fire_frame_is_additive(glow: &CursorGlow, out: &[GlowQuad]) -> bool {
    glow.halos().iter().any(|h| h.mode == HaloMode::Add)
        || out.iter().chain(glow.under_quads()).any(|q| q.alpha == 0)
        || glow.patches().iter().any(|p| p.mode == FireMode::Add)
}

#[test]
fn fire_on_a_light_ground_is_ink_never_light() {
    // `(ceilings off, coarse roots)`: the shipped body has neither.
    const SHIPPED: (bool, bool) = (false, false);
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
               (unsolved, coarse): (bool, bool),
               reds: (u16, u16),
               mixed: bool,
               each: &mut dyn FnMut(&CursorGlow, &[GlowQuad], u16)| {
        let (mut glow, mut out, mut text) = (CursorGlow::default(), Vec::new(), vec![' '; g.cols]);
        // The controls' seams: the ceilings off (`unsolved`), and the roots laid in coarse
        // slabs (`coarse`).
        glow.light_body_unsolved = unsolved;
        glow.fire_light_root_coarse = coarse;
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
    let judge = |frame: bool, seams: (bool, bool), reds: (u16, u16)| {
        let mut w = [f32::INFINITY; 4];
        let (glow, out, t) = run(frame, false, seams, reds, false, &mut |glow, out, caret| {
            let f = composite(glow, out, caret, reds, false);
            w[0] = w[0].min(worst(
                &f,
                row,
                &mut (caret - 3..caret).filter(|c| !(caret - reds.0..caret - reds.1).contains(c)),
            ));
            w[1] = w[1].min(worst(&f, row, &mut (caret - reds.0..caret - reds.1)));
            for above in [row - 1, row - 2] {
                let cols = || caret.saturating_sub(16)..caret;
                w[2] = w[2].min(worst(&f, above, &mut cols().filter(|c| c % 2 == 0)));
                w[3] = w[3].min(worst(&f, above, &mut cols().filter(|c| c % 2 == 1)));
            }
        });
        (w, glow, out, t)
    };
    let (handed, _, _, _) = judge(true, SHIPPED, (6, 3));
    assert!(
        handed[0] >= 4.5,
        "the newest letters read at {:.2}:1",
        handed[0]
    );
    // A known row's cells are each held at their own bar, 4.5:1 for the
    // red ink (5.36:1 bare); 3:1 is the bar of a row whose text is unread.
    assert!(
        handed[1] >= 4.5,
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
    let (probed, mut glow, mut out, t) = judge(false, SHIPPED, (6, 3));
    assert!(
        probed[0] >= 4.5 && probed[2] >= 4.5 && probed[3] >= 3.0,
        "with the rows above unread: newest {:.2}:1, old output {:.2}:1, old red {:.2}:1",
        probed[0],
        probed[2],
        probed[3]
    );
    let (red_head, _, _, _) = judge(true, SHIPPED, (3, 0));
    assert!(
        red_head[1] >= 4.5,
        "a red letter at the caret reads at {:.2}:1",
        red_head[1]
    );
    // The ceilings' control, under the shipped feather: only the ceilings
    // come off, and the red letters they held fall under their bar.
    let (ceilingless, _, _, _) = judge(true, (true, false), (3, 0));
    assert!(
        ceilingless[1] < 4.5 && ceilingless[1] < red_head[1],
        "control: with no ceilings a red letter at the caret reads at {:.2}:1 ({:.2}:1 held)",
        ceilingless[1],
        red_head[1]
    );
    let (ceilingless, _, _, _) = judge(true, (true, false), (6, 3));
    assert!(
        ceilingless[1] < 4.5,
        "control: with no ceilings a red letter on the typed line reads at {:.2}:1",
        ceilingless[1]
    );

    let mixed_run = |seams: (bool, bool)| {
        let mut minimum = f32::INFINITY;
        let mut frames = 0;
        let mut scanlines = 0;
        run(true, false, seams, (6, 3), true, &mut |glow, out, caret| {
            let f = composite(glow, out, caret, (6, 3), true);
            for col in caret - 8..caret {
                let (ink, counters) = glyph(g, row, col);
                let first = ink.first().expect("the captured glyph has stems").0;
                let last = ink.last().expect("the captured glyph has stems").0;
                for y in first..=last {
                    let stems: Vec<_> = ink.iter().copied().filter(|p| p.0 == y).collect();
                    let ground: Vec<_> = counters.iter().copied().filter(|p| p.0 == y).collect();
                    assert!(
                        !stems.is_empty() && !ground.is_empty(),
                        "each captured glyph scanline has stems and adjacent counters"
                    );
                    minimum = minimum.min(ratio(&f, &stems, &ground));
                    scanlines += 1;
                }
            }
            frames += 1;
        });
        assert!(
            frames > 0 && scanlines >= 8 * 13,
            "the mixed-colour run judges actual painted frames and glyph scanlines"
        );
        minimum
    };
    let mixed = mixed_run(SHIPPED);
    assert!(
        (4.5..20.0).contains(&mixed),
        "the light ink stays readable over visible flame on its dark cell ({mixed:.2}:1)"
    );
    // The roots' feather's control: with the ceilings off, roots laid in the flame's coarse
    // slabs obscure the light ink the feathered roots keep clear.
    let uncapped_mixed = mixed_run((true, true));
    assert!(
        uncapped_mixed < 4.5,
        "control: the uncapped ramp on coarse roots obscures the light ink ({uncapped_mixed:.2}:1)"
    );

    let (mut flame_above, mut flame_faint) = (1.0f32, 1.0f32);
    run(false, true, SHIPPED, (6, 3), false, &mut |glow, _, _| {
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

fn unpremul(c: u32, a: u8) -> u32 {
    if a == 0 {
        return 0;
    }
    let ch = |sh: u32| ((((c >> sh) & 0xff) * 255 + u32::from(a) / 2) / u32::from(a)).min(255);
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

const DARK_PAIR: (u32, u32) = (0x00D0_D0D0, 0x0011_1318);

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

struct FireHost {
    fx: crate::cursor_fx::CursorFx,
    cfg: GlowConfig,
    momentum_glow: bool,
    text: Vec<char>,
    rows: Option<Vec<Vec<aterm_core::terminal::RenderCell>>>,
    /// The letters a key types, column by column (none: `m`).
    typed: Vec<char>,
    /// The frames' grid ([`tall_geom`] unless a test sets another).
    geom: Geom,
}

impl FireHost {
    fn new(momentum_glow: bool, unbudgeted: bool) -> Self {
        Self::styled(GlowStyle::Fire, DARK_PAIR, momentum_glow, unbudgeted)
    }

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
            typed: Vec::new(),
            geom: tall_geom(),
        }
    }

    fn frame(&mut self, now: Instant, cur: (u16, u16), key: bool) {
        let g = self.geom;
        self.fx.begin(true, true);
        if key {
            self.fx.cadence.on_keystroke(now);
            self.fx
                .momentum
                .on_key(now, crate::cursor_momentum::MOMENTUM_GLOW_TAU_S);
            self.fx.glow.note_typed_cells(now, 1);
            let col = usize::from(cur.1) - 1;
            self.text[col] = self.typed.get(col).copied().unwrap_or('m');
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
            accent_pinned: false,
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

fn dark_fire_burst(
    keys: u16,
    gap_ms: u64,
    above: Option<&dyn Fn(usize) -> aterm_core::terminal::RenderCell>,
    frame_text: bool,
) -> CursorGlow {
    dark_fire_burst_with(keys, gap_ms, above, frame_text, None, &|_| {}).0
}

fn dark_fire_burst_with(
    keys: u16,
    gap_ms: u64,
    above: Option<&dyn Fn(usize) -> aterm_core::terminal::RenderCell>,
    frame_text: bool,
    on_row: Option<&dyn Fn(usize) -> aterm_core::terminal::RenderCell>,
    setup: &dyn Fn(&mut CursorGlow),
) -> (CursorGlow, Vec<GlowQuad>) {
    fire_burst_on(
        DARK_PAIR,
        (keys, gap_ms),
        (above, on_row),
        frame_text,
        setup,
    )
}

/// A row's cells by column, for [`fire_burst_on`].
type RowCells<'a> = Option<&'a dyn Fn(usize) -> aterm_core::terminal::RenderCell>;

/// [`dark_fire_burst_with`] on any `(fg, bg)` pair: a light page burns as
/// paper fire.
fn fire_burst_on(
    pair: (u32, u32),
    keys: (u16, u64),
    rows: (RowCells<'_>, RowCells<'_>),
    frame_text: bool,
    setup: &dyn Fn(&mut CursorGlow),
) -> (CursorGlow, Vec<GlowQuad>) {
    style_burst_on(GlowStyle::Fire, pair, keys, rows, frame_text, setup)
}

/// [`fire_burst_on`] in any style.
fn style_burst_on(
    style: GlowStyle,
    pair: (u32, u32),
    (keys, gap_ms): (u16, u64),
    (above, on_row): (RowCells<'_>, RowCells<'_>),
    frame_text: bool,
    setup: &dyn Fn(&mut CursorGlow),
) -> (CursorGlow, Vec<GlowQuad>) {
    let g = tall_geom();
    let (row, start) = (4u16, 2u16);
    let mut c = cfg(style, true);
    c.intensity = 1.0;
    c.dark_theme = aterm_render::theme_is_dark(pair.1);
    (c.theme_fg, c.theme_bg) = pair;
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
                    (cell.fg, cell.bg) = (rgb(pair.0), rgb(pair.1));
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
    let q = (ch / 4) as usize;
    let (top, middle) = (
        by_y[..q].iter().sum::<u32>() as f32 / q as f32,
        by_y[q..3 * q].iter().sum::<u32>() as f32 / (2 * q) as f32,
    );
    assert!(
        top < 0.35 * middle,
        "the flame stands to the row's top: {top:.0} against {middle:.0} a pixel row"
    );
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
    // Over a line typed without a gap the row's tongues stand only past its
    // end, so the control lays the crest that burns under every letter.
    let (unheld, unheld_out) = hot(&|glow| {
        glow.fire_row_unheld = true;
        glow.fire_kept_crest = true;
    });
    let (washed, _) = worst(&unheld, &unheld_out, 30);
    assert!(
        washed < 4.5,
        "control: unheld, the flame leaves the letters {washed:.2}:1"
    );
}

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
        edge_cut: false,
    });
    let (mut out, mut halos) = (Vec::new(), Vec::new());
    glow.emit_particles(
        at,
        &c,
        g,
        &mut out,
        &mut halos,
        &mut crate::cursor_glow::glow_sink(
            &mut Vec::new(),
            &mut aterm_render::GlowRamps::default(),
            true,
        ),
        CursorGlow::MAX_QUADS,
    );
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
                    // The control lays one core per tongue on its axis, on one floor.
                    glow.fire_even_cores = uncooled;
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

#[test]
fn a_fire_meteor_streaks_without_a_tick_at_its_joins() {
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Fire, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let floor = rel_luminance(DARK_PAIR.1);
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

#[test]
fn the_fire_typing_halo_is_the_forges_warm_light() {
    // The light's saturation, its brightest channel, how much warmer than
    // cool it leans, and its hue (degrees) over the halo's area.
    let chroma = |quads: &[GlowQuad]| -> (f32, f32, u64, f32) {
        let (mut sat, mut top, mut warm_minus_cool) = (0.0f32, 0.0f32, 0i64);
        let mut sum = [0.0f32; 3];
        for q in quads.iter().filter(|q| q.alpha == 0) {
            let area = f32::from(q.w) * f32::from(q.h);
            for c in [q.color, q.color2] {
                let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
                let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
                sat += (hi - lo) as f32 * area;
                top += hi as f32 * area;
                warm_minus_cool += (i64::from(r) - i64::from(b)) * area as i64;
                for (s, v) in sum.iter_mut().zip([r, g, b]) {
                    *s += v as f32 * area;
                }
            }
        }
        let [r, g, b] = sum;
        let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
        let hue = if hi > lo && hi == r {
            60.0 * (g - b) / (hi - lo)
        } else {
            90.0
        };
        (sat / top.max(1.0), top, warm_minus_cool.max(0) as u64, hue)
    };
    let shipped = fire_typing_halo(4, halo_rows(false), &|_| {});
    let unwarmed = fire_typing_halo(4, halo_rows(false), &|fx| fx.momentum_unwarmed = true);
    // The control: a halo warmed toward the rod's amber (#ff9632).
    let amber = fire_typing_halo(4, halo_rows(false), &|fx| {
        fx.momentum_fire_ember = Some(0x00FF_9632);
    });
    let (sat, top, warm, hue) = chroma(&shipped);
    let (u_sat, u_top, _, u_hue) = chroma(&unwarmed);
    let (_, _, _, a_hue) = chroma(&amber);
    assert!(top > 0.0, "fixture: the halo is lit");
    // An ember's orange-red, never the tan a dim gold or amber makes of a
    // dark page's blue-grey.
    assert!(
        hue <= 25.0,
        "the fire's typing halo burns hue {hue:.1} degrees, a tan"
    );
    assert!(
        u_hue > 25.0 && a_hue > 25.0,
        "control: the momentum ramp's halo burns hue {u_hue:.1} degrees, the amber-warmed \
         one {a_hue:.1}"
    );
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

/// looks7 M5: the fire's tongues and embers stood over a word icon (the
/// fox's chin on paper in `enter-bottom`'s `enter`, embers by its cheek on
/// dark), the icons' shade lifting the ribbons' light alone. Typed under a
/// face over the two rows above, once each frame's icons are named no fire
/// light (a quad, a halo, a field patch) lies on the icon, on either page,
/// and the embers keep off it whole. Negative control: unshaded, the
/// frames lay fire on it.
#[test]
fn a_word_icon_wears_no_fire() {
    let g = tall_geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    // A face over columns 8..14 of rows 2 and 3, above the typed row 4.
    let icon = (8 * cw, 14 * cw, 2 * ch, 4 * ch);
    let hits = |(x0, x1, y0, y1): (i32, i32, i32, i32)| {
        x0 < icon.1 && icon.0 < x1 && y0 < icon.3 && icon.2 < y1
    };
    let rect = |x: u16, y: u16, w: u16, h: u16| {
        let (x, y) = (i32::from(x), i32::from(y));
        (x, x + i32::from(w), y, y + i32::from(h))
    };
    let on_icon = |dark: bool, shade: bool| -> usize {
        let mut c = cfg(GlowStyle::Fire, true);
        c.intensity = 1.0;
        if dark {
            (c.theme_fg, c.theme_bg) = DARK_PAIR;
        } else {
            c.dark_theme = false;
            c.theme_fg = 0x001F_2328;
            c.theme_bg = 0x00FF_FFFF;
        }
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = Instant::now();
        glow.tick(Some((4, 2)), t, &c, g, &mut out);
        let mut n = 0;
        for col in 3..20u16 {
            t += ms(60);
            glow.note_typed_glyph(t, 1, false, rk::TypedClass::Glyph);
            glow.tick(Some((4, col)), t + ms(3), &c, g, &mut out);
            glow.note_word_icons([icon]);
            // The streams as a host publishes them, before its word pass.
            let mut under = glow.under_quads().to_vec();
            let mut halos = glow.halos().to_vec();
            let mut patches = glow.patches().to_vec();
            if shade {
                // This crate's unit tests fold the ramps into the quads
                // (`CursorGlow::fold_glow_ramps`): the streams carry none.
                glow.shade_word_icons((
                    (&mut out, &mut GlowRamps::default()),
                    (&mut under, &mut GlowRamps::default()),
                    &mut halos,
                    &mut patches,
                ));
            }
            n += out
                .iter()
                .chain(&under)
                .filter(|q| q.w > 0 && q.h > 0 && hits(rect(q.x, q.y, q.w, q.h)))
                .count()
                + halos
                    .iter()
                    .filter(|h| h.w > 0 && h.h > 0 && hits(rect(h.x, h.y, h.w, h.h)))
                    .count()
                + patches
                    .iter()
                    .filter(|p| p.w > 0 && p.h > 0 && hits(rect(p.x, p.y, p.w, p.h)))
                    .count();
        }
        n
    };
    for dark in [true, false] {
        assert!(
            on_icon(dark, false) > 0,
            "control (dark {dark}): unshaded, the fire lays no light on the icon"
        );
        let lit = on_icon(dark, true);
        assert_eq!(
            lit, 0,
            "{lit} fire lights lie on the word icon (dark {dark})"
        );
    }
}

/// The kept flame's head burns its gold core in the blank cells at the
/// caret (its own cell and the one ahead), with no olive round the row; its
/// core burnt from its root (the negative control) lays clearly less.
#[test]
fn the_kept_flames_cores_burn_gold_in_the_gaps() {
    for gap_ms in [57u64, 90] {
        let gold = kept_phrase_gold(gap_ms, false, false);
        assert!(
            gold.lead >= 6,
            "{gap_ms} ms apart: the caret's blank cells show {} gold pixels",
            gold.lead
        );
        assert_eq!(
            gold.olive, 0,
            "{gap_ms} ms apart: {} pixels read olive",
            gold.olive
        );
        let base = kept_phrase_gold(gap_ms, true, false);
        assert!(
            base.lead + 6 <= gold.lead,
            "control, {gap_ms} ms apart: cores burnt from their root lay {} gold pixels at the \
             caret against {}",
            base.lead,
            gold.lead
        );
    }
}

/// looks10 L2, A DENSE ROW BURNS A FLAME: kept in its row under a line of
/// text (the trail gallery's `context`), the dark fire led at the air's lead
/// and stood a spark by the ember caret, and the row read as no fire. The
/// cell past the caret stands a low flame: lit 12 px or more of the row's
/// height and burning gold (4 px or more), with no olive round the row (nor
/// in the dark fire's olive census, `the_dark_fire_is_never_olive`, which a
/// lead past 0.75 of the head's root breaks at the lead tongue's root by a
/// pixel or three). Negative control: at
/// the air's lead (`fire_kept_lead_air`) it stands 6 px rows shorter or more.
#[test]
fn a_dense_row_burns_a_flame_past_the_caret() {
    let g = tall_geom();
    let (row, cw, ch) = (4i32, g.cw as i32, g.ch as i32);
    let gold = |p: u32| {
        let (h, s, v) = crate::color_math::rgb2hsv(p & 0x00FF_FFFF);
        (38.0..58.0).contains(&h) && s > 0.4 && v >= 0.6
    };
    for gap_ms in [57u64, 90] {
        let stand = |air_lead: bool| {
            let (glass, _, caret, _) = kept_phrase(gap_ms, &|glow| {
                glow.fire_kept_lead_air = air_lead;
            });
            let x0 = i32::from(caret + 1) * cw;
            let top = row * ch;
            let lit_rows = (top..top + ch)
                .filter(|&y| (x0..x0 + cw).any(|x| dark_lift(glass.at(y, x)) >= 120.0))
                .count();
            let golden = (top..top + ch)
                .flat_map(|y| (x0..x0 + cw).map(move |x| (y, x)))
                .filter(|&(y, x)| gold(glass.at(y, x)))
                .count();
            (
                lit_rows,
                golden,
                olive_px(&glass, g, top - 3 * ch..top + ch),
            )
        };
        let (tall, golden, olive) = stand(false);
        assert!(
            tall >= 12 && golden >= 4,
            "{gap_ms} ms apart: the cell past the caret lights {tall} px rows, {golden} gold"
        );
        assert_eq!(olive, 0, "{gap_ms} ms apart: {olive} pixels read olive");
        let (spark, ..) = stand(true);
        assert!(
            spark + 6 <= tall,
            "control, {gap_ms} ms apart: at the air's lead it lights {spark} px rows of {tall}"
        );
    }
}

/// looks7 M6: kept in its row under a line of text, the dark flame stood a
/// short gold tongue in each word's gap between the burning letters, a
/// brown flank round it, and in the gallery's `context` it read as a comma
/// (`brown, fox`). Its word's gap lays no gold and next to no light; with
/// the tongue stood there (the negative control) the gap burns gold.
#[test]
fn a_kept_word_gap_stands_no_comma() {
    for gap_ms in [57u64, 90] {
        let gold = kept_phrase_gold(gap_ms, false, false);
        assert_eq!(
            gold.gap, 0,
            "{gap_ms} ms apart: the word's gap shows {} gold pixels",
            gold.gap
        );
        assert!(
            gold.gap_lift <= 12.0,
            "{gap_ms} ms apart: the word's gap lays {:.1} of light",
            gold.gap_lift
        );
        let stood = kept_phrase_gold(gap_ms, false, true);
        assert!(
            stood.gap >= 6,
            "control, {gap_ms} ms apart: its tongue stood, the gap shows only {} gold pixels",
            stood.gap
        );
    }
}

fn kept_phrase(gap_ms: u64, setup: &dyn Fn(&mut CursorGlow)) -> (Glass, Vec<char>, u16, usize) {
    let g = tall_geom();
    let (row, start) = (4u16, 2u16);
    let phrase: Vec<char> = "echo the quick brown fox".chars().collect();
    let mut c = cfg(GlowStyle::Fire, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut glow = CursorGlow::default();
    setup(&mut glow);
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
    let rings = glow.halo_cells().iter().filter(|h| h.row == row).count();
    (fire_dark_glass(&glow, &out, g, &ink), text, caret, rings)
}

/// [`kept_phrase_gold`]'s count.
struct KeptGold {
    /// Gold pixels in the word's gap among the last six cells.
    gap: usize,
    /// The word's gap's mean light ([`dark_lift`]).
    gap_lift: f32,
    /// Gold pixels in the caret's cell and the one ahead.
    lead: usize,
    /// Olive pixels round the row.
    olive: usize,
}

/// [`kept_phrase`]'s gold pixels (hue 38°-58°, saturation over 0.4, value
/// 0.6 or more) in the word's gap among its last six cells and in the
/// caret's blank cells, and the olive ones round the row.
fn kept_phrase_gold(gap_ms: u64, from_base: bool, gap_tongues: bool) -> KeptGold {
    let g = tall_geom();
    let row = 4u16;
    let (glass, text, caret, _) = kept_phrase(gap_ms, &|glow| {
        glow.fire_kept_core_from_base = from_base;
        glow.fire_gap_tongues = gap_tongues;
    });
    let (top, ch, cw) = (i32::from(row) * g.ch as i32, g.ch as i32, g.cw as i32);
    let gold = |p: u32| {
        let (h, s, v) = crate::color_math::rgb2hsv(p & 0x00FF_FFFF);
        (38.0..58.0).contains(&h) && s > 0.4 && v >= 0.6
    };
    let mut out = KeptGold {
        gap: 0,
        gap_lift: 0.0,
        lead: 0,
        olive: olive_px(&glass, g, top - 3 * ch..top + ch),
    };
    for col in caret - 6..=caret + 1 {
        if col < caret && text[usize::from(col)] != ' ' {
            continue;
        }
        let x0 = i32::from(col) * cw;
        let (mut lit, mut n) = (0.0f32, 0usize);
        for y in top..top + ch {
            for x in x0..x0 + cw {
                let p = glass.at(y, x);
                let is_gold = usize::from(gold(p));
                if col < caret {
                    out.gap += is_gold;
                    lit += dark_lift(p);
                    n += 1;
                } else {
                    out.lead += is_gold;
                }
            }
        }
        if n > 0 {
            out.gap_lift = out.gap_lift.max(lit / n as f32);
        }
    }
    out
}

/// The light a frame lays over the dark page at `p`: its channels' lift over
/// the page's, summed.
fn dark_lift(p: u32) -> f32 {
    let page = DARK_PAIR.1;
    [16, 8, 0]
        .iter()
        .map(|&sh| ((p >> sh) & 0xff).saturating_sub((page >> sh) & 0xff) as f32)
        .sum()
}

/// Typed as `echo the quick brown fox` under a line of text, quick and slower, over the newest
/// eight cells and the caret's: the brightest blank cell lays at least three times the light that
/// the brightest letter's counters carry, every letter keeps the row's bar (6.2:1), and the blaze
/// rings no letter of the row. Negative control: laid as one crest along the line
/// ([`CursorGlow::fire_kept_crest`]), the counters carry at least half the gaps' light and the
/// blaze rings them.
#[test]
fn the_kept_flame_rises_in_the_gaps_not_over_the_letters() {
    let g = tall_geom();
    let row = 4u16;
    let (top, ch, cw) = (i32::from(row) * g.ch as i32, g.ch as i32, g.cw as i32);
    // `(brightest gap, brightest letter, worst contrast, rings)`.
    let run = |gap_ms: u64, crest: bool| -> (f32, f32, f32, usize) {
        let (glass, text, caret, rings) = kept_phrase(gap_ms, &|glow| {
            glow.fire_kept_crest = crest;
        });
        let (mut gap, mut letter, mut worst) = (0.0f32, 0.0f32, f32::INFINITY);
        for col in caret - 8..=caret {
            let x0 = i32::from(col) * cw;
            if col == caret || text[usize::from(col)] == ' ' {
                let px: Vec<f32> = (top..top + ch * 8 / 10)
                    .flat_map(|y| (x0..x0 + cw).map(move |x| (y, x)))
                    .map(|(y, x)| dark_lift(glass.at(y, x)))
                    .collect();
                gap = gap.max(px.iter().sum::<f32>() / px.len() as f32);
            } else {
                let (i, cnt) = glyph(g, row, col);
                worst = worst.min(ratio(&glass, &i, &cnt));
                let lit = cnt
                    .iter()
                    .map(|&(y, x)| dark_lift(glass.at(y, x)))
                    .sum::<f32>();
                letter = letter.max(lit / cnt.len() as f32);
            }
        }
        (gap, letter, worst, rings)
    };
    for gap_ms in [57u64, 90] {
        let (gap, letter, worst, rings) = run(gap_ms, false);
        assert!(
            gap >= 30.0 && gap >= 3.0 * letter,
            "{gap_ms} ms apart: the gaps lay {gap:.1}, the letters' counters {letter:.1}"
        );
        assert!(
            worst >= CursorGlow::FIRE_DARK_ROW_BAR,
            "{gap_ms} ms apart: a letter beside the flame reads at {worst:.2}:1"
        );
        assert_eq!(
            rings, 0,
            "{gap_ms} ms apart: the blaze rings {rings} letters"
        );
        let (c_gap, c_letter, _, c_rings) = run(gap_ms, true);
        assert!(
            c_letter * 2.0 >= c_gap && c_rings > 0,
            "control, {gap_ms} ms apart: one crest lays {c_letter:.1} in the counters against \
             {c_gap:.1} in the gaps and rings {c_rings} letters"
        );
    }
}

/// Typed as `echo the quick brown fox` under a line of text, quick and slower, over the newest
/// eight cells' letters with no descender: down every pixel column under the baseline the light
/// changes from one pixel row to the next by at most a fifth of the band's brightest, and every
/// letter keeps the row's bar. Negative controls: capped hard ([`CursorGlow::fire_kept_cap_hard`])
/// a step of over a fifth; capped hard and laid as one crest ([`CursorGlow::fire_kept_crest`]) a
/// step of half or more.
#[test]
fn under_a_kept_letter_the_flame_eases_in_over_pixels() {
    let g = tall_geom();
    let row = 4u16;
    let (top, ch, cw) = (i32::from(row) * g.ch as i32, g.ch as i32, g.cw as i32);
    // `(hardest step as a share of the band's brightest, worst contrast)`.
    let run = |gap_ms: u64, crest: bool, hard: bool| -> (f32, f32) {
        let (glass, text, caret, _) = kept_phrase(gap_ms, &|glow| {
            glow.fire_kept_crest = crest;
            glow.fire_kept_cap_hard = hard;
        });
        let band = top + ch * 8 / 10..top + ch;
        let (mut step, mut brightest, mut worst) = (0.0f32, 0.0f32, f32::INFINITY);
        for col in caret - 8..=caret {
            let x0 = i32::from(col) * cw;
            for y in band.clone() {
                for x in x0..x0 + cw {
                    brightest = brightest.max(dark_lift(glass.at(y, x)));
                }
            }
            let glyph_at = text[usize::from(col)];
            if col == caret || glyph_at == ' ' {
                continue;
            }
            let (i, cnt) = glyph(g, row, col);
            worst = worst.min(ratio(&glass, &i, &cnt));
            if crate::cursor_glow::emit::DESCENDER_GLYPHS.contains(glyph_at) {
                continue;
            }
            for x in x0..x0 + cw {
                let down: Vec<f32> = band.clone().map(|y| dark_lift(glass.at(y, x))).collect();
                for w in down.windows(2) {
                    step = step.max((w[1] - w[0]).abs());
                }
            }
        }
        (step / brightest.max(1.0), worst)
    };
    for gap_ms in [57u64, 90] {
        let (step, worst) = run(gap_ms, false, false);
        assert!(
            step <= 0.2,
            "{gap_ms} ms apart: under a letter the flame steps {step:.3} of its brightest in a pixel"
        );
        assert!(
            worst >= CursorGlow::FIRE_DARK_ROW_BAR,
            "{gap_ms} ms apart: a letter beside the flame reads at {worst:.2}:1"
        );
        let (hard, _) = run(gap_ms, false, true);
        let (round3, _) = run(gap_ms, true, true);
        assert!(
            hard > 0.2 && round3 >= 0.5,
            "control, {gap_ms} ms apart: capped in one pixel the flame steps {hard:.3}, \
             laid as round 3 laid it {round3:.3}"
        );
    }
}

/// A dark burst of `keys` letters of a sentence at 50 ms a key on row 4,
/// ticked to its last key.
fn dark_burst(keys: u16, seam: impl Fn(&mut CursorGlow)) -> (CursorGlow, Geom, u16) {
    dark_burst_on(
        tall_geom(),
        "the quick brown fox jumps over the lazy dog",
        keys,
        seam,
    )
}

/// The trail gallery's `burst` on dark: its sentence typed whole at its
/// pace, 50 ms a key, after the prompt on row 4 of its 14×26 glass.
fn gallery_dark_burst(seam: impl Fn(&mut CursorGlow)) -> (CursorGlow, Geom, u16) {
    let g = Geom {
        cw: 14,
        ch: 26,
        rows: 8,
        cols: 80,
        origin_x: 12,
        origin_y: 2,
        win_w: 1144,
        win_h: 212,
        head: 0,
    };
    let line = "echo the quick brown fox jumps over the lazy dog as the kitty flies";
    dark_burst_on(g, line, line.len() as u16, seam)
}

/// A dark burst of `keys` letters of `line` at 50 ms a key on row 4 of
/// `g`, from column 2, ticked to its last key.
fn dark_burst_on(
    g: Geom,
    line: &str,
    keys: u16,
    seam: impl Fn(&mut CursorGlow),
) -> (CursorGlow, Geom, u16) {
    let (row, start) = (4u16, 2u16);
    let line: Vec<char> = line.chars().collect();
    let mut c = cfg(GlowStyle::Fire, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let mut glow = CursorGlow::default();
    seam(&mut glow);
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    let mut t = Instant::now();
    for k in 0..=keys {
        if k > 0 {
            t += Duration::from_millis(50);
            text[usize::from(start + k - 1)] = line[usize::from(k - 1) % line.len()];
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
    (glow, g, start + keys)
}

#[test]
fn the_air_flames_taper_from_feathered_roots() {
    // Over the burst's newest eight letters: how many pixels the gold cores stand proud of their
    // flames' bodies (a core past its flame reads as a straight-sided column), and the share of
    // the lit columns whose root lies within a pixel of the run's lowest (a ruler-straight floor).
    let shape = |keys: u16, columns: bool| -> (i32, usize, usize) {
        let (glow, g, caret) = dark_burst(keys, |glow| glow.fire_column_tongues = columns);
        let bg = DARK_PAIR.1 as i32 & 0xff;
        let mut cores = Glass::new(g, DARK_PAIR.1);
        cores.add_quads(glow.under_quads());
        let mut field = Glass::new(g, DARK_PAIR.1);
        field.add_fire_patches(glow.patches());
        let mut both = Glass::new(g, DARK_PAIR.1);
        both.add_quads(glow.under_quads());
        both.add_fire_patches(glow.patches());
        let row_top = 4 * g.ch as i32;
        let newest = i32::from(caret) - 1;
        let top = |gl: &Glass, x: i32| (0..row_top).find(|&y| gl.level(y, x) > bg + 24);
        let (mut proud, mut roots) = (0, Vec::new());
        for x in (newest - 7) * g.cw as i32..(newest + 1) * g.cw as i32 {
            if let Some(c) = top(&cores, x) {
                proud += (top(&field, x).unwrap_or(row_top) - c).max(0);
            }
            if let Some(root) = (0..row_top).rev().find(|&y| both.level(y, x) > bg + 24) {
                roots.push(root);
            }
        }
        let lowest = roots.iter().copied().max().unwrap_or(0);
        let on_line = roots.iter().filter(|&&r| r >= lowest - 1).count();
        (proud, on_line, roots.len())
    };
    for keys in [20, 40] {
        let (proud, on_line, lit) = shape(keys, false);
        assert!(
            lit > 40,
            "fixture: {keys} keys light the air ({lit} columns)"
        );
        assert!(
            proud <= lit as i32,
            "{keys} keys: the cores stand {proud} px proud of their flames over {lit} columns"
        );
        assert!(
            2 * on_line < lit,
            "{keys} keys: {on_line} of {lit} roots stand on one line"
        );
        let (proud, on_line, lit) = shape(keys, true);
        assert!(
            proud > 3 * lit as i32 || 2 * on_line > lit,
            "control, {keys} keys: round 2's columns stood {proud} px proud, {on_line} of {lit} roots on one line"
        );
    }
}

#[test]
fn the_dark_caret_bloom_burns_ember() {
    // The caret's bloom (the halos a cell high or wider) alone over the
    // dark page: the light it adds keeps an ember's hue, never the tan a
    // dim gold makes of the page's blue-grey.
    let warmest = |unwarmed: bool| -> (f32, usize) {
        let (glow, g, _) = dark_burst(30, |glow| glow.fire_bloom_unwarmed = unwarmed);
        let bloom: Vec<RainHalo> = glow
            .halos()
            .iter()
            .filter(|h| usize::from(h.rx) >= g.ch)
            .copied()
            .collect();
        let mut glass = Glass::new(g, DARK_PAIR.1);
        glass.add_halos(&bloom);
        let ch = |p: u32, s: u32| ((p >> s) & 0xff) as i32;
        let (mut worst, mut lit) = (0.0f32, 0);
        for y in 0..glass.h as i32 {
            for x in 0..glass.w as i32 {
                let (p, b) = (glass.at(y, x), DARK_PAIR.1);
                let (r, gr, bl) = (
                    ch(p, 16) - ch(b, 16),
                    ch(p, 8) - ch(b, 8),
                    ch(p, 0) - ch(b, 0),
                );
                let (hi, lo) = (r.max(gr).max(bl), r.min(gr).min(bl));
                if hi < 6 || hi == lo {
                    continue;
                }
                lit += 1;
                let hue = if hi == r {
                    60.0 * (gr - bl) as f32 / (hi - lo) as f32
                } else {
                    90.0
                };
                worst = worst.max(hue);
            }
        }
        (worst, lit)
    };
    let (hue, lit) = warmest(false);
    assert!(lit > 200, "fixture: the bloom lights the page ({lit} px)");
    assert!(
        hue <= 30.0,
        "the caret's bloom adds light of hue {hue:.0} degrees, a tan"
    );
    let (hue, _) = warmest(true);
    assert!(
        hue > 30.0,
        "control: lit from the gold the bloom adds hue {hue:.0} degrees"
    );
}

/// The lone runs of visible paper fire (a pixel more than 24 levels off the
/// white page in some channel, eight-connected) in `band` rows of `f`, as
/// `(x, y, width, height)`, leaving out the caret's cell.
fn paper_fire_runs(
    f: &Glass,
    band: (i32, i32),
    caret: (i32, i32, i32, i32),
) -> Vec<(i32, i32, i32, i32)> {
    let seen_px = |y: i32, x: i32| {
        let inside_caret = (caret.0..caret.2).contains(&x) && (caret.1..caret.3).contains(&y);
        let p = f.at(y, x);
        !inside_caret
            && [16u32, 8, 0]
                .iter()
                .any(|&sh| 255 - ((p >> sh) & 0xff) as i32 > 24)
    };
    let (w, h) = (f.w as i32, f.h as i32);
    let mut seen = vec![false; f.w * f.h];
    let mut runs = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if seen[(y * w + x) as usize] || !seen_px(y, x) {
                continue;
            }
            let (mut x0, mut x1, mut y0, mut y1) = (x, x, y, y);
            let mut stack = vec![(y, x)];
            seen[(y * w + x) as usize] = true;
            while let Some((cy, cx)) = stack.pop() {
                (x0, x1, y0, y1) = (x0.min(cx), x1.max(cx), y0.min(cy), y1.max(cy));
                for (dy, dx) in [
                    (-1, -1),
                    (-1, 0),
                    (-1, 1),
                    (0, -1),
                    (0, 1),
                    (1, -1),
                    (1, 0),
                    (1, 1),
                ] {
                    let (ny, nx) = (cy + dy, cx + dx);
                    if (0..h).contains(&ny)
                        && (0..w).contains(&nx)
                        && !seen[(ny * w + nx) as usize]
                        && seen_px(ny, nx)
                    {
                        seen[(ny * w + nx) as usize] = true;
                        stack.push((ny, nx));
                    }
                }
            }
            if y0 >= band.0 && y1 < band.1 {
                runs.push((x0, y0, x1 - x0 + 1, y1 - y0 + 1));
            }
        }
    }
    runs
}

#[test]
fn paper_fire_leaves_no_lone_ember_dash_on_a_lettered_row() {
    // No visible run in the typed row is at least 6 px long and at most 3 px tall, typing and
    // cooling; the squat tongues (the negative control) leave such lone dashes.
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Fire, true);
    c.dark_theme = false;
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
    let (row, start) = (4u16, 2u16);
    let row_top = i32::from(g.origin_y) + i32::from(row) * g.ch as i32;
    let dashes = |squat: bool| {
        let mut glow = CursorGlow {
            fire_light_squat: squat,
            ..CursorGlow::default()
        };
        let (mut out, mut text) = (Vec::new(), vec![' '; g.cols]);
        let mut t = Instant::now();
        let mut found = Vec::new();
        let mut judge = |glow: &CursorGlow, caret: u16| {
            let mut f = Glass::new(g, 0x00FF_FFFF);
            f.add_quads(glow.under_quads());
            let (x0, y0, x1, y1) = f.cell_rect(row, caret);
            for run in paper_fire_runs(&f, (row_top, row_top + g.ch as i32), (x0, y0, x1, y1)) {
                if run.2 >= 6 && run.3 <= 3 {
                    found.push(run);
                }
            }
        };
        glow.observe_row(row, start, &text, t);
        glow.tick(Some((row, start)), t, &c, g, &mut out);
        let mut caret = start;
        for k in 1..=24u16 {
            t += Duration::from_millis(if k % 5 == 0 { 140 } else { 45 });
            text[usize::from(start + k - 1)] = if k % 6 == 0 { ' ' } else { 'n' };
            caret = start + k;
            glow.observe_row(row, caret, &text, t);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
            judge(&glow, caret);
        }
        for _ in 0..90 {
            t += Duration::from_millis(16);
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
            judge(&glow, caret);
        }
        found
    };
    let shipped = dashes(false);
    assert!(
        shipped.is_empty(),
        "lone ember dashes on paper (x, y, w, h): {:?}",
        &shipped[..shipped.len().min(6)]
    );
    assert!(
        !dashes(true).is_empty(),
        "control: the squat tongues leave a lone dash on the lettered row"
    );
}

#[test]
fn a_paper_tongue_s_root_feathers_over_pixels_not_a_cut() {
    // Up from each lit column's lowest pixel, the first four rows rise by no more than 0.45 of
    // the column's peak a row, typing and cooling; laid coarse (the negative control) a root
    // steps by more.
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Fire, true);
    c.dark_theme = false;
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
    let (row, start) = (4u16, 2u16);
    let row_top = i32::from(g.origin_y) + i32::from(row) * g.ch as i32;
    let worst = |coarse: bool| -> f32 {
        let mut glow = CursorGlow {
            fire_light_root_coarse: coarse,
            ..CursorGlow::default()
        };
        let (mut out, mut text) = (Vec::new(), vec![' '; g.cols]);
        let mut t = Instant::now();
        let mut worst = 0.0f32;
        let mut judge = |glow: &CursorGlow| {
            let mut f = Glass::new(g, 0x00FF_FFFF);
            f.add_quads(glow.under_quads());
            for x in 0..i32::from(g.win_w) {
                let ink = |y: i32| {
                    255 - {
                        let p = f.at(y, x);
                        ((p >> 16) & 0xff).min((p >> 8) & 0xff).min(p & 0xff) as i32
                    }
                };
                let band = row_top..row_top + g.ch as i32;
                let peak = band.clone().map(ink).max().unwrap_or(0);
                if peak < 60 {
                    continue;
                }
                let Some(foot) = band.clone().rev().find(|&y| ink(y) >= 4) else {
                    continue;
                };
                for y in (foot - 3..=foot).rev() {
                    let step = (ink(y - 1) - ink(y)) as f32 / peak as f32;
                    worst = worst.max(step);
                }
            }
        };
        glow.observe_row(row, start, &text, t);
        glow.tick(Some((row, start)), t, &c, g, &mut out);
        let mut caret = start;
        for k in 1..=24u16 {
            t += Duration::from_millis(if k % 5 == 0 { 140 } else { 45 });
            text[usize::from(start + k - 1)] = if k % 6 == 0 { ' ' } else { 'n' };
            caret = start + k;
            glow.observe_row(row, caret, &text, t);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
            judge(&glow);
        }
        for _ in 0..60 {
            t += Duration::from_millis(16);
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
            judge(&glow);
        }
        worst
    };
    let fine = worst(false);
    let coarse = worst(true);
    assert!(
        fine <= 0.45,
        "a paper tongue's root steps by {fine:.2} of its column's peak in a pixel row"
    );
    assert!(
        coarse >= 0.6,
        "control: laid coarse, a root steps by only {coarse:.2}"
    );
}

#[test]
fn a_dark_fire_keeps_its_forge_and_embers_off_a_filled_row_above() {
    // Under a prompt's powerline bar (a filled row, no letters over most of it) neither the
    // forge's crown nor the embers lay light on the bar; unheld (the negative control) both do.
    let g = tall_geom();
    let (row, ch, cw) = (4u16, g.ch as i32, g.cw as i32);
    let bar = 0x0030_3030u32;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let filled = |_: usize| aterm_core::terminal::RenderCell {
        ch: ' ',
        fg: rgb(DARK_PAIR.0),
        bg: rgb(bar),
        ..Default::default()
    };
    let lift = |unheld: bool| -> usize {
        let mut lit = 0;
        for (keys, gap) in [(8u16, 60u64), (16, 55), (28, 50)] {
            let (glow, out) = dark_fire_burst_with(keys, gap, Some(&filled), true, None, &|glow| {
                glow.fire_filled_unheld = unheld;
            });
            let mut glass = Glass::new(g, DARK_PAIR.1);
            let bar_rows = (i32::from(row) - 1) * ch..i32::from(row) * ch;
            for y in bar_rows.clone() {
                for x in 0..i32::from(g.win_w) {
                    glass.px[y as usize * glass.w + x as usize] = bar;
                }
            }
            glass.add_quads(glow.under_quads());
            glass.add_fire_patches(glow.patches());
            glass.add_quads(&out);
            glass.add_halos(glow.halos());
            let caret_x = (2 + i32::from(keys)) * cw;
            for y in bar_rows {
                for x in 0..i32::from(g.win_w) {
                    // The tongues kept in their row may feather a pixel or
                    // two over the bar's foot; the bar's body stays bare.
                    if y >= i32::from(row) * ch - 2 && (x - caret_x).abs() > 2 * cw {
                        continue;
                    }
                    let p = glass.at(y, x);
                    let up = [16u32, 8, 0]
                        .iter()
                        .map(|&sh| ((p >> sh) & 0xff) as i32 - ((bar >> sh) & 0xff) as i32)
                        .max()
                        .unwrap_or(0);
                    if up > 12 {
                        lit += 1;
                    }
                }
            }
        }
        lit
    };
    let held = lift(false);
    assert_eq!(
        held, 0,
        "{held} pixels of the filled bar above lit by the fire"
    );
    assert!(
        lift(true) > 20,
        "control: unheld, the forge and the embers leave the bar bare"
    );
}

/// looks7 M7: in the gallery's `context` a sparkle's moon stood on the
/// prompt's powerline bar above the typed row, past its segments' words.
/// Typed under a filled row, on dark and on paper, no sparkle light lifts
/// the bar; unheld (the negative control) the sparkles stand on it.
#[test]
fn a_sparkle_keeps_off_a_filled_row_above() {
    let g = tall_geom();
    let (row, ch) = (4u16, g.ch as i32);
    let lift = |pair: (u32, u32), bar: u32, unheld: bool| -> usize {
        let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        let filled = |_: usize| aterm_core::terminal::RenderCell {
            ch: ' ',
            fg: rgb(pair.0),
            bg: rgb(bar),
            ..Default::default()
        };
        let mut lit = 0;
        for (keys, gap) in [(8u16, 60u64), (16, 55), (28, 50)] {
            let (glow, out) = style_burst_on(
                GlowStyle::Sparkle,
                pair,
                (keys, gap),
                (Some(&filled), None),
                true,
                &|glow| {
                    glow.fire_filled_unheld = unheld;
                },
            );
            let mut glass = Glass::new(g, pair.1);
            let bar_rows = (i32::from(row) - 1) * ch..i32::from(row) * ch;
            for y in bar_rows.clone() {
                for x in 0..i32::from(g.win_w) {
                    glass.px[y as usize * glass.w + x as usize] = bar;
                }
            }
            glass.add_quads(glow.under_quads());
            glass.add_quads(&out);
            glass.add_halos(glow.halos());
            // The caret's own bloom feathers a few pixels into the bar's
            // foot; the bar's body stays bare.
            for y in bar_rows.start..bar_rows.end - 8 {
                for x in 0..i32::from(g.win_w) {
                    let p = glass.at(y, x);
                    let moved = [16u32, 8, 0]
                        .iter()
                        .map(|&sh| (((p >> sh) & 0xff) as i32 - ((bar >> sh) & 0xff) as i32).abs())
                        .max()
                        .unwrap_or(0);
                    if moved > 12 {
                        lit += 1;
                    }
                }
            }
        }
        lit
    };
    for (pair, bar) in [
        (DARK_PAIR, 0x0030_3030u32),
        ((0x001F_2328, 0x00FF_FFFF), 0x0033_3333),
    ] {
        let held = lift(pair, bar, false);
        assert_eq!(
            held, 0,
            "{held} pixels of the filled bar above lit by sparkles ({pair:06x?})"
        );
        assert!(
            lift(pair, bar, true) > 0,
            "control ({pair:06x?}): unheld, the sparkles leave the bar bare"
        );
    }
}

/// looks9 Q8: in the gallery's `context` on Solarized Dark Water's drops
/// stood on the prompt's powerline bar above the typed row as specks.
/// Typed under a filled row, on the reference dark, on Solarized Dark and
/// on paper, no drop lifts the bar outside the caret's own bloom. Negative
/// control: the drops laid as they were lift it. (Typing's Laser sparks fall
/// short of the bar: Laser's spark is
/// [`a_laser_spark_s_whole_dash_keeps_off_a_filled_row`]'s, its crackle
/// [`a_laser_crackle_keeps_off_a_filled_row_above`]'s.)
#[test]
fn a_water_drop_keeps_off_a_filled_row_above() {
    let g = tall_geom();
    let (row, ch, cw) = (4u16, g.ch as i32, g.cw as i32);
    let lift = |style: GlowStyle, pair: (u32, u32), bar: u32, on_bars: bool| -> usize {
        let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        let filled = |_: usize| aterm_core::terminal::RenderCell {
            ch: ' ',
            fg: rgb(pair.0),
            bg: rgb(bar),
            ..Default::default()
        };
        let mut lit = 0;
        for (keys, gap) in [(8u16, 60u64), (16, 55), (28, 50), (24, 40)] {
            let (glow, out) = style_burst_on(
                style,
                pair,
                (keys, gap),
                (Some(&filled), None),
                true,
                &|glow| {
                    glow.particles_on_bars = on_bars;
                },
            );
            let mut glass = Glass::new(g, pair.1);
            let bar_rows = (i32::from(row) - 1) * ch..i32::from(row) * ch;
            for y in bar_rows.clone() {
                for x in 0..i32::from(g.win_w) {
                    glass.px[y as usize * glass.w + x as usize] = bar;
                }
            }
            glass.add_quads(glow.under_quads());
            glass.add_quads(&out);
            glass.add_halos(glow.halos());
            // The caret's own bloom feathers a few pixels into the bar's
            // foot over the caret; the rest of the bar stays bare.
            let caret_x = (2 + i32::from(keys)) * cw;
            for y in bar_rows.clone() {
                for x in 0..i32::from(g.win_w) {
                    if y >= bar_rows.end - 8 && (x - caret_x).abs() <= 2 * cw {
                        continue;
                    }
                    let p = glass.at(y, x);
                    let moved = [16u32, 8, 0]
                        .iter()
                        .map(|&sh| (((p >> sh) & 0xff) as i32 - ((bar >> sh) & 0xff) as i32).abs())
                        .max()
                        .unwrap_or(0);
                    if moved > 12 {
                        lit += 1;
                    }
                }
            }
        }
        lit
    };
    for (pair, bar) in [
        (DARK_PAIR, 0x0030_3030u32),
        (SOLARIZED_PAIR, 0x0030_3030),
        ((0x001F_2328, 0x00FF_FFFF), 0x0033_3333),
    ] {
        let held = lift(GlowStyle::Water, pair, bar, false);
        assert_eq!(
            held, 0,
            "{held} px of the bar above lit by Water's drops ({pair:06x?})"
        );
        let laid = lift(GlowStyle::Water, pair, bar, true);
        assert!(
            laid > 0,
            "control ({pair:06x?}): laid as they were, the drops leave the bar bare"
        );
    }
}

/// looks9 round 3's review of Q8: A LASER SPARK'S WHOLE DASH KEEPS OFF A
/// FILLED ROW. A Laser spark is a dash from its head back along its live
/// velocity; standing on a prompt's powerline bar it was a lone violet dash
/// apart from its bolt (Solarized Dark's `context`). Typing's sparks rarely
/// reach the bar above, so these are laid by hand on the reference dark, a
/// bar on row 3 and the caret resting on row 4 well clear of them: one on
/// the bar, and one falling out of it, its head a pixel past the cull's
/// reach below the bar and its dash's trail inside it. Neither lights a
/// pixel of the bar. Negative controls: laid as they were
/// (`particles_on_bars`), the first lights the bar; read by its head alone
/// (`laser_spark_head_only`), the second's dash does.
#[test]
fn a_laser_spark_s_whole_dash_keeps_off_a_filled_row() {
    let g = tall_geom();
    let (pair, bar) = (DARK_PAIR, 0x0030_3030u32);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let (ch, cw) = (g.ch as f32, g.cw as f32);
    let (bar_row, row, col) = (3u16, 4u16, 34u16);
    let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for (r, line) in rows.iter_mut().enumerate() {
        for cell in line.iter_mut() {
            let ground = if r == usize::from(bar_row) {
                bar
            } else {
                pair.1
            };
            (cell.fg, cell.bg) = (rgb(pair.0), rgb(ground));
        }
    }
    let bar_bottom = f32::from(g.origin_y) + f32::from(bar_row + 1) * ch;
    // `(x, y, vx, vy)`: flying along the bar; and falling out of it at six
    // rows a second, its head a pixel past the cull's margin (0.14 of a row)
    // below the bar, its dash reaching 0.05 s back (0.3 of a row) into it.
    let standing = (10.5 * cw, bar_bottom - 0.5 * ch, 8.0 * ch, 0.0);
    let falling = (20.5 * cw, bar_bottom + 0.14 * ch + 1.0, 0.0, 6.0 * ch);
    // The bar's pixels the sparks change, laid at rest beside the caret.
    let lit = |sparks: &[(f32, f32, f32, f32)], setup: &dyn Fn(&mut CursorGlow)| -> usize {
        let frame = |with: bool| -> Glass {
            let mut c = cfg(GlowStyle::Laser, true);
            c.intensity = 1.0;
            c.dark_theme = true;
            (c.theme_fg, c.theme_bg) = pair;
            let mut glow = CursorGlow::default();
            setup(&mut glow);
            let mut out = Vec::new();
            let t0 = Instant::now();
            glow.observe_frame_text(Some((row, col)), &rows, t0);
            glow.tick(Some((row, col)), t0, &c, g, &mut out);
            let t1 = t0 + Duration::from_millis(16);
            if with {
                for &(x0, y0, vx, vy) in sparks {
                    glow.particles.push(Particle {
                        x0,
                        y0,
                        vx,
                        vy,
                        gy: 0.0,
                        life: 0.4,
                        hue: 0.3,
                        cov_scale: 1.0,
                        born: t1,
                        edge_cut: false,
                    });
                }
            }
            out.clear();
            glow.observe_frame_text(Some((row, col)), &rows, t1);
            glow.tick(Some((row, col)), t1, &c, g, &mut out);
            let mut glass = Glass::new(g, pair.1);
            glass.add_quads(glow.under_quads());
            glass.add_quads(&out);
            glass.add_halos(glow.halos());
            glass
        };
        let (on, off) = (frame(true), frame(false));
        let ys = i32::from(bar_row) * g.ch as i32..i32::from(bar_row + 1) * g.ch as i32;
        ys.flat_map(|y| (0..i32::from(g.win_w)).map(move |x| (y, x)))
            .filter(|&(y, x)| {
                let (a, b) = (on.at(y, x), off.at(y, x));
                [16u32, 8, 0]
                    .iter()
                    .any(|&sh| (((a >> sh) & 0xff) as i32 - ((b >> sh) & 0xff) as i32).abs() > 12)
            })
            .count()
    };
    let held = lit(&[standing, falling], &|_| {});
    assert_eq!(held, 0, "{held} px of the bar lit by Laser's sparks");
    let laid = lit(&[standing], &|glow| glow.particles_on_bars = true);
    assert!(
        laid > 0,
        "control: laid as it was, the spark on the bar leaves it bare"
    );
    let headed = lit(&[falling], &|glow| glow.laser_spark_head_only = true);
    assert!(
        headed > 0,
        "control: read by its head alone, the falling spark's dash leaves the bar bare"
    );
}

/// looks9 Q8: Laser's crackle, laid at a random angle from a typed cell,
/// rose onto Solarized Dark's powerline bar above the typed line as a
/// violet arc apart from the bolt (the gallery's `context`), and one
/// crossing a pane's edge is cut there to a lone fragment. Swept round every
/// angle at three lengths from a cell under a filled row, and from a cell
/// of the grid's top row, no kept crackle's corona meets the bar or leaves
/// the grid, and a third or more of them are kept (a crackle that meets
/// one is dropped: [`a_laser_crackle_that_meets_an_edge_is_dropped`]).
/// Negative control: laid as they were, the crackles aimed up meet it.
#[test]
fn a_laser_crackle_keeps_off_a_filled_row_above() {
    let g = tall_geom();
    let (pair, bar) = (SOLARIZED_PAIR, 0x0030_3030u32);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let ch = g.ch as f32;
    let half = ch * 0.15;
    // `(row, barred)`: under a filled row, or on the grid's top row.
    let sweep = |(row, barred): (u16, bool), kept: bool| -> (usize, usize, usize) {
        let col = 20u16;
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for (r, line) in rows.iter_mut().enumerate() {
            for cell in line.iter_mut() {
                let ground = if barred && r + 1 == usize::from(row) {
                    bar
                } else {
                    pair.1
                };
                (cell.fg, cell.bg) = (rgb(pair.0), rgb(ground));
            }
        }
        for cell in &mut rows[usize::from(row)][2..usize::from(col)] {
            cell.ch = 'm';
        }
        // The bar's foot, or the grid's top edge: the root row's top.
        let ceiling = f32::from(g.origin_y) + f32::from(row) * ch;
        let (x0, y0) = g.cell_center(row, col - 3);
        let mut glow = CursorGlow::default();
        glow.observe_frame_text(Some((row, col)), &rows, Instant::now());
        glow.particles_on_bars = !kept;
        let (mut laid, mut touched, mut dropped) = (0, 0, 0);
        for i in 0..72 {
            let (sin_a, cos_a) = (i as f32 * 5f32.to_radians()).sin_cos();
            for len in [1.6f32, 3.0, 4.4] {
                glow.bolts.clear();
                glow.spawn_bolt(
                    (x0, y0),
                    (x0 + cos_a * len * ch, y0 + sin_a * len * ch),
                    ch,
                    false,
                    Instant::now(),
                );
                if kept {
                    glow.keep_crackle_off_bars(g, pair.1, row, y0);
                }
                let Some(b) = glow.bolts.last() else {
                    dropped += 1;
                    continue;
                };
                laid += 1;
                let top = b
                    .pts
                    .as_slice()
                    .windows(2)
                    .flat_map(|w| (0..=8).map(move |k| w[0].1 + (w[1].1 - w[0].1) * k as f32 / 8.0))
                    .fold(f32::INFINITY, f32::min);
                touched += usize::from(top - half < ceiling);
            }
        }
        (laid, touched, dropped)
    };
    for fixture in [(4u16, true), (0, false)] {
        let (laid, touched, dropped) = sweep(fixture, true);
        assert!(
            laid * 3 >= laid + dropped,
            "{fixture:?}: only {laid} crackles kept of {}",
            laid + dropped
        );
        assert_eq!(
            touched, 0,
            "{fixture:?}: {touched} of {laid} crackles meet the bar or the edge"
        );
        let (old, old_touched, _) = sweep(fixture, false);
        assert!(
            old_touched * 4 >= old,
            "control {fixture:?}: laid as they were, only {old_touched} of {old} crackles meet it"
        );
    }
}

/// looks9 round 3's Q8: A LASER CRACKLE THAT MEETS AN EDGE IS DROPPED, NOT
/// TURNED. Turned about its root's row to aim away from the pane's bottom
/// edge (round 2), the crackle a typed cell on a pane's next-to-last row
/// threw down rose two rows into the air over the typed line, its root
/// hidden among the letters: a lone violet zigzag beside the cat's word
/// icon, apart from the caret's bolt (the gallery's `split` at `b0`). Swept
/// round every angle at three lengths from a cell of the pane's
/// next-to-last row, every crackle kept is the one laid, and none aimed down
/// reaches above its row. Negative control: turned (`crackle_turned`), a
/// tenth or more of the crackles aimed down rise over the row's top by half
/// a row and more.
#[test]
fn a_laser_crackle_that_meets_an_edge_is_dropped() {
    let g = tall_geom();
    let pair = SOLARIZED_PAIR;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let ch = g.ch as f32;
    let row = (g.rows - 2) as u16;
    let col = 20u16;
    let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for line in &mut rows {
        for cell in line.iter_mut() {
            (cell.fg, cell.bg) = (rgb(pair.0), rgb(pair.1));
        }
    }
    for cell in &mut rows[usize::from(row)][2..usize::from(col)] {
        cell.ch = 'm';
    }
    let row_top = f32::from(g.origin_y) + f32::from(row) * ch;
    let (x0, y0) = g.cell_center(row, col - 3);
    // `(down, risen, changed)`: the crackles laid aiming down, those kept
    // that rise half a row over the row's top, and those kept not as laid.
    let sweep = |turned: bool| {
        let mut glow = CursorGlow::default();
        glow.observe_frame_text(Some((row, col)), &rows, Instant::now());
        glow.crackle_turned = turned;
        let (mut down, mut risen, mut changed) = (0usize, 0usize, 0usize);
        for i in 0..72 {
            let (sin_a, cos_a) = (i as f32 * 5f32.to_radians()).sin_cos();
            for len in [1.6f32, 3.0, 4.4] {
                glow.bolts.clear();
                glow.spawn_bolt(
                    (x0, y0),
                    (x0 + cos_a * len * ch, y0 + sin_a * len * ch),
                    ch,
                    false,
                    Instant::now(),
                );
                let laid = glow.bolts.last().map(|b| b.pts);
                glow.keep_crackle_off_bars(g, pair.1, row, y0);
                down += usize::from(sin_a > 0.2);
                let Some(b) = glow.bolts.last() else {
                    continue;
                };
                changed += usize::from(laid.is_some_and(|l| l.as_slice() != b.pts.as_slice()));
                let top = b
                    .pts
                    .as_slice()
                    .iter()
                    .map(|p| p.1)
                    .fold(f32::INFINITY, f32::min);
                risen += usize::from(sin_a > 0.2 && top < row_top - 0.5 * ch);
            }
        }
        (down, risen, changed)
    };
    let (down, risen, changed) = sweep(false);
    assert!(down >= 60, "fixture: {down} crackles aimed down");
    assert_eq!(changed, 0, "{changed} kept crackles are not as laid");
    assert_eq!(
        risen, 0,
        "{risen} of {down} crackles aimed down rise over the typed row"
    );
    let (_, old_risen, _) = sweep(true);
    assert!(
        old_risen * 10 >= down,
        "control: turned, only {old_risen} of {down} crackles aimed down rise over the row"
    );
}

#[test]
fn a_paper_fire_keeps_its_tongues_and_embers_off_a_filled_row_above() {
    // Under a prompt's dark powerline bar the bar stops paper fire's tongues and embers as a
    // letter does: its body stays bare; unheld (the negative control) the fire paints it.
    let g = tall_geom();
    let (row, ch, cw) = (4u16, g.ch as i32, g.cw as i32);
    let (pair, bar) = ((0x001F_2328u32, 0x00FF_FFFFu32), 0x0033_3333u32);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let filled = |_: usize| aterm_core::terminal::RenderCell {
        ch: ' ',
        fg: rgb(0x00FF_FFFF),
        bg: rgb(bar),
        ..Default::default()
    };
    let painted = |unheld: bool| -> usize {
        let mut lit = 0;
        for (keys, gap) in [(8u16, 60u64), (16, 55), (28, 50)] {
            let (glow, out) =
                fire_burst_on(pair, (keys, gap), (Some(&filled), None), true, &|glow| {
                    glow.fire_filled_unheld = unheld;
                });
            let mut glass = Glass::new(g, pair.1);
            let bar_rows = (i32::from(row) - 1) * ch..i32::from(row) * ch;
            for y in bar_rows.clone() {
                for x in 0..i32::from(g.win_w) {
                    glass.px[y as usize * glass.w + x as usize] = bar;
                }
            }
            glass.add_quads(glow.under_quads());
            glass.add_fire_patches(glow.patches());
            glass.add_quads(&out);
            glass.add_halos(glow.halos());
            let caret_x = (2 + i32::from(keys)) * cw;
            for y in bar_rows {
                for x in 0..i32::from(g.win_w) {
                    // A tongue kept in its row may feather a pixel or two
                    // over the bar's foot; the bar's body stays bare.
                    if y >= i32::from(row) * ch - 2 && (x - caret_x).abs() > 2 * cw {
                        continue;
                    }
                    let p = glass.at(y, x);
                    let off = [16u32, 8, 0]
                        .iter()
                        .map(|&sh| (((p >> sh) & 0xff) as i32 - ((bar >> sh) & 0xff) as i32).abs())
                        .max()
                        .unwrap_or(0);
                    if off > 12 {
                        lit += 1;
                    }
                }
            }
        }
        lit
    };
    let held = painted(false);
    assert_eq!(
        held, 0,
        "{held} pixels of the filled bar above painted by paper fire"
    );
    assert!(
        painted(true) > 20,
        "control: unheld, the paper fire leaves the bar bare"
    );
}

/// The air cores' roots over a dark burst's newest thirty letters, in x
/// order: `(x, core_root, flame_root)` per core, its root where its light
/// shows over the dark (the lowest lit pixel at its run's middle) and its
/// flame's (the fire field's alone) at the same x.
fn air_core_roots(glow: &CursorGlow, g: Geom, caret: u16) -> Vec<(i32, i32, i32)> {
    let mut cores = Glass::new(g, DARK_PAIR.1);
    cores.add_quads(glow.under_quads());
    let mut field = Glass::new(g, DARK_PAIR.1);
    field.add_fire_patches(glow.patches());
    let bg = DARK_PAIR.1 as i32 & 0xff;
    let (ox, oy) = (i32::from(g.origin_x), i32::from(g.origin_y));
    let row_top = oy + 4 * g.ch as i32;
    let newest = i32::from(caret) - 1;
    // The gold heart above a root is no root, and a dim core's root reads
    // as one.
    let lit = |gl: &Glass, x: i32, y: i32| ((gl.at(y, x) >> 16) & 0xff) as i32 > bg + 40;
    let root = |gl: &Glass, x: i32| (0..row_top).rev().find(|&y| lit(gl, x, y));
    let mut roots = Vec::new();
    let mut run: Option<(i32, i32, i32)> = None;
    for x in ox + (newest - 30).max(0) * g.cw as i32..=ox + (newest + 2) * g.cw as i32 {
        match (root(&cores, x), run) {
            (Some(r), None) => run = Some((x, x, r)),
            (Some(r), Some((x0, _, low))) => run = Some((x0, x, low.max(r))),
            (None, Some((x0, x1, low))) => {
                if x1 - x0 + 1 >= 2 {
                    let xc = (x0 + x1) / 2;
                    roots.push((xc, low, root(&field, xc).unwrap_or(row_top)));
                }
                run = None;
            }
            (None, None) => {}
        }
    }
    roots
}

type Burst = fn(&dyn Fn(&mut CursorGlow)) -> (CursorGlow, Geom, u16);

/// The dark bursts the air cores are judged on: forty letters of a
/// sentence, and the trail gallery's `burst` line.
const AIR_BURSTS: [(&str, Burst); 2] = [
    ("40 keys", |seam| dark_burst(40, seam)),
    ("the gallery's burst", |seam| gallery_dark_burst(seam)),
];

#[test]
fn the_dark_air_cores_stand_apart_not_as_a_comb() {
    // No three neighbouring roots lie within 2 px of each other; laid even (the negative
    // control), three neighbours share a floor.
    let comb = |burst: Burst, even: bool| -> (usize, Vec<i32>) {
        let (glow, g, caret) = burst(&|glow: &mut CursorGlow| glow.fire_even_cores = even);
        let roots: Vec<i32> = air_core_roots(&glow, g, caret)
            .iter()
            .map(|r| r.1)
            .collect();
        let combs = roots
            .windows(3)
            .filter(|w| {
                let (lo, hi) = (w.iter().min().unwrap_or(&0), w.iter().max().unwrap_or(&0));
                hi - lo <= 2
            })
            .count();
        (combs, roots)
    };
    for (name, burst) in AIR_BURSTS {
        let (combs, roots) = comb(burst, false);
        assert!(
            roots.len() >= 4,
            "fixture ({name}): the burst lays its cores ({roots:?})"
        );
        assert_eq!(
            combs, 0,
            "{name}: three neighbouring roots share a floor: {roots:?}"
        );
        let (even_combs, even_roots) = comb(burst, true);
        assert!(
            even_combs > 0,
            "control ({name}): laid even, the roots {even_roots:?} stand apart"
        );
    }
}

#[test]
fn a_dark_air_core_stands_on_its_own_flame_s_root() {
    // The whole tongue rises with its core: no core's root shows more than 4 px above its
    // flame's at its x. Lifted alone (the negative control), a core stands 6 px or more above.
    let floating = |burst: Burst, alone: bool| -> Vec<i32> {
        let (glow, g, caret) = burst(&|glow: &mut CursorGlow| glow.fire_core_lift_alone = alone);
        air_core_roots(&glow, g, caret)
            .iter()
            .map(|&(_, core, flame)| flame - core)
            .collect()
    };
    for (name, burst) in AIR_BURSTS {
        let above = floating(burst, false);
        assert!(
            above.len() >= 4,
            "fixture ({name}): the burst lays its cores"
        );
        assert!(
            above.iter().all(|&d| d <= 4),
            "{name}: a core's root stands above its flame's by {above:?} px"
        );
        let alone = floating(burst, true);
        assert!(
            alone.iter().any(|&d| d >= 6),
            "control ({name}): lifted alone, the cores stand only {alone:?} px above their flames"
        );
    }
}

/// The share of the lit pixels over the dark burst's row that read tan
/// (warm, greyed and mid-bright: hue 25-55, saturation 0.30-0.72, value
/// 0.5-0.86, as `#c1964c` to `#ca9a42` do), over the gallery `burst`'s last
/// 28 keys, with additive fire held to palette index `cap`.
fn burst_tan_share(cap: i32) -> f32 {
    let g = Geom {
        cw: 14,
        ch: 26,
        rows: 8,
        cols: 80,
        origin_x: 12,
        origin_y: 2,
        win_w: 1144,
        win_h: 212,
        head: 0,
    };
    let line: Vec<char> = "echo the quick brown fox jumps over the lazy dog as the kitty flies"
        .chars()
        .collect();
    let (row, start) = (4u16, 2u16);
    let mut c = cfg(GlowStyle::Fire, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    let mut t = Instant::now();
    let (mut lit, mut tan) = (0usize, 0usize);
    let row_top = i32::from(g.origin_y) + i32::from(row) * g.ch as i32;
    for k in 0..=line.len() as u16 {
        if k > 0 {
            t += Duration::from_millis(50);
            text[usize::from(start + k - 1)] = line[usize::from(k - 1)];
            glow.note_synthetic_typed(t, 1);
        }
        glow.observe_row(row, start + k, &text, t);
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for (col, &ch) in text.iter().enumerate() {
            rows[usize::from(row)][col].ch = ch;
        }
        glow.observe_frame_text(Some((row, start + k)), &rows, t);
        glow.tick(Some((row, start + k)), t, &c, g, &mut out);
        if usize::from(k) + 28 <= line.len() {
            continue;
        }
        let mut f = Glass::new(g, DARK_PAIR.1);
        f.fire_cap = cap;
        f.add_quads(glow.under_quads());
        f.add_fire_patches(glow.patches());
        f.add_quads(&out);
        f.add_halos(glow.halos());
        for y in 0..row_top {
            for x in 0..g.win_w as i32 {
                let (h, s, v) = crate::color_math::rgb2hsv(f.at(y, x));
                if v > 0.3 {
                    lit += 1;
                }
                if (25.0..=55.0).contains(&h) && (0.3..=0.72).contains(&s) && v > 0.5 && v < 0.86 {
                    tan += 1;
                }
            }
        }
    }
    assert!(lit > 20_000, "fixture: the burst burns ({lit} lit px)");
    tan as f32 / lit as f32
}

#[test]
fn the_dark_burst_burns_amber_tongues_not_a_tan_curtain() {
    // Additive fire stops a breath past amber, so at most 2.5% of the burst's light reads tan;
    // up to the field's own ceiling (the negative control), 3.5% or more does.
    let held = burst_tan_share(aterm_render::fire_field::FIRE_ADD_IDX_MAX);
    assert!(
        held <= 0.025,
        "{:.1}% of the burst's light reads tan",
        held * 100.0
    );
    let pale = burst_tan_share(1023);
    assert!(
        pale >= 0.035,
        "control: up to the field's ceiling only {:.1}% reads tan",
        pale * 100.0
    );
}

/// The light (summed channels times area) the additive quads of a dark
/// `rainbow kitty pet` frame lay in the row over the caret, across the
/// caret's cell and one either side, with a powerline segment (`main +2` on
/// `#cdcd00`) filling that row over the caret, 40 and 120 ms after its last
/// key.
fn ribbon_light_on_the_bar_above(unheld: bool) -> u64 {
    let g = tall_geom();
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let (row, caret_col) = (4u16, 21u16);
    let mut host = FireHost::new(true, false);
    let mut c = cfg_for_style_name("rainbow kitty pet", true);
    c.dark_theme = true;
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    host.cfg = c;
    host.fx.rainbow_rim_unheld = unheld;
    let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for line in &mut rows {
        for cell in line.iter_mut() {
            (cell.fg, cell.bg) = (rgb(DARK_PAIR.0), rgb(DARK_PAIR.1));
        }
    }
    for (i, ch) in " main +2 ".chars().enumerate() {
        let cell = &mut rows[usize::from(row - 1)][13 + i];
        (cell.ch, cell.fg, cell.bg) = (ch, [0, 0, 0], rgb(0x00CD_CD00));
    }
    host.rows = Some(rows);
    let mut t = Instant::now();
    host.frame(t, (row, 2), false);
    for k in 1..=(caret_col - 2) {
        t += Duration::from_millis(85);
        host.frame(t, (row, 2 + k), true);
    }
    let top = i32::from(row) * g.ch as i32;
    let (x0, x1) = (
        i32::from(caret_col - 1) * g.cw as i32,
        i32::from(caret_col + 2) * g.cw as i32,
    );
    let mut light = 0u64;
    for after in [40u64, 120] {
        host.frame(t + Duration::from_millis(after), (row, caret_col), false);
        for q in host.fx.glow_scratch.iter().filter(|q| q.alpha == 0) {
            let (qy0, qy1) = (i32::from(q.y), (i32::from(q.y) + i32::from(q.h)).min(top));
            let (qx0, qx1) = (
                i32::from(q.x).max(x0),
                (i32::from(q.x) + i32::from(q.w)).min(x1),
            );
            if qy1 <= qy0 || qx1 <= qx0 {
                continue;
            }
            let sum = |c: u32| u64::from(((c >> 16) & 0xff) + ((c >> 8) & 0xff) + (c & 0xff));
            light += (sum(q.color) + sum(q.color2)) / 2 * (qy1 - qy0) as u64 * (qx1 - qx0) as u64;
        }
    }
    light
}

#[test]
fn the_rainbow_block_s_halo_keeps_off_the_prompt_bar_above() {
    // Held off an inked or filled row next to the caret's as the momentum glow is, the rainbow
    // block's halo rings lay a twelfth or less on the powerline segment over the caret of what
    // they lay whole (the negative control).
    let held = ribbon_light_on_the_bar_above(false);
    let whole = ribbon_light_on_the_bar_above(true);
    assert!(
        whole > 2_000,
        "fixture: whole, the rings light the bar ({whole})"
    );
    assert!(
        held * 12 <= whole,
        "the rings lay {held} on the bar over the caret, against {whole} whole"
    );
}

/// The dim warm px (a red, an orange or a yellow at or over 0.3 saturation
/// and under 0.6 value: maroon, brown, olive) a dark `rainbow kitty pet`
/// Return at the foot of the window lays in the letter guard's reach under
/// the line it printed (that row's last 6 px to its ring and feather under
/// it), composited with the printed letters, ticked every 20 ms from the
/// Return and counted from 40 to 200 ms after it.
fn dark_return_mud_under_the_printed_line(in_guard: bool) -> usize {
    use aterm_core::terminal::RenderCell;
    let g = tall_geom();
    let bottom = g.rows as u16 - 1;
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
    let mut host = FireHost::new(true, false);
    let mut c = cfg_for_style_name("rainbow kitty pet", true);
    c.dark_theme = true;
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = DARK_PAIR;
    host.cfg = c;
    host.fx.glow.v2.lay_trains_in_guard(in_guard);
    host.text[0] = '$';
    host.rows = Some(frame(&[]));
    let mut t = Instant::now();
    host.frame(t, (bottom, 2), false);
    for k in 1..=22u16 {
        t += Duration::from_millis(60);
        host.frame(t, (bottom, 2 + k), true);
    }
    let typed = host.text.clone();
    t += Duration::from_millis(30);
    let mut printed = vec![' '; g.cols];
    printed[..19].fill('m');
    let mut prompt = vec![' '; g.cols];
    prompt[0] = '$';
    host.fx.glow.note_scroll(2);
    let rows = frame(&[
        (bottom - 2, &typed),
        (bottom - 1, &printed),
        (bottom, &prompt),
    ]);
    host.rows = Some(rows.clone());
    host.text = prompt;
    host.fx.glow.note_synthetic_move(t);
    let ch = g.ch as i32;
    let reach = LETTER_GUARD_RING + (g.ch as f32 * InkLedger::FEATHER_Y).ceil() as i32;
    let (y0, y1) = (i32::from(bottom) * ch - 6, i32::from(bottom) * ch + reach);
    let mut mud = 0;
    for after in (0..=200u64).step_by(20) {
        host.frame(t + Duration::from_millis(after), (bottom, 2), false);
        if after < 40 {
            continue;
        }
        let mut f = Glass::new(g, DARK_PAIR.1);
        f.add_quads(host.fx.glow.under_quads());
        for (r, line) in rows.iter().enumerate() {
            for (col, cell) in line.iter().enumerate() {
                if cell.ch != ' ' && cell.ch != '\0' {
                    for (y, x) in glyph(g, r as u16, col as u16).0 {
                        f.px[y as usize * f.w + x as usize] = DARK_PAIR.0;
                    }
                }
            }
        }
        f.add_quads(&host.fx.glow_scratch);
        f.add_halos(host.fx.glow.halos());
        for y in y0..y1 {
            for x in 0..g.win_w as i32 {
                let (h, s, v) = crate::color_math::rgb2hsv(f.at(y, x));
                if s >= 0.3 && (0.16..0.6).contains(&v) && (h < 70.0 || h >= 330.0) {
                    mud += 1;
                }
            }
        }
    }
    mud
}

#[test]
fn a_dark_return_s_train_keeps_clear_of_the_letter_guard_s_reach() {
    // A warm light the letter guard cuts to a share of itself reads maroon, brown and olive on a
    // dark page. Kept clear of the guard's reach and tapering to a point at its edge, the Return's
    // train lays a twentieth or less of the mud it lays inside it (the negative control).
    let clear = dark_return_mud_under_the_printed_line(false);
    let inside = dark_return_mud_under_the_printed_line(true);
    assert!(
        inside >= 500,
        "fixture: inside the guard's reach the train reads brown ({inside})"
    );
    assert!(
        clear * 20 <= inside,
        "the train lays {clear} dim warm px in the guard's reach, against {inside} inside it"
    );
}

/// Solarized Dark's text and teal page: a tinted dark page ([`tinted_dark_page`]).
const SOLARIZED_PAIR: (u32, u32) = (0x0083_9496, 0x0000_2B36);

/// The pixels of a fire frame on `pair`'s page that the fire lights (brighter
/// than the same glyphs on the bare page) and that read green: hue 70..180°
/// at a saturation and value the eye names a hue at. Fire's white-hot core,
/// clipped in red and then green, reads a lemon yellow up to 70° on every
/// dark page, the reference page included (52 of 14,912 lit pixels of
/// [`fire_on_solarized`]'s run there read 50..70°, none past it).
fn green_fire_pixels(
    glow: &CursorGlow,
    out: &[GlowQuad],
    g: Geom,
    pair: (u32, u32),
    ink: &[(u16, u16)],
) -> (usize, usize) {
    green_pixels_from(glow, out, g, pair, ink, 0)
}

/// [`green_fire_pixels`] judged only from pixel column `x_from` on.
fn green_pixels_from(
    glow: &CursorGlow,
    out: &[GlowQuad],
    g: Geom,
    pair: (u32, u32),
    ink: &[(u16, u16)],
    x_from: usize,
) -> (usize, usize) {
    let mut lit = Glass::new(g, pair.1);
    lit.add_quads(glow.under_quads());
    lit.add_fire_patches(glow.patches());
    let mut bare = Glass::new(g, pair.1);
    for &(row, col) in ink {
        for (y, x) in glyph(g, row, col).0 {
            lit.px[y as usize * lit.w + x as usize] = pair.0;
            bare.px[y as usize * bare.w + x as usize] = pair.0;
        }
    }
    lit.add_quads(out);
    lit.add_halos(glow.halos());
    let (mut fire, mut green) = (0usize, 0usize);
    for (i, (&p, &q)) in lit.px.iter().zip(&bare.px).enumerate() {
        if i % lit.w < x_from {
            continue;
        }
        let (p, q) = (p & 0x00FF_FFFF, q & 0x00FF_FFFF);
        let ch = |c: u32, sh: u32| ((c >> sh) & 0xff) as i32;
        let moved = [16, 8, 0]
            .iter()
            .any(|&sh| (ch(p, sh) - ch(q, sh)).abs() > 12);
        if !moved || rel_luminance(p) <= rel_luminance(q) {
            continue;
        }
        fire += 1;
        let (h, s, v) = crate::color_math::rgb2hsv(p);
        if s >= 0.25 && v >= 0.15 && (70.0..180.0).contains(&h) {
            green += 1;
        }
    }
    (fire, green)
}

fn fire_on_solarized(grounded: bool) -> (usize, usize) {
    let g = tall_geom();
    let (row, start) = (3u16, 2u16);
    let mut host = FireHost::styled(GlowStyle::Fire, SOLARIZED_PAIR, true, false);
    host.fx.glow.night_ground_off = !grounded;
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    let (mut fire, mut green) = (0usize, 0usize);
    let mut judge = |host: &FireHost, caret: u16| {
        let ink: Vec<(u16, u16)> = (start..caret).map(|col| (row, col)).collect();
        let (f, gr) = green_fire_pixels(
            &host.fx.glow,
            &host.fx.glow_scratch,
            g,
            SOLARIZED_PAIR,
            &ink,
        );
        fire += f;
        green += gr;
    };
    let keys = 30u16;
    for k in 1..=keys {
        t += Duration::from_millis(57);
        host.frame(t, (row, start + k), true);
        if k % 5 == 0 {
            judge(&host, start + k);
        }
    }
    for dt in [33u64, 120, 300] {
        host.frame(t + Duration::from_millis(dt), (row, start + keys), false);
        judge(&host, start + keys);
    }
    (fire, green)
}

/// FIRE IS NEVER GREEN, A TINTED DIM PAGE INCLUDED (looks8 K5): typed on
/// Solarized Dark's teal, no pixel the fire lights reads green; its flames
/// and its caret bloom burn over a night ground ([`push_night_ground`]).
/// Negative control: laid straight on the teal, its feathered foot, licks
/// and bloom read green.
#[test]
fn fire_on_a_tinted_page_never_reads_green() {
    let (fire, green) = fire_on_solarized(true);
    assert!(fire > 500, "fixture: the fire lights the page ({fire} px)");
    assert_eq!(
        green, 0,
        "{green} of {fire} fire pixels read green on Solarized Dark"
    );
    let (bare_fire, bare_green) = fire_on_solarized(false);
    assert!(
        bare_green * 50 > bare_fire,
        "control: on the bare teal only {bare_green} of {bare_fire} fire pixels read green"
    );
}

fn sparkle_bloom_on_solarized(grounded: bool) -> (usize, usize) {
    let g = tall_geom();
    let (row, start) = (3u16, 2u16);
    let mut host = FireHost::styled(GlowStyle::Sparkle, SOLARIZED_PAIR, true, false);
    host.cfg.color = crate::cursor_glow::style::SPARKLE_DEFAULT_COLOR;
    host.fx.glow.night_ground_off = !grounded;
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    let (mut lit, mut green) = (0usize, 0usize);
    let keys = 12u16;
    for k in 1..=keys {
        t += Duration::from_millis(80);
        host.frame(t, (row, start + k), true);
        let caret = start + k;
        let ink: Vec<(u16, u16)> = (start..caret).map(|col| (row, col)).collect();
        // Ahead of the caret only the bloom lies.
        let x_from = usize::from(g.origin_x) + usize::from(caret + 1) * g.cw;
        let (l, gr) = green_pixels_from(
            &host.fx.glow,
            &host.fx.glow_scratch,
            g,
            SOLARIZED_PAIR,
            &ink,
            x_from,
        );
        lit += l;
        green += gr;
    }
    (lit, green)
}

/// SPARKLE'S CARET BLOOM READS GOLD ON A TINTED PAGE (looks8 K6): on
/// Solarized Dark its gold glows over a night ground
/// ([`push_night_ground`]), and no pixel of it reads sage. Negative
/// control: added straight to the teal, the bloom reads sage.
#[test]
fn sparkles_bloom_reads_gold_on_a_tinted_page() {
    let (lit, green) = sparkle_bloom_on_solarized(true);
    assert!(
        lit > 100,
        "fixture: the bloom lights the page ahead of the caret ({lit} px)"
    );
    assert_eq!(
        green, 0,
        "{green} of {lit} bloom pixels read sage on Solarized Dark"
    );
    let (bare_lit, bare_green) = sparkle_bloom_on_solarized(false);
    assert!(
        bare_green * 4 > bare_lit,
        "control: on the bare teal only {bare_green} of {bare_lit} bloom pixels read sage"
    );
}

fn sparkle_tail_on_solarized(matched: bool) -> (usize, usize) {
    let g = tall_geom();
    let (row, start) = (3u16, 2u16);
    let mut host = FireHost::styled(GlowStyle::Sparkle, SOLARIZED_PAIR, true, false);
    host.cfg.color = crate::cursor_glow::style::SPARKLE_DEFAULT_COLOR;
    host.fx.glow.rail_unmatched = !matched;
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    let (mut lit, mut green) = (0usize, 0usize);
    let mut judge = |host: &FireHost, caret: u16| {
        let ink: Vec<(u16, u16)> = (start..caret).map(|col| (row, col)).collect();
        let (l, gr) = green_fire_pixels(
            &host.fx.glow,
            &host.fx.glow_scratch,
            g,
            SOLARIZED_PAIR,
            &ink,
        );
        lit += l;
        green += gr;
    };
    let keys = 24u16;
    for k in 1..=keys {
        t += Duration::from_millis(70);
        host.frame(t, (row, start + k), true);
        if k % 4 == 0 {
            judge(&host, start + k);
        }
    }
    for dt in [60u64, 160, 320] {
        host.frame(t + Duration::from_millis(dt), (row, start + keys), false);
        judge(&host, start + keys);
    }
    (lit, green)
}

/// SPARKLE'S TAIL FADES IN ITS GOLD ON A TINTED PAGE (looks10 L11): on Solarized Dark the
/// rail under the typed line is laid page-matched ([`crate::cursor_glow::page_matched`]), so as
/// its opacity fades along the tail it reads as it fades on the reference dark page, dark
/// amber, and no pixel of the frame reads sage. Negative control: laid straight over the teal
/// (every other light still on its night ground), the rail's amber faded by its opacity let the
/// page's green and blue through (`#2c3c2d`, hue 124, under the trail gallery's typed line).
#[test]
fn sparkles_tail_fades_in_gold_on_a_tinted_page() {
    let (lit, green) = sparkle_tail_on_solarized(true);
    assert!(lit > 500, "fixture: the tail lights the page ({lit} px)");
    assert_eq!(
        green, 0,
        "{green} of {lit} tail pixels read sage on Solarized Dark"
    );
    let (bare_lit, bare_green) = sparkle_tail_on_solarized(false);
    assert!(
        bare_green > 100,
        "control: on the bare teal only {bare_green} of {bare_lit} tail pixels read sage"
    );
}

/// The pixels Water lifts by a 24th or more of full scale over Solarized
/// Dark's page behind the caret, the gallery's measure, as it types a line:
/// the frame just after its last key.
fn water_on_solarized(lit_bed: bool) -> usize {
    let g = tall_geom();
    let (row, start) = (3u16, 2u16);
    let mut host = FireHost::styled(GlowStyle::Water, SOLARIZED_PAIR, true, false);
    host.fx.glow.water_lit_off = !lit_bed;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for cell in page.iter_mut().flatten() {
        (cell.fg, cell.bg) = (rgb(SOLARIZED_PAIR.0), rgb(SOLARIZED_PAIR.1));
    }
    host.rows = Some(page);
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    let keys = 24u16;
    for k in 1..=keys {
        t += Duration::from_millis(60);
        host.frame(t, (row, start + k), true);
    }
    host.frame(t + Duration::from_millis(16), (row, start + keys), false);
    let mut lit = Glass::new(g, SOLARIZED_PAIR.1);
    lit.add_quads(host.fx.glow.under_quads());
    lit.add_quads(&host.fx.glow_scratch);
    lit.add_halos(host.fx.glow.halos());
    let page = SOLARIZED_PAIR.1;
    let ch = |c: u32, sh: u32| ((c >> sh) & 0xFF) as i32;
    let x0 = i32::from(g.origin_x) + i32::from(start) * g.cw as i32;
    let x1 = i32::from(g.origin_x) + i32::from(start + keys) * g.cw as i32;
    let y0 = i32::from(g.origin_y) + i32::from(row) * g.ch as i32;
    let y1 = y0 + 2 * g.ch as i32;
    (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (y, x)))
        .filter(|&(y, x)| {
            let p = lit.at(y, x) & 0x00FF_FFFF;
            [16, 8, 0].iter().any(|&sh| ch(p, sh) - ch(page, sh) >= 24)
        })
        .count()
}

/// WATER READS WHILE TYPING ON A LIT PAGE (looks8 K4): on Solarized Dark
/// the letter law held Water's stream to a whisper behind the caret (the
/// gallery's 23 px lifted a 24th or more); it lays Beam's and Lumen's lit
/// page bed there, its rail in the air below the letters
/// ([`crate::cursor_glow::bed::BedShape::WATER_LIT`]). Negative control:
/// without it, the stream behind the caret is a whisper.
#[test]
fn water_reads_while_typing_on_a_lit_page() {
    let kept = water_on_solarized(true);
    let bare = water_on_solarized(false);
    assert!(
        kept >= 4 * bare.max(50),
        "Water behind the caret on Solarized Dark lifts {kept} px (bare {bare})"
    );
    assert!(bare < 200, "control: the bare stream lifts {bare} px");
}

/// The rainbow kitty typing `echo the quick brown fox` on Solarized Dark
/// over a filled, lettered row below (a selection), the frame 300 ms after
/// its last key, judged as the pixels it lifts by 8 or more over the page
/// in the typed span: `(bed, rail, at_foot)`, the bed over the row's upper
/// cells, the rail in its last three pixel rows, and of those the pixels
/// within the judge's ring of the `q`, the row's one descender. With
/// `row_rail` off the rail is kept below its row; with `weighed` the letter
/// guard weighs and cuts the rail in its row's foot band as any light.
fn ribbon_on_solarized_selection(row_rail: bool, weighed: bool) -> (usize, usize, usize) {
    let g = tall_geom();
    let (row, start) = (3u16, 9u16);
    let mut host = FireHost::styled(GlowStyle::RainbowKitty, SOLARIZED_PAIR, true, false);
    host.fx.glow.v2.ribbon_mut().row_rail_off = !row_rail;
    host.fx.glow.ink_ledger.foot_rails_weighed = weighed;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for cell in page.iter_mut().flatten() {
        (cell.fg, cell.bg) = (rgb(SOLARIZED_PAIR.0), rgb(SOLARIZED_PAIR.1));
    }
    for cell in &mut page[usize::from(row + 1)] {
        (cell.ch, cell.bg) = ('s', rgb(0x0033_415E));
    }
    host.rows = Some(page);
    let word = "echo the quick brown fox";
    let mut typed = vec![' '; g.cols];
    for (k, c) in word.chars().enumerate() {
        typed[usize::from(start) + k] = c;
    }
    host.typed = typed;
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    let keys = word.len() as u16;
    for k in 1..=keys {
        t += Duration::from_millis(60);
        host.frame(t, (row, start + k), true);
    }
    host.frame(t + Duration::from_millis(300), (row, start + keys), false);
    let mut lit = Glass::new(g, SOLARIZED_PAIR.1);
    lit.add_quads(host.fx.glow.under_quads());
    lit.add_quads(&host.fx.glow_scratch);
    lit.add_halos(host.fx.glow.halos());
    let page = SOLARIZED_PAIR.1;
    let ch = |c: u32, sh: u32| ((c >> sh) & 0xFF) as i32;
    let (cw, chi) = (g.cw as i32, g.ch as i32);
    let x_of = |col: u16| i32::from(g.origin_x) + i32::from(col) * cw;
    let y0 = i32::from(g.origin_y) + i32::from(row) * chi;
    let yb = y0 + chi;
    let count = |(xa, xb): (i32, i32), (ya, yb): (i32, i32)| {
        (ya..yb)
            .flat_map(|y| (xa..xb).map(move |x| (y, x)))
            .filter(|&(y, x)| {
                let p = lit.at(y, x) & 0x00FF_FFFF;
                [16, 8, 0].iter().any(|&sh| ch(p, sh) - ch(page, sh) >= 8)
            })
            .count()
    };
    let span = (x_of(start), x_of(start + keys - 1));
    let band = (yb - 3, yb);
    let q = start + word.find('q').expect("the line has a q") as u16;
    let ring = crate::cursor_glow::LETTER_GUARD_RING;
    (
        count(span, (y0, yb - 4)),
        count(span, band),
        count((x_of(q) - ring, x_of(q + 1) + ring), band),
    )
}

/// THE RAIL KEEPS OFF A FILLED ROW AND THE LETTERS (looks8 K9): typed on
/// Solarized Dark over a selection, the rainbow kitty lays its rail in its
/// own row's foot band, parted round the `q`'s descender, and keeps its
/// whole bed: the letter guard reads a lettered cell's whole box as ink, so
/// the rail is noted as a foot rail it neither weighs nor cuts
/// ([`crate::cursor_glow::InkLedger::note_foot_rail`]). Negative controls:
/// kept below its row the rail lays nothing in the band (the selection's
/// letters cut it); weighed as any light, the rail is the brightest ground
/// of every cell of its row and the row's cuts take the bed with it.
#[test]
fn the_rail_keeps_off_a_selection_and_the_letters() {
    let (bed, rail, at_foot) = ribbon_on_solarized_selection(true, false);
    let (bare_bed, bare_rail, _) = ribbon_on_solarized_selection(false, false);
    let (weighed_bed, weighed_rail, _) = ribbon_on_solarized_selection(true, true);
    assert!(
        rail >= 600,
        "the rail lifts {rail} px in its row's foot band (kept below it, {bare_rail})"
    );
    assert_eq!(
        at_foot, 0,
        "the rail lifts {at_foot} px within the judge's ring of the q"
    );
    assert!(
        bed * 10 >= bare_bed * 9,
        "the bed lifts {bed} px beside the rail, {bare_bed} with the rail kept below"
    );
    assert!(
        bare_rail <= 20,
        "control: kept below its row, the rail lifts {bare_rail} px in the band"
    );
    assert!(
        weighed_bed * 4 <= bed && weighed_rail * 2 <= rail,
        "control: weighed as any light, the rail ({weighed_rail} px) leaves the bed {weighed_bed} px"
    );
}

/// The rainbow kitty typing `echo the quick brown fox` on a dark page in a
/// short row (8 x 16 px, a 13 px font at 1x: the Settings card's cursor
/// demo), the frame's text handed in, the frame 300 ms after its last key:
/// how far its bed lifts the page (the sum over the pixels of each one's
/// greatest channel lift) in the typed span's cells but the last four, above
/// their baseline band, with the letter guard on and with it off. With `rounded` the rail's hairline is rounded alone
/// (a pixel on this row, inside the judge's ring).
fn ribbon_bed_on_a_short_row(rounded: bool) -> (usize, usize) {
    let g = geom();
    let (row, start) = (2u16, 2u16);
    let word = "echo the quick brown fox";
    let bed = |guarded: bool| {
        let mut host = FireHost::styled(GlowStyle::RainbowKitty, DARK_PAIR, true, false);
        host.geom = g;
        host.text = vec![' '; g.cols];
        host.fx.glow.v2.ribbon_mut().rail_gap_rounded = rounded;
        host.fx.glow.set_letter_guard(guarded);
        let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for cell in page.iter_mut().flatten() {
            (cell.fg, cell.bg) = (rgb(DARK_PAIR.0), rgb(DARK_PAIR.1));
        }
        host.rows = Some(page);
        let mut typed = vec![' '; g.cols];
        for (k, c) in word.chars().enumerate() {
            typed[usize::from(start) + k] = c;
        }
        host.typed = typed;
        let mut t = Instant::now();
        host.frame(t, (row, start), false);
        let keys = word.len() as u16;
        for k in 1..=keys {
            t += Duration::from_millis(60);
            host.frame(t, (row, start + k), true);
        }
        host.frame(t + Duration::from_millis(300), (row, start + keys), false);
        let mut lit = Glass::new(g, DARK_PAIR.1);
        lit.add_quads(host.fx.glow.under_quads());
        lit.add_quads(&host.fx.glow_scratch);
        lit.add_halos(host.fx.glow.halos());
        let page = DARK_PAIR.1;
        let ch = |c: u32, sh: u32| ((c >> sh) & 0xFF) as i32;
        let (cw, chi) = (g.cw as i32, g.ch as i32);
        let x_of = |col: u16| i32::from(g.origin_x) + i32::from(col) * cw;
        let y0 = i32::from(g.origin_y) + i32::from(row) * chi;
        (y0..y0 + chi - 4)
            .flat_map(|y| (x_of(start)..x_of(start + keys - 4)).map(move |x| (y, x)))
            .map(|(y, x)| {
                let p = lit.at(y, x) & 0x00FF_FFFF;
                [16, 8, 0]
                    .iter()
                    .map(|&sh| (ch(p, sh) - ch(page, sh)).max(0))
                    .max()
                    .unwrap_or(0) as usize
            })
            .sum::<usize>()
    };
    (bed(true), bed(false))
}

/// THE RIBBON KEEPS ITS BED OVER LETTERS ON A SHORT ROW (the Settings cursor
/// demo's dark ribbon, settings round 2): on a 16 px row the rail's hairline
/// is the judge's ring ([`crate::rainbow_kitty::ribbon::rail_gap_px`]), so the
/// rail lies clear of its row's ring and the letter guard, which reads a
/// lettered cell's whole box as ink, weighs none of it for the letters above:
/// a row whose letters read whole keeps its bed. Negative control: the
/// hairline rounded alone, a pixel, the guard weighed the rail's vivid ink as
/// every cell's brightest ground and cut the row's bed with it.
#[test]
fn the_ribbon_keeps_its_bed_over_letters_on_a_short_row() {
    let (bed, uncut) = ribbon_bed_on_a_short_row(false);
    let (rounded_bed, rounded_uncut) = ribbon_bed_on_a_short_row(true);
    assert!(
        uncut >= 10_000,
        "premise: the bed lifts the page by {uncut} uncut"
    );
    assert!(
        bed * 10 >= uncut * 9,
        "the guarded bed lifts the page by {bed} of the {uncut} it lifts uncut"
    );
    assert!(
        rounded_bed * 10 < rounded_uncut * 8,
        "control: with a pixel's hairline the guarded bed lifts the page by {rounded_bed} of \
         {rounded_uncut}"
    );
}

/// What the rainbow kitty's in-row rail lights round the `q` of `quick`
/// ([`rail_dip`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RailDip {
    /// The pixel columns of the typed span's rail band (its foot band and
    /// the selection's top below it) the rail leaves dark.
    gaps: usize,
    /// The pixels it lifts in the foot band within the judge's ring of the
    /// `q`'s cell.
    at_foot: usize,
    /// The pixels it lifts in that band on the selection's lettered cells
    /// beside the `q` whose glyphs may reach their cell's top.
    on_tall: usize,
    /// The pixels it lifts on the selection's short glyphs beside the `q`
    /// within the judge's ring of where their ink may begin
    /// (`emit::SHORT_GLYPH_TOP_CH`).
    on_short: usize,
}

/// The rainbow kitty typing `echo the quick brown fox` on Solarized Dark
/// over a selection whose cells hold `below(col, q)` (`' '` blank; `q` the
/// `q`'s column), `tune` setting its controls, the frame 300 ms after its
/// last key ([`RailDip`]).
fn rail_dip(below: impl Fn(usize, usize) -> char, tune: impl FnOnce(&mut CursorGlow)) -> RailDip {
    let g = tall_geom();
    let (row, start) = (3u16, 9u16);
    let mut host = FireHost::styled(GlowStyle::RainbowKitty, SOLARIZED_PAIR, true, false);
    tune(&mut host.fx.glow);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let word = "echo the quick brown fox";
    let q = start + word.find('q').expect("the line has a q") as u16;
    let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for cell in page.iter_mut().flatten() {
        (cell.fg, cell.bg) = (rgb(SOLARIZED_PAIR.0), rgb(SOLARIZED_PAIR.1));
    }
    let glyph_at = |col: usize| below(col, usize::from(q));
    for (col, cell) in page[usize::from(row + 1)].iter_mut().enumerate() {
        cell.bg = rgb(0x0033_415E);
        cell.ch = glyph_at(col);
    }
    host.rows = Some(page);
    let mut typed = vec![' '; g.cols];
    for (k, c) in word.chars().enumerate() {
        typed[usize::from(start) + k] = c;
    }
    host.typed = typed;
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    let keys = word.len() as u16;
    for k in 1..=keys {
        t += Duration::from_millis(60);
        host.frame(t, (row, start + k), true);
    }
    host.frame(t + Duration::from_millis(300), (row, start + keys), false);
    let mut lit = Glass::new(g, SOLARIZED_PAIR.1);
    lit.add_quads(host.fx.glow.under_quads());
    lit.add_quads(&host.fx.glow_scratch);
    lit.add_halos(host.fx.glow.halos());
    let page = SOLARIZED_PAIR.1;
    let ch = |c: u32, sh: u32| ((c >> sh) & 0xFF) as i32;
    let (cw, chi) = (g.cw as i32, g.ch as i32);
    let x_of = |col: u16| i32::from(g.origin_x) + i32::from(col) * cw;
    let yb = i32::from(g.origin_y) + (i32::from(row) + 1) * chi;
    let lifted = |y: i32, x: i32| {
        let p = lit.at(y, x) & 0x00FF_FFFF;
        [16, 8, 0].iter().any(|&sh| ch(p, sh) - ch(page, sh) >= 8)
    };
    let ring = crate::cursor_glow::LETTER_GUARD_RING;
    let short_top = (super::super::emit::SHORT_GLYPH_TOP_CH * chi as f32).floor() as i32;
    let col_of = |x: i32| ((x - i32::from(g.origin_x)) / cw) as usize;
    // The rail's band: its own row's foot band and the dip below it.
    let band = yb - 3..yb + ring + 4;
    let gaps = (x_of(start)..x_of(start + keys - 1))
        .filter(|&x| !band.clone().any(|y| lifted(y, x)))
        .count();
    let at_foot = (yb - 3..yb)
        .flat_map(|y| (x_of(q) - ring..x_of(q + 1) + ring).map(move |x| (y, x)))
        .filter(|&(y, x)| lifted(y, x))
        .count();
    let short = super::super::emit::is_short_glyph;
    let beside = || (x_of(q - 3)..x_of(q + 4)).filter(|&x| glyph_at(col_of(x)) != ' ');
    let on_tall = band
        .clone()
        .filter(|&y| y >= yb)
        .flat_map(|y| beside().map(move |x| (y, x)))
        .filter(|&(_, x)| !short(glyph_at(col_of(x))))
        .filter(|&(y, x)| lifted(y, x))
        .count();
    let on_short = (yb + short_top - ring..yb + short_top + 4)
        .flat_map(|y| beside().map(move |x| (y, x)))
        .filter(|&(_, x)| short(glyph_at(col_of(x))))
        .filter(|&(y, x)| lifted(y, x))
        .count();
    RailDip {
        gaps,
        at_foot,
        on_tall,
        on_short,
    }
}

/// [`rail_dip`] over a selection whose letters leave blank the cells under
/// the `q` and a cell each side (`s` on the rest), parted or not: `(gaps,
/// at_foot, on_letters)`, the last the pixels lifted on the selection's
/// lettered cells beside the `q`.
fn rail_under_a_descender(parted: bool) -> (usize, usize, usize) {
    let r = rail_dip(
        |col, q| {
            if (q - 1..=q + 1).contains(&col) {
                ' '
            } else {
                's'
            }
        },
        |glow| glow.v2.ribbon_mut().row_rail_parted = parted,
    );
    (r.gaps, r.at_foot, r.on_tall + r.on_short)
}

/// The gallery `context`'s selection line, `src/trail.rs:412  a selected
/// line under the prompt`, laid with its `2` two cells before the `q` above
/// it: the cells under the `q` and the one before it blank, the `a` under
/// the cell after it.
fn gallery_selection(col: usize, q: usize) -> char {
    const LINE: &str = "src/trail.rs:412  a selected line under the prompt";
    let two = LINE.find('2').expect("the line has a 2");
    (col + two + 2)
        .checked_sub(q)
        .and_then(|i| LINE.chars().nth(i))
        .unwrap_or(' ')
}

/// THE ROW RAIL DIPS UNDER A DESCENDER (looks9 Q9): over Solarized Dark's
/// selection in the gallery's `context` the rainbow kitty's in-row rail
/// parted round the `q` of `quick` and broke there. Where the selection's
/// cells under the `q` are blank it dips under the row into their top and
/// stays one rail: no pixel column of the typed span's rail band is dark,
/// it lifts nothing in the foot band within the judge's ring of the `q`, and
/// it adds no light on the selection's lettered cells beside the dip.
/// Negative control: parted as it was, the rail breaks.
#[test]
fn the_row_rail_dips_under_a_descender() {
    let (gaps, at_foot, on_letters) = rail_under_a_descender(false);
    let (parted_gaps, _, parted_letters) = rail_under_a_descender(true);
    assert_eq!(
        gaps, 0,
        "the rail leaves {gaps} px columns of its band dark"
    );
    assert_eq!(
        at_foot, 0,
        "the rail lifts {at_foot} px within the q's ring"
    );
    // The row's own glow feathers into the selection's top either way.
    assert_eq!(
        on_letters, parted_letters,
        "the dip lifts {on_letters} px on the selection's letters ({parted_letters} parted)"
    );
    assert!(
        parted_gaps >= 10,
        "control: parted round the q, the rail leaves only {parted_gaps} columns dark"
    );
}

/// looks9 Q9, round 2: THE ROW RAIL DIPS PAST A SHORT GLYPH BELOW. In the
/// gallery's `context` on Solarized Dark the selection holds `412  a` under
/// `quick`: the `a` lies under the cell after the `q`'s, and the dip, which
/// kept off every lettered cell below whole, ended at the `q`'s stem and
/// left a 10 px gap past it. Over that selection the rail is one rail (no
/// pixel column of the typed span's rail band dark), lifts nothing within
/// the judge's ring of the `q`'s cell in the foot band, nor within the ring
/// of where a short glyph's ink may begin below, and lights the tall glyphs'
/// cells below (the `2`) no more than parted. Negative control: with every
/// lettered cell kept whole, the dip stops at the `a` and the rail breaks.
#[test]
fn the_row_rail_dips_past_a_short_glyph_below() {
    let dip = rail_dip(gallery_selection, |_| {});
    let parted = rail_dip(gallery_selection, |glow| {
        glow.v2.ribbon_mut().row_rail_parted = true;
    });
    assert_eq!(dip.gaps, 0, "the rail breaks: {dip:?}");
    assert_eq!(dip.at_foot, 0, "the rail lights the q's ring: {dip:?}");
    assert_eq!(
        (dip.on_tall, dip.on_short),
        (parted.on_tall, parted.on_short),
        "the dip lights the letters below: {dip:?}, parted {parted:?}"
    );
    let whole = rail_dip(gallery_selection, |glow| glow.letters_whole_cells = true);
    assert!(
        whole.gaps >= 8,
        "control: with every lettered cell whole the rail reads {whole:?}"
    );
}

/// The glass after `keys` keys typed at 60 ms from column 2 of row 3 by a
/// [`FireHost`] of `style` on `pair`, `tune` setting its controls, and the
/// frame 16 ms after the last key; `enter` then moves the caret to the next
/// row's column 2 and draws one more frame 16 ms later.
fn typed_glass(
    style: GlowStyle,
    pair: (u32, u32),
    keys: u16,
    enter: bool,
    tune: impl FnOnce(&mut CursorGlow),
) -> Glass {
    let g = tall_geom();
    let (row, start) = (3u16, 2u16);
    let mut host = FireHost::styled(style, pair, true, false);
    tune(&mut host.fx.glow);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for cell in page.iter_mut().flatten() {
        (cell.fg, cell.bg) = (rgb(pair.0), rgb(pair.1));
    }
    host.rows = Some(page);
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    for k in 1..=keys {
        t += Duration::from_millis(60);
        host.frame(t, (row, start + k), true);
    }
    t += Duration::from_millis(16);
    host.frame(t, (row, start + keys), false);
    if enter {
        host.text = vec![' '; g.cols];
        t += Duration::from_millis(16);
        host.frame(t, (row + 1, start), false);
    }
    let mut lit = Glass::new(g, pair.1);
    lit.add_quads(host.fx.glow.under_quads());
    lit.add_quads(&host.fx.glow_scratch);
    lit.add_halos(host.fx.glow.halos());
    lit
}

/// The pixels of `lit` in rows `ys` and columns `xs` whose `channel` (16 red,
/// 8 green, 0 blue) rises `by` or more over `page`'s.
fn lifted_px(
    lit: &Glass,
    page: u32,
    ys: std::ops::Range<i32>,
    xs: std::ops::Range<i32>,
    channel: u32,
    by: i32,
) -> Vec<(i32, i32)> {
    let ch = |c: u32| ((c >> channel) & 0xFF) as i32;
    ys.flat_map(|y| xs.clone().map(move |x| (y, x)))
        .filter(|&(y, x)| ch(lit.at(y, x) & 0x00FF_FFFF) - ch(page) >= by)
        .collect()
}

/// WATER'S LIT RAIL SWELLS (looks8 round 2's K4): on Solarized Dark Water's
/// rail in the air below the typed line was a straight cyan bar, Beam's tube
/// in another hue; its spine now falls and rises below the row as the crest
/// swells, never into it. Read down each column of the rail's band under
/// the line's last five cells (two cells' columns of it at least: the foam
/// breaks where it will, looks9 round 2's Q3), the brightest pixel's row
/// wanders over three pixels or more. Negative control: the straight rail's
/// stays within a pixel.
#[test]
fn water_s_lit_rail_swells() {
    let g = tall_geom();
    let wander = |straight: bool| {
        let lit = typed_glass(GlowStyle::Water, SOLARIZED_PAIR, 24, false, |glow| {
            glow.water_rail_straight = straight;
        });
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let band = 4 * ch..4 * ch + ch / 2;
        let mut peaks = Vec::new();
        for x in 21 * cw..26 * cw {
            let mut best: Option<(i32, i32)> = None;
            for y in band.clone() {
                let b = (lit.at(y, x) & 0xFF) as i32;
                if b - (SOLARIZED_PAIR.1 & 0xFF) as i32 >= 40 && best.is_none_or(|(_, v)| b > v) {
                    best = Some((y, b));
                }
            }
            if let Some((y, _)) = best {
                peaks.push(y);
            }
        }
        assert!(
            peaks.len() >= 2 * cw as usize,
            "fixture: the rail lies under the line ({} columns)",
            peaks.len()
        );
        peaks.iter().max().unwrap_or(&0) - peaks.iter().min().unwrap_or(&0)
    };
    let (swell, straight) = (wander(false), wander(true));
    assert!(swell >= 3, "the rail's spine wanders {swell} px");
    assert!(
        straight <= 1,
        "control: the straight rail's spine wanders {straight} px"
    );
}

/// WATER'S LIT RAIL LEAVES WITH THE CARET (looks8 round 2's K4): after an
/// Enter on Solarized Dark the typed line's rail lay on as a cyan dash under
/// its last word, detached from the caret on the row below. The frame after
/// the Enter lights none of the band under the typed line's last two cells
/// (the Enter's own leap lights the air further back, as Beam's does).
/// Negative control: laid under every row, the dash is there. The line is 22
/// keys long, a crest of the foam under its last two cells (under 24 keys'
/// round 2's foam breaks, looks9 round 2's Q3).
#[test]
fn water_s_lit_rail_leaves_with_the_caret() {
    let g = tall_geom();
    let dash = |any_row: bool| {
        let lit = typed_glass(GlowStyle::Water, SOLARIZED_PAIR, 22, true, |glow| {
            glow.water_rail_any_row = any_row;
        });
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        lifted_px(
            &lit,
            SOLARIZED_PAIR.1,
            4 * ch..4 * ch + ch / 3,
            22 * cw..24 * cw,
            0,
            60,
        )
        .len()
    };
    let (left, kept) = (dash(false), dash(true));
    assert!(
        kept >= 30,
        "control: laid under every row, the typed line's rail lights {kept} px"
    );
    assert!(
        left <= 2,
        "after the Enter the typed line's rail lights {left} px of {kept}"
    );
}

/// Water's lit rail under the typed line's last five cells, typed 24 keys
/// on Solarized Dark (`crisp`: looks8's rail, [`CursorGlow::water_rail_crisp`]; `pale`:
/// looks9's sea foam, [`CursorGlow::water_foam_pale`]),
/// read in the air below the row as each pixel's lift over the page:
/// `(chroma, steepest, runs, drops)`, the most chroma any well-lit pixel's
/// lift carries for its brightness, the steepest one-pixel rise of any
/// column's light as a share of its peak, the runs of lit columns, and the
/// droplets (lit blobs of a few pixels standing alone).
fn water_rail_reading(crisp: bool, pale: bool) -> (f32, f32, usize, usize) {
    let g = tall_geom();
    let lit = typed_glass(GlowStyle::Water, SOLARIZED_PAIR, 24, false, |glow| {
        glow.water_rail_crisp = crisp;
        glow.water_foam_pale = pale;
    });
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let page = SOLARIZED_PAIR.1;
    let lift = |y: i32, x: i32| {
        let p = lit.at(y, x);
        let l = |sh: u32| (((p >> sh) & 0xFF) as i32 - ((page >> sh) & 0xFF) as i32).max(0);
        [l(16), l(8), l(0)]
    };
    let bright = |y: i32, x: i32| lift(y, x).into_iter().max().unwrap_or(0);
    let (xs, ys) = (21 * cw..26 * cw, 4 * ch..4 * ch + ch * 3 / 4);
    let (w, h) = (xs.len(), ys.len());
    let at = |i: usize| (ys.start + (i / w) as i32, xs.start + (i % w) as i32);
    // Lit blobs, 4-connected: a droplet is one of a few pixels.
    let on: Vec<bool> = (0..w * h).map(|i| bright(at(i).0, at(i).1) >= 20).collect();
    let mut blob = vec![usize::MAX; on.len()];
    let mut sizes = Vec::new();
    for first in 0..on.len() {
        if !on[first] || blob[first] != usize::MAX {
            continue;
        }
        let id = sizes.len();
        let (mut stack, mut size) = (vec![first], 0usize);
        blob[first] = id;
        while let Some(i) = stack.pop() {
            size += 1;
            let (r, c) = (i / w, i % w);
            let near = [
                (r > 0).then(|| i - w),
                (r + 1 < h).then(|| i + w),
                (c > 0).then(|| i - 1),
                (c + 1 < w).then(|| i + 1),
            ];
            for j in near.into_iter().flatten() {
                if on[j] && blob[j] == usize::MAX {
                    blob[j] = id;
                    stack.push(j);
                }
            }
        }
        sizes.push(size);
    }
    let drop = |i: usize| blob[i] != usize::MAX && sizes[blob[i]] <= 24;
    let drops = sizes.iter().filter(|&&n| n <= 24).count();
    // The crest: every pixel but the droplets'.
    let (mut chroma, mut steepest) = (0.0f32, 0.0f32);
    let mut lit_cols = Vec::with_capacity(w);
    for c in 0..w {
        let col: Vec<i32> = (0..h)
            .map(|r| r * w + c)
            .map(|i| if drop(i) { 0 } else { bright(at(i).0, at(i).1) })
            .collect();
        let peak = col.iter().copied().max().unwrap_or(0);
        lit_cols.push(peak >= 20);
        if peak >= 60 {
            let step = col
                .windows(2)
                .map(|v| (v[1] - v[0]).abs())
                .max()
                .unwrap_or(0);
            steepest = steepest.max(step as f32 / peak as f32);
        }
        for i in (0..h).map(|r| r * w + c).filter(|&i| !drop(i)) {
            let l = lift(at(i).0, at(i).1);
            let (hi, lo) = (
                l.into_iter().max().unwrap_or(0),
                l.into_iter().min().unwrap_or(0),
            );
            if hi >= 40 {
                chroma = chroma.max((hi - lo) as f32 / hi as f32);
            }
        }
    }
    let runs = lit_cols.windows(2).filter(|v| v[1] && !v[0]).count() + usize::from(lit_cols[0]);
    (chroma, steepest, runs, drops)
}

/// WATER'S LIT RAIL IS FOAM (looks9 Q3), WATERY CYAN AND SOFT (looks10 L5): on Solarized
/// Dark its rail under the typed line was a crisp saturated 2-3 px cyan sine that read as a
/// spell-check squiggle (`solarized-dark/water-dark.t0`); looks9 made it a pale sea-foam aqua
/// whose crests rose from the page in two pixels, near-white hard-topped arcs (`#83d5db`). It
/// is foam on a body of water in watery cyan: the most chroma a well-lit pixel's lift carries
/// is between 0.45 and 0.75 of its brightness (its hue held, never the squiggle's whole
/// chroma), and soft (no column's light rises by three tenths of its peak in one pixel), and
/// its body keeps the rail whole (one run of lit columns; broken into runs with a droplet in
/// each break, looks9 round 2's foam was a dash-dot rule,
/// [`water_s_foam_rides_one_body_of_water`]; its crests break as
/// [`water_s_foam_never_keeps_one_pitch`] reads). Negative controls: looks8's rail is
/// saturated and steep, and looks9's sea foam pale and steeper.
#[test]
fn water_s_lit_rail_is_foam() {
    let (chroma, steepest, runs, ..) = water_rail_reading(false, false);
    let crisp = water_rail_reading(true, false);
    let pale = water_rail_reading(false, true);
    assert!(
        (0.45..=0.75).contains(&chroma),
        "the foam's light carries chroma {chroma:.2} of its brightness"
    );
    assert!(
        steepest <= 0.3,
        "the foam's light rises {steepest:.2} of its peak in one pixel"
    );
    assert_eq!(runs, 1, "the foam's body breaks into {runs} runs");
    assert!(
        crisp.0 >= 0.9 && crisp.1 >= 0.6,
        "control: looks8's rail (chroma, steepest, runs, drops) reads {crisp:?}"
    );
    assert!(
        pale.0 < 0.45 && pale.1 > 0.3,
        "control: looks9's sea foam (chroma, steepest, runs, drops) reads {pale:?}"
    );
}

/// Water's foam laid on a flat rail 60 cells long at the tall cell (13 x 26
/// px), its swell as the gallery's (`even`: looks9 round 1's foam,
/// [`crate::cursor_glow::bed::BedScratch::foam_even`]): `(pitch, heights,
/// sizes, bare)`, the longest over the shortest distance between successive
/// troughs of its spine, the spread in px of its crests' heights, the
/// largest over the smallest droplet, and the share of its open breaks (a
/// trough whose light the foam takes) that shed no droplet. The rail's last
/// three cells, where it feathers in to the caret, are left out.
fn foam_on_a_flat_rail(even: bool) -> (f32, f32, f32, f32) {
    use crate::cursor_glow::bed::{FOAM_SWELL_VARY, foam_rail, swell_rail};
    let g = tall_geom();
    let (cw, ch) = (g.cw as f32, g.ch as f32);
    let vert = |x: f32| aterm_render::RibbonVertex {
        x,
        spine: 0.0,
        up: 2.0,
        dn: 2.0,
        core_up: 1.0,
        core_dn: 1.0,
        color: 0x0000_c9d7,
        cov: 1.0,
        lift: 0.0,
        lift_span: 0.0,
    };
    let mut verts = vec![vert(0.0), vert(60.0 * cw)];
    let wave = (0.14 * ch, 1.8 / cw, 0.0);
    let vary = if even { 0.0 } else { FOAM_SWELL_VARY };
    swell_rail(&mut verts, wave, cw, vary);
    let spines: Vec<f32> = verts.iter().map(|v| v.spine).collect();
    let mut drops = Vec::new();
    foam_rail(&mut verts, wave, (ch, cw), (even, false), &mut drops);
    let reach = 57.0 * cw;
    let (mut troughs, mut crests) = (Vec::new(), Vec::new());
    for i in 1..verts.len() - 1 {
        if verts[i].x > reach {
            break;
        }
        let (a, v, b) = (spines[i - 1], spines[i], spines[i + 1]);
        if v > a && v >= b {
            troughs.push(i);
        }
        if v < a && v <= b {
            crests.push(v);
        }
    }
    let pitches: Vec<f32> = troughs
        .windows(2)
        .map(|w| verts[w[1]].x - verts[w[0]].x)
        .collect();
    let spread = |v: &[f32]| {
        let (lo, hi) = v
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &p| (lo.min(p), hi.max(p)));
        (lo, hi)
    };
    let (short, long) = spread(&pitches);
    let (low, high) = spread(&crests);
    let sizes: Vec<f32> = drops.iter().filter(|d| d.0 <= reach).map(|d| d.2).collect();
    let (least, most) = spread(&sizes);
    let open: Vec<f32> = troughs
        .iter()
        .filter(|&&i| verts[i].cov < 0.5)
        .map(|&i| verts[i].x)
        .collect();
    let bare = open
        .iter()
        .filter(|&&x| !drops.iter().any(|d| (d.0 - x).abs() < 0.5 * cw))
        .count();
    (
        long / short,
        high - low,
        most / least,
        bare as f32 / open.len().max(1) as f32,
    )
}

/// looks9 round 2's Q3: WATER'S FOAM NEVER KEEPS ONE PITCH. On Solarized
/// Dark round 1's foam broke at every trough a like arc's length apart, a
/// like droplet under each, its crests of one height: a dash-dot rule. On a
/// flat rail 60 cells long its troughs fall closer and farther apart (the
/// longest pitch 1.4 times the shortest or more), its crests stand a pixel
/// or more apart in height, its droplets run from small to large (the
/// largest 1.5 times the smallest or more) and one in ten or more of its
/// open breaks sheds none. Negative control: round 1's even foam keeps one pitch
/// (within 1.2, the quarter-cell vertices'), one height (within 0.3 px), one
/// droplet and sheds one under every break.
#[test]
fn water_s_foam_never_keeps_one_pitch() {
    let (pitch, heights, sizes, bare) = foam_on_a_flat_rail(false);
    let even = foam_on_a_flat_rail(true);
    assert!(pitch >= 1.4, "the foam's pitch varies {pitch:.2} times");
    assert!(
        heights >= 1.0,
        "the foam's crests stand {heights:.2} px apart"
    );
    assert!(sizes >= 1.5, "the foam's droplets vary {sizes:.2} times");
    assert!(bare >= 0.1, "{bare:.2} of the foam's breaks shed nothing");
    assert!(
        even.0 <= 1.2 && even.1 <= 0.3 && even.2 <= 1.0 && even.3 == 0.0,
        "control: round 1's foam (pitch, heights, sizes, bare) reads {even:?}"
    );
}

/// The longest run of unlit columns (lifted under 10 over the page) in the
/// air below the typed row from six cells behind the caret to a cell and a
/// half behind it, Water typed `keys` keys on Solarized Dark (`bodiless`:
/// its crests and droplets alone, [`CursorGlow::water_foam_bodiless`]).
fn foam_longest_gap(keys: u16, bodiless: bool) -> i32 {
    let g = tall_geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let page = SOLARIZED_PAIR.1;
    let lit = typed_glass(GlowStyle::Water, SOLARIZED_PAIR, keys, false, |glow| {
        glow.water_foam_bodiless = bodiless;
    });
    let caret = 2 + i32::from(keys);
    let lift = |x: i32| {
        (4 * ch..4 * ch + ch * 3 / 4)
            .map(|y| {
                let p = lit.at(y, x);
                [16u32, 8, 0]
                    .map(|sh| (((p >> sh) & 0xFF) as i32 - ((page >> sh) & 0xFF) as i32).max(0))
                    .into_iter()
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0)
    };
    let (mut run, mut longest) = (0, 0);
    for x in (caret - 6) * cw..caret * cw - cw * 3 / 2 {
        run = if lift(x) < 10 { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// looks9 round 3's Q3: WATER'S FOAM RIDES ONE BODY OF WATER. On Solarized
/// Dark round 2's foam was still pale crests broken over the troughs, a
/// droplet between them at one height under the row: a dash-dot rule at
/// 1x. Its crests now ride a soft swell of water, whole along the rail and
/// fading downward ([`super::bed::FOAM_BODY`]): typed 20 to 36 keys, no
/// column of the rail's light from six cells behind the caret to a cell and
/// a half behind it is unlit (none lifted under 10 over the page). Negative
/// control: the crests and droplets alone (`water_foam_bodiless`) leave a
/// gap of four columns or more on every key.
#[test]
fn water_s_foam_rides_one_body_of_water() {
    for keys in (20u16..=36).step_by(4) {
        let (gap, bare) = (foam_longest_gap(keys, false), foam_longest_gap(keys, true));
        assert_eq!(gap, 0, "{keys} keys: the foam leaves {gap} columns unlit");
        assert!(
            bare >= 4,
            "{keys} keys: control: the crests alone leave only {bare} columns unlit"
        );
    }
}

/// How deep below the typed row Water's light reaches (the deepest pixel
/// lifted 10 or more over its ground), from six cells behind the caret to a
/// cell and a half behind it, typed 24 keys on Solarized Dark with the row
/// under the typed one bare of letters, on the page's ground or `filled`
/// with a ground apart from the page's (Solarized's `base01`, a selection or
/// a bar; `text_only`: the body reads the row's letters alone,
/// [`CursorGlow::water_body_text_only`]). The rail lies under the row, as
/// it does where the frame names no feet for its row: laid in its own row
/// over a filled one ([`super::bed::BedShape::row_rail`]), it keeps its body
/// off the fill whatever depth the body takes.
fn water_body_depth(filled: bool, text_only: bool) -> i32 {
    let g = tall_geom();
    let (row, start, keys) = (3u16, 2u16, 24u16);
    let pair = SOLARIZED_PAIR;
    let fill = 0x0058_6E75u32;
    let mut host = FireHost::styled(GlowStyle::Water, pair, true, false);
    host.fx.glow.water_body_text_only = text_only;
    host.fx.glow.set_bed_row_rails(false);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for (r, line) in page.iter_mut().enumerate() {
        for cell in line.iter_mut() {
            let ground = if filled && r == usize::from(row + 1) {
                fill
            } else {
                pair.1
            };
            (cell.fg, cell.bg) = (rgb(pair.0), rgb(ground));
        }
    }
    host.rows = Some(page);
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    for k in 1..=keys {
        t += Duration::from_millis(60);
        host.frame(t, (row, start + k), true);
    }
    t += Duration::from_millis(16);
    host.frame(t, (row, start + keys), false);
    let ground = if filled { fill } else { pair.1 };
    let mut lit = Glass::new(g, ground);
    lit.add_quads(host.fx.glow.under_quads());
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let caret = i32::from(start + keys);
    let bottom = i32::from(row + 1) * ch;
    let xs = (caret - 6) * cw..caret * cw - cw * 3 / 2;
    (bottom..bottom + ch)
        .filter(|&y| !lifted_px(&lit, ground, y..y + 1, xs.clone(), 8, 10).is_empty())
        .map(|y| y - bottom + 1)
        .max()
        .unwrap_or(0)
}

/// looks9 round 3's Q3: WATER'S BODY TAKES ITS DEPTH ONLY OVER AIR. Under a
/// row bare of letters on the page's ground the body of water fades out a
/// half row down ([`super::bed::FOAM_BODY`]); under a filled row (a
/// selection, a bar) bare of letters it keeps to the rail's own depth, four
/// pixels or more shallower. Negative control: read by its letters alone
/// (`water_body_text_only`), the filled row takes the body as deep as air.
#[test]
fn water_s_body_takes_its_depth_only_over_air() {
    let (air, filled) = (
        water_body_depth(false, false),
        water_body_depth(true, false),
    );
    assert!(
        filled + 4 <= air,
        "over a filled row the body reaches {filled} px, over air {air} px"
    );
    let old = water_body_depth(true, true);
    assert!(
        old + 1 >= air,
        "control: read by its letters alone the filled row takes {old} px of {air}"
    );
}

/// The light of Water's foam at the caret, typed `keys` keys on Solarized
/// Dark (`head_cut`: round 1's foam, its last arc whole into the caret,
/// [`CursorGlow::water_foam_head_cut`]): the brightest lift over the page in
/// the last four columns of the caret's cell, in the air below the row, as
/// a share of the brightest anywhere there between the caret and twelve
/// cells behind it.
fn foam_light_at_the_caret(keys: u16, head_cut: bool) -> f32 {
    let g = tall_geom();
    let lit = typed_glass(GlowStyle::Water, SOLARIZED_PAIR, keys, false, |glow| {
        glow.water_foam_head_cut = head_cut;
    });
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let caret = 2 + i32::from(keys);
    let page = SOLARIZED_PAIR.1;
    let bright = |x: i32| {
        (4 * ch..4 * ch + ch * 3 / 4)
            .map(|y| {
                let p = lit.at(y, x);
                [16u32, 8, 0]
                    .map(|sh| (((p >> sh) & 0xFF) as i32 - ((page >> sh) & 0xFF) as i32).max(0))
                    .into_iter()
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0)
    };
    let end = (caret + 1) * cw;
    let head = (end - 4..end).map(bright).max().unwrap_or(0);
    let all = ((caret - 12) * cw..end).map(bright).max().unwrap_or(0);
    head as f32 / all.max(1) as f32
}

/// looks9 round 2's Q3: WATER'S FOAM FEATHERS IN TO THE CARET. On Solarized
/// Dark round 1's foam ran its last arc at full light to the caret cell's
/// edge and stopped there, cut flat. Typed 20 to 36 keys, the foam's light
/// in the last four columns of the caret's cell is never over half the
/// brightest behind it. Negative control: round 1's whole last arc
/// (`water_foam_head_cut`) brings 0.85 of it there or more on some key.
#[test]
fn water_s_foam_feathers_in_to_the_caret() {
    let worst = |cut: bool| {
        (20u16..=36)
            .step_by(2)
            .map(|k| foam_light_at_the_caret(k, cut))
            .fold(0.0f32, f32::max)
    };
    let (feathered, cut) = (worst(false), worst(true));
    assert!(
        feathered <= 0.5,
        "the foam brings {feathered:.2} of its light to the caret's edge"
    );
    assert!(
        cut >= 0.85,
        "control: round 1's last arc brings {cut:.2} of its light there"
    );
}

/// COMET READS ON THE REFERENCE DARK (looks8 round 2's K4): its typing trail
/// was a deep-blue sliver under the last letter and grey specks of dust. Its
/// tail is now ice, lifted over the plain rail's light and laid clear of the
/// letters' feet, from a white nucleus under the caret, and its dust is ice.
/// The band under the typed line's last six cells lights twice the blue
/// pixels it did. Negative control: the old rail, nucleus-less, and grey
/// dust.
#[test]
fn comet_reads_on_the_reference_dark() {
    let g = tall_geom();
    let blue = |grey: bool| {
        let lit = typed_glass(GlowStyle::Comet, DARK_PAIR, 16, false, |glow| {
            glow.comet_dust_grey = grey;
        });
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        lifted_px(
            &lit,
            DARK_PAIR.1,
            4 * ch..4 * ch + ch / 2,
            12 * cw..19 * cw,
            0,
            40,
        )
        .len()
    };
    let (icy, old) = (blue(false), blue(true));
    assert!(
        icy >= 2 * old.max(20),
        "the icy tail lights {icy} px, the old rail {old}"
    );
}

/// looks9 Q2, round 2: COMET'S ICY TAIL TAKES ITS DEPTH OVER A BARE ROW. Held
/// to the leading over the next row's letters even where that row is bare,
/// the tail under the typed line was a 2 px hairline on the reference dark.
/// Over a bare row, under the typed line's last four cells, the tail lights
/// a band at least 6 px deep (blue lifted 40 or more), twice what the leading
/// allows. Negative control: held to the leading (`comet_tail_hairline`), it
/// lights 3 px or less.
#[test]
fn comet_s_icy_tail_takes_its_depth_over_a_bare_row() {
    let g = tall_geom();
    let depth = |hairline: bool| {
        let lit = typed_glass(GlowStyle::Comet, DARK_PAIR, 16, false, |glow| {
            glow.comet_tail_hairline = hairline;
        });
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        (14 * cw..18 * cw)
            .map(|x| lifted_px(&lit, DARK_PAIR.1, 4 * ch..4 * ch + ch / 2, x..x + 1, 0, 40).len())
            .max()
            .unwrap_or(0)
    };
    let (deep, hairline) = (depth(false), depth(true));
    assert!(deep >= 6, "the icy tail lights a band {deep} px deep");
    assert!(
        hairline <= 3,
        "control: held to the leading the tail lights {hairline} px"
    );
}

/// How deep below the typed row Comet's icy tail reaches on the reference
/// dark (the deepest pixel row of its bed and rail, laid on the page, under
/// the typed line's last four cells lifted 40 or more in blue), typed 16 keys with the row under the
/// typed one bare of letters, on the page's ground or on `fill` (a
/// selection's or a powerline bar's; `text_only`: the tail reads that row's
/// letters alone, [`CursorGlow::comet_tail_text_only`]).
fn comet_tail_depth(fill: Option<u32>, text_only: bool) -> i32 {
    let g = tall_geom();
    let (row, start, keys) = (3u16, 2u16, 16u16);
    let pair = DARK_PAIR;
    let mut host = FireHost::styled(GlowStyle::Comet, pair, true, false);
    host.fx.glow.comet_tail_text_only = text_only;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for (r, line) in page.iter_mut().enumerate() {
        for cell in line.iter_mut() {
            let ground = match fill {
                Some(f) if r == usize::from(row + 1) => f,
                _ => pair.1,
            };
            (cell.fg, cell.bg) = (rgb(pair.0), rgb(ground));
        }
    }
    host.rows = Some(page);
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    for k in 1..=keys {
        t += Duration::from_millis(60);
        host.frame(t, (row, start + k), true);
    }
    t += Duration::from_millis(16);
    host.frame(t, (row, start + keys), false);
    let mut lit = Glass::new(g, pair.1);
    lit.add_quads(host.fx.glow.under_quads());
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let bottom = i32::from(row + 1) * ch;
    let xs = 14 * cw..18 * cw;
    (bottom..bottom + ch / 2)
        .filter(|&y| !lifted_px(&lit, pair.1, y..y + 1, xs.clone(), 0, 40).is_empty())
        .map(|y| y - bottom + 1)
        .max()
        .unwrap_or(0)
}

/// looks9 round 3's review: COMET'S ICY TAIL TAKES ITS DEPTH ONLY OVER AIR.
/// Over a row bare of letters on the page's ground the tail grows to its
/// whole depth ([`super::bed::ICY_AIR_RAIL_GROW`]); under a filled row bare
/// of letters, a selection's (`#264f78`) or a powerline bar's (`#303030`),
/// it keeps to the leading's depth, as over letters, two pixels or more
/// shallower than over air, and Water's body (since looks9 round 3's Q3)
/// does the same. Negative control: read by its letters alone
/// (`comet_tail_text_only`), the filled row takes the tail as deep as air.
#[test]
fn comet_s_icy_tail_takes_its_depth_only_over_air() {
    let air = comet_tail_depth(None, false);
    assert!(air >= 6, "over air the tail reaches only {air} px");
    for fill in [0x0026_4F78u32, 0x0030_3030] {
        let filled = comet_tail_depth(Some(fill), false);
        assert!(
            filled + 2 <= air,
            "over a filled row ({fill:06x}) the tail reaches {filled} px, over air {air} px"
        );
        let old = comet_tail_depth(Some(fill), true);
        assert!(
            old + 1 >= air,
            "control ({fill:06x}): read by its letters alone the filled row takes {old} px of {air}"
        );
    }
}

/// WATER'S DROPS BEAD ON A DARK PAGE (looks8 round 2's K4): on the
/// reference dark its flung and falling drops were navy smudges a hair over
/// the page; in the air, clear of every letter, each is a bead of clear blue
/// with a glint, as on paper. Under the typed line its drops light twice the
/// blue pixels they did. Negative control: the faint foam.
#[test]
fn water_s_drops_bead_on_a_dark_page() {
    let g = tall_geom();
    let beads = |dim: bool| {
        let lit = typed_glass(GlowStyle::Water, DARK_PAIR, 24, false, |glow| {
            glow.water_drops_dim = dim;
        });
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        lifted_px(
            &lit,
            DARK_PAIR.1,
            4 * ch + ch / 2..7 * ch,
            0..48 * cw,
            0,
            50,
        )
        .len()
    };
    let (bead, foam) = (beads(false), beads(true));
    assert!(
        bead >= 2 * foam.max(6),
        "the drops light {bead} px, the foam {foam}"
    );
}

/// CLASSIC'S SPECTRUM FACE ON DARK KEEPS ITS BYTES WITH THE CARET ON A LETTER
/// ([`CursorGlow::classic_keeps_its_bytes`]): the caret-letter punch
/// ([`CursorGlow::keep_light_off_caret_letter`]) leaves Classic's dark spectrum lights as the
/// letter guard does, byte for byte, with the momentum glow on and off: the caret typed
/// `echo quick fox`, then jumps back onto the `q` (an arrow key, a vim motion) and rests.
/// Punched, every one of the 12 frames after the move differed from v0.28's (the review of
/// looks8 round 3). Negative control: on paper, where Classic is guarded, the punch cuts the
/// caret's letter out of its landing ring's rim laid over the letters as it was
/// ([`crate::classic_wake::ClassicWake::ring_cut`]; whole and off the letters, looks10 L12,
/// the rim is not drawn inside the word).
#[test]
fn classic_s_dark_spectrum_keeps_its_bytes_with_the_caret_on_a_letter() {
    use crate::cursor_glow::CaretLetterLaw;
    const PAPER: (u32, u32) = (0x001F_2328, 0x00FF_FFFF);
    let g = tall_geom();
    let run = |pair: (u32, u32), momentum: bool, law_on: bool| {
        let mut host = FireHost::styled(GlowStyle::Classic, pair, momentum, false);
        host.fx
            .glow
            .set_caret_letter_law(CaretLetterLaw::LightOff, law_on);
        host.fx.glow.classic.ring_cut = pair == PAPER;
        let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for cell in page.iter_mut().flatten() {
            (cell.fg, cell.bg) = (rgb(pair.0), rgb(pair.1));
        }
        host.rows = Some(page);
        let word = "echo quick fox";
        let mut typed = vec![' '; g.cols];
        for (k, c) in word.chars().enumerate() {
            typed[2 + k] = c;
        }
        host.typed = typed;
        let mut t = Instant::now();
        host.frame(t, (3, 2), false);
        for k in 1..=word.len() as u16 {
            t += Duration::from_millis(60);
            host.frame(t, (3, 2 + k), true);
        }
        (0..12)
            .map(|_| {
                t += Duration::from_millis(16);
                host.frame(t, (3, 7), false);
                host.fx.glow_scratch.clone()
            })
            .collect::<Vec<_>>()
    };
    for momentum in [false, true] {
        let (on, off) = (
            run(DARK_PAIR, momentum, true),
            run(DARK_PAIR, momentum, false),
        );
        assert!(
            off.iter().any(|q| !q.is_empty()),
            "fixture: Classic lays over-text light after the move (momentum {momentum})"
        );
        let differ: Vec<usize> = (0..on.len()).filter(|&k| on[k] != off[k]).collect();
        assert!(
            differ.is_empty(),
            "the caret-letter punch changed Classic's dark spectrum frames {differ:?} \
             (momentum {momentum})"
        );
    }
    let (on, off) = (run(PAPER, false, true), run(PAPER, false, false));
    assert!(
        (0..on.len()).any(|k| on[k] != off[k]),
        "negative control: on paper the punch leaves Classic's light on the caret's letter"
    );
}

/// THE FRAME'S NIGHT GROUNDS LIE UNDER THE RAMPS' LIGHT TOO
/// ([`CursorGlow::ground_the_frame`]): the grounds laid once every light is in go in
/// front of the under stream, and its ramps move past them with the quads they stood
/// before, as the tick's own grounds move them; and the over stream's ramped lights lay
/// their grounds as its quads do. Left where they stood, every live ramp drew before the
/// grounds (production lays the ramps unfolded), and on a tinted dark page Sparkle's
/// grounds were painted over its own light: 58 ramps before 654 grounds, 202 px of its
/// tail greyed from gold `ef9203` towards `26272b` (the review of looks8 round 3); and the
/// over stream's 22 ramps laid none of their 33 grounds. With every run a ramp
/// ([`aterm_render::RampCost::ALWAYS`]) no ramp stands before a ground, and Sparkle's frame
/// on Solarized Dark composites exactly as the same stream laid in quads alone. Fixture:
/// the frame lays grounds and ramps in both streams. (Fire's flames are patches, and lay no
/// ramp here.)
#[test]
fn the_frame_s_night_grounds_lie_under_the_ramps_light_too() {
    let g = tall_geom();
    {
        let style = GlowStyle::Sparkle;
        let run = |ramps: bool| {
            let mut host = FireHost::styled(style, SOLARIZED_PAIR, true, false);
            host.cfg.color = crate::cursor_glow::style::SPARKLE_DEFAULT_COLOR;
            host.fx.glow.fold_glow_ramps(false);
            if ramps {
                host.fx.glow.set_ramp_cost(aterm_render::RampCost::ALWAYS);
            } else {
                host.fx.glow.set_glow_ramps(false);
            }
            let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
            let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for cell in page.iter_mut().flatten() {
                (cell.fg, cell.bg) = (rgb(SOLARIZED_PAIR.0), rgb(SOLARIZED_PAIR.1));
            }
            host.rows = Some(page);
            let mut t = Instant::now();
            host.frame(t, (3, 2), false);
            for k in 1..=16u16 {
                t += Duration::from_millis(60);
                host.frame(t, (3, 2 + k), true);
            }
            t += Duration::from_millis(16);
            host.frame(t, (3, 18), false);
            let ng = host
                .fx
                .glow
                .night_memo
                .and_then(|(_, ng)| ng)
                .expect("fixture: a night ground");
            let is_ground = |q: &GlowQuad| {
                q.alpha > 0
                    && q.color == aterm_render::premul_rgb(ng.ink, q.alpha)
                    && q.color2 == aterm_render::premul_rgb(ng.ink, q.alpha2)
            };
            let grounds = host
                .fx
                .glow
                .under_quads()
                .iter()
                .take_while(|q| is_ground(q))
                .count();
            let ats: Vec<u32> = host
                .fx
                .glow
                .under_ramps
                .items
                .iter()
                .map(|r| r.at)
                .collect();
            let over_ramps = host.fx.glow.out_ramps.items.len();
            let mut lit = Glass::new(g, SOLARIZED_PAIR.1);
            lit.add_quads(&host.fx.glow.under_laid());
            (grounds, ats, over_ramps, lit)
        };
        let (grounds, ats, over_ramps, lit) = run(true);
        let (_, _, _, oracle) = run(false);
        assert!(
            grounds > 0 && !ats.is_empty() && over_ramps > 0,
            "fixture: {style:?} lays {grounds} grounds, {} under-stream ramps and \
             {over_ramps} over-stream ramps",
            ats.len()
        );
        let before = ats.iter().filter(|&&at| (at as usize) < grounds).count();
        assert_eq!(
            before, 0,
            "{style:?}: under-stream ramps drawn before the night grounds ({ats:?})"
        );
        let differ = lit
            .px
            .iter()
            .zip(&oracle.px)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            differ, 0,
            "{style:?}: the ramped frame composites off the quads' in {differ} px"
        );
    }
}

/// The light Comet typing 20 keys on GitHub Light at `gap` ms a key
/// (`short`: its wedge as looks8 laid it), the frame after the last key:
/// `(reach, worst)`, the cells behind the caret whose counters its tail
/// darkens by 24 or more, and the least contrast of the typed letters.
fn paper_comet_tail(gap: u64, short: bool) -> (usize, f32) {
    let g = tall_geom();
    let pair = (0x001F_2328u32, 0x00FF_FFFFu32);
    let (row, start, keys) = (4u16, 2u16, 20u16);
    let (glow, out) = style_burst_on(
        GlowStyle::Comet,
        pair,
        (keys, gap),
        (None, None),
        true,
        &|glow| {
            glow.comet_light_tail_short = short;
        },
    );
    let mut glass = Glass::new(g, pair.1);
    glass.add_quads(glow.under_quads());
    let caret = start + keys;
    for col in start..caret {
        for (y, x) in glyph(g, row, col).0 {
            glass.px[y as usize * glass.w + x as usize] = pair.0;
        }
    }
    glass.add_quads(&out);
    glass.add_halos(glow.halos());
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let yc = i32::from(row) * ch + ch / 2;
    // A cell's counters (between its strokes) at the row's middle.
    let dark = |back: u16| {
        let x0 = i32::from(caret - back) * cw;
        [4, 8]
            .iter()
            .map(|&dx| {
                let p = glass.at(yc + 1, x0 + dx);
                [16u32, 8, 0]
                    .iter()
                    .map(|&sh| 255 - ((p >> sh) & 0xff) as i32)
                    .max()
                    .unwrap_or(0)
            })
            .min()
            .unwrap_or(0)
    };
    let reach = (1..keys).take_while(|&back| dark(back) >= 24).count();
    let worst = (start..caret)
        .map(|col| {
            let (i, c) = glyph(g, row, col);
            ratio(&glass, &i, &c)
        })
        .fold(f32::INFINITY, f32::min);
    (reach, worst)
}

/// THE PAPER COMET'S TAIL HAS PRESENCE (looks9 Q2): on paper Comet was
/// mostly a soft blue blob, its tail's light gone two cells behind the
/// caret (`comet-light.t0`). Typing on GitHub Light, its tail darkens the
/// counters of five cells or more behind the caret, and every typed letter
/// keeps 4.5:1. Negative control: the wedge as looks8 laid it reaches three
/// or fewer.
#[test]
fn the_paper_comet_s_tail_has_presence() {
    for gap in [60u64, 120] {
        let (reach, worst) = paper_comet_tail(gap, false);
        let (short, _) = paper_comet_tail(gap, true);
        assert!(reach >= 5, "{gap} ms: the tail reaches {reach} cells");
        assert!(
            worst >= 4.5,
            "{gap} ms: a letter in the tail reads {worst:.2}:1"
        );
        assert!(
            short <= 3,
            "{gap} ms: control: looks8's tail reaches {short} cells"
        );
    }
}

/// The light Comet (`tube`: laid as looks9 laid it) typing 20 keys on GitHub Light, the
/// frame after the last key, lights alone on the page: for the cells one and four behind the
/// caret, the rows its tail lights in the cell's middle column and the hue of its spine there.
fn paper_comet_girth(tube: bool) -> [(usize, f32); 2] {
    let g = tall_geom();
    let pair = (0x001F_2328u32, 0x00FF_FFFFu32);
    let (row, start, keys) = (4u16, 2u16, 20u16);
    let (glow, out) = style_burst_on(
        GlowStyle::Comet,
        pair,
        (keys, 60),
        (None, None),
        true,
        &|glow| {
            glow.comet_light_tube = tube;
        },
    );
    let mut glass = Glass::new(g, pair.1);
    glass.add_quads(glow.under_quads());
    glass.add_quads(&out);
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let caret = i32::from(start + keys);
    [1, 4].map(|back| {
        let x = (caret - back) * cw + cw / 2;
        let rows: Vec<i32> = (i32::from(row) * ch..(i32::from(row) + 1) * ch)
            .filter(|&y| {
                let p = glass.at(y, x);
                [16u32, 8, 0]
                    .iter()
                    .any(|&sh| 255 - ((p >> sh) & 0xff) >= 16)
            })
            .collect();
        let spine = rows
            .iter()
            .copied()
            .min_by_key(|&y| {
                let p = glass.at(y, x);
                [16u32, 8, 0]
                    .iter()
                    .map(|&sh| (p >> sh) & 0xff)
                    .sum::<u32>()
            })
            .unwrap_or(i32::from(row) * ch + ch / 2);
        (rows.len(), crate::color_math::rgb2hsv(glass.at(spine, x)).0)
    })
}

/// looks10 L4, THE PAPER COMET IS A COMET, NOT BEAM'S TUBE: on paper Comet's tail laid an
/// even wedge in its colour alone, and beside Beam's tube it read as the same light with a
/// ring. Its tail is now a needle of ice: four cells behind the caret it lights under a third
/// of the rows it lights one cell behind, and its spine there reads its colour turned toward
/// ice ([`CursorGlow::comet_paper_ice`]), six degrees or more toward its cyan. Negative
/// control: laid as looks9 laid it, the tail four cells back is a third of its head's girth or
/// more, in the head's own hue.
#[test]
fn the_paper_comet_s_tail_is_a_needle_of_ice() {
    let [(head_rows, head_hue), (tail_rows, tail_hue)] = paper_comet_girth(false);
    let toward_ice = |h: f32| {
        let d = (h - CursorGlow::COMET_LIGHT_ICE_HUE).abs();
        d.min(360.0 - d)
    };
    assert!(tail_rows > 0, "fixture: the tail lights four cells back");
    assert!(
        tail_rows * 3 < head_rows,
        "four cells back the tail lights {tail_rows} rows, one back {head_rows}"
    );
    assert!(
        toward_ice(head_hue) - toward_ice(tail_hue) >= 6.0,
        "the tail's spine reads hue {tail_hue:.1}, its head's {head_hue:.1}"
    );
    let [(tube_head, tube_head_hue), (tube_tail, tube_tail_hue)] = paper_comet_girth(true);
    assert!(
        tube_tail * 3 >= tube_head
            && (toward_ice(tube_head_hue) - toward_ice(tube_tail_hue)).abs() < 3.0,
        "control: looks9's wedge lights {tube_tail} rows of {tube_head} four cells back, hue {tube_tail_hue:.1} of {tube_head_hue:.1}"
    );
}

/// looks9 round 2's Q8: A LASER STRIKE STAYS IN ITS PANE. On paper a jump's
/// strike threw a fork at the pane's bottom edge, cut there by the divider
/// to a lone violet fragment (the gallery's `split`, `b0`). Forged 400 times
/// along jumps on the grid's last rows, under a filled bar of a row above
/// that the jump does not cross, no fork and no channel of a strike reaches,
/// its corona's girth and all, past the grid's edge or onto the bar.
/// Negative control: laid as forged (`strike_forks_cut`), a tenth or more
/// of the strikes throw a fork past the edge.
#[test]
fn a_laser_strike_stays_in_its_pane() {
    let g = tall_geom();
    let ch = g.ch as f32;
    let half = ch * 0.15;
    let (pair, bar) = (SOLARIZED_PAIR, 0x0030_3030u32);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let (row, bar_row) = ((g.rows - 2) as u16, (g.rows - 4) as u16);
    let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for (r, line) in page.iter_mut().enumerate() {
        for cell in line.iter_mut() {
            let ground = if r == usize::from(bar_row) {
                bar
            } else {
                pair.1
            };
            (cell.fg, cell.bg) = (rgb(pair.0), rgb(ground));
        }
    }
    let (top, bot) = (
        f32::from(g.origin_y),
        f32::from(g.origin_y) + g.rows as f32 * ch,
    );
    let (left, right) = (
        f32::from(g.origin_x),
        f32::from(g.origin_x) + (g.cols * g.cw) as f32,
    );
    let (bar_top, bar_bot) = (
        top + f32::from(bar_row) * ch,
        top + f32::from(bar_row + 1) * ch,
    );
    let strikes = |cut: bool| {
        let mut glow = CursorGlow::default();
        glow.observe_frame_text(Some((row, 4)), &page, Instant::now());
        glow.strike_forks_cut = cut;
        let (mut out, mut forks) = (0usize, 0usize);
        for i in 0..400u32 {
            let (r0, r1) = (row + (i % 2) as u16, row + ((i / 2) % 2) as u16);
            let from = g.cell_center(r0, 2 + (i % 7) as u16);
            let to = g.cell_center(r1, 9 + (i % 29) as u16);
            let now = Instant::now();
            glow.bolts.clear();
            glow.rng = i.wrapping_mul(2_654_435_761) | 1;
            glow.spawn_bolt(from, to, ch, true, now);
            glow.keep_strike_in_grid(g, pair.1, (r0, r1), now);
            forks += glow.bolts.iter().filter(|b| b.branch).count();
            let past = glow.bolts.iter().any(|b| {
                b.pts.as_slice().windows(2).any(|w| {
                    (0..=8).any(|k| {
                        let f = k as f32 / 8.0;
                        let (x, y) = (
                            w[0].0 + (w[1].0 - w[0].0) * f,
                            w[0].1 + (w[1].1 - w[0].1) * f,
                        );
                        y + half > bot
                            || y - half < top
                            || x < left
                            || x > right
                            || (y + half > bar_top && y - half < bar_bot)
                    })
                })
            });
            out += usize::from(past);
        }
        (out, forks)
    };
    let (kept, forks) = strikes(false);
    let (cut, _) = strikes(true);
    assert_eq!(
        kept, 0,
        "{kept} of 400 strikes leave the pane or meet the bar"
    );
    assert!(forks >= 400, "fixture: the strikes keep {forks} forks");
    assert!(
        cut >= 40,
        "control: laid as forged, only {cut} of 400 strikes leave the pane"
    );
}

/// looks9 round 3's review: A LASER STRIKE AND CRACKLE STAY IN A SPLIT'S
/// PANE. In a split the caret's pane has its edge inside the window's grid,
/// where the GUI clips the glow to the pane; the keepers read the window's
/// edge alone, so a fork or crackle that crossed the divider was cut there to
/// a fragment. The window here is 16 rows by 48 columns and the caret's pane
/// its top-left 8 rows by 24 columns (the frame hands the pane's cells at
/// its offset, as the GUI's split does). Forged 400 times along jumps on the
/// pane's last rows, no strike reaches past the pane's right edge, or, its
/// corona's girth and all, past its bottom edge; swept round every angle at three lengths
/// from the pane's next-to-last row, no kept crackle does. Negative control:
/// the pane's edge unread (`bolt_window_edge`), a tenth or more of the
/// strikes and of the crackles aimed down cross it.
#[test]
fn a_laser_strike_and_crackle_stay_in_a_split_s_pane() {
    let pane_g = tall_geom();
    let g = Geom {
        rows: pane_g.rows * 2,
        win_h: pane_g.win_h * 2,
        ..pane_g
    };
    let ch = g.ch as f32;
    let half = ch * 0.15;
    let pair = SOLARIZED_PAIR;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let pane_cols = pane_g.cols / 2;
    let mut pane = vec![vec![aterm_core::terminal::RenderCell::default(); pane_cols]; pane_g.rows];
    for line in &mut pane {
        for cell in line.iter_mut() {
            (cell.fg, cell.bg) = (rgb(pair.0), rgb(pair.1));
        }
    }
    let (bot, right) = (
        f32::from(g.origin_y) + pane_g.rows as f32 * ch,
        f32::from(g.origin_x) + (pane_cols * g.cw) as f32,
    );
    let crosses = |b: &Bolt| {
        b.pts.as_slice().windows(2).any(|w| {
            (0..=8).any(|k| {
                let f = k as f32 / 8.0;
                let (x, y) = (
                    w[0].0 + (w[1].0 - w[0].0) * f,
                    w[0].1 + (w[1].1 - w[0].1) * f,
                );
                y + half > bot || x > right
            })
        })
    };
    let row = (pane_g.rows - 2) as u16;
    let strikes = |window_edge: bool| {
        let mut glow = CursorGlow::default();
        glow.observe_frame_text_at(Some((row, 4)), &pane, ((0, 0), pane_cols), Instant::now());
        glow.bolt_window_edge = window_edge;
        let mut out = 0usize;
        for i in 0..400u32 {
            let (r0, r1) = (row + (i % 2) as u16, row + ((i / 2) % 2) as u16);
            let from = g.cell_center(r0, 2 + (i % 7) as u16);
            let to = g.cell_center(r1, 9 + (i % 13) as u16);
            let now = Instant::now();
            glow.bolts.clear();
            glow.rng = i.wrapping_mul(2_654_435_761) | 1;
            glow.spawn_bolt(from, to, ch, true, now);
            glow.keep_strike_in_grid(g, pair.1, (r0, r1), now);
            out += usize::from(glow.bolts.iter().any(crosses));
        }
        out
    };
    let crackles = |window_edge: bool| {
        let mut glow = CursorGlow::default();
        glow.observe_frame_text_at(Some((row, 20)), &pane, ((0, 0), pane_cols), Instant::now());
        glow.bolt_window_edge = window_edge;
        let (x0, y0) = g.cell_center(row, 17);
        let (mut down, mut out) = (0usize, 0usize);
        for i in 0..72 {
            let (sin_a, cos_a) = (i as f32 * 5f32.to_radians()).sin_cos();
            for len in [1.6f32, 3.0, 4.4] {
                glow.bolts.clear();
                glow.spawn_bolt(
                    (x0, y0),
                    (x0 + cos_a * len * ch, y0 + sin_a * len * ch),
                    ch,
                    false,
                    Instant::now(),
                );
                glow.keep_crackle_off_bars(g, pair.1, row, y0);
                down += usize::from(sin_a > 0.2);
                out += usize::from(glow.bolts.last().is_some_and(crosses));
            }
        }
        (down, out)
    };
    let kept = strikes(false);
    assert_eq!(kept, 0, "{kept} of 400 strikes cross the pane's edge");
    let cut = strikes(true);
    assert!(
        cut >= 40,
        "control: the pane's edge unread, only {cut} of 400 strikes cross it"
    );
    let (down, kept) = crackles(false);
    assert!(down >= 60, "fixture: {down} crackles aimed down");
    assert_eq!(kept, 0, "{kept} kept crackles cross the pane's edge");
    let (_, cut) = crackles(true);
    assert!(
        cut * 10 >= down,
        "control: the pane's edge unread, only {cut} of {down} crackles aimed down cross it"
    );
}

/// A colour's CIELAB chroma C*ab (sRGB, D65 white), as the lab's `trail-hue`
/// judge reads it.
fn cielab_chroma(c: u32) -> f64 {
    let lin = |v: u32| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin((c >> 16) & 0xff), lin((c >> 8) & 0xff), lin(c & 0xff));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (a, bb) = (500.0 * (f(x) - f(y)), 200.0 * (f(y) - f(z)));
    a.hypot(bb)
}

/// The burst's first keys (`$ echo the quick`, a key per 50 ms) typed on
/// Solarized Dark's teal under a blank row, at seven phases of the flames'
/// flicker and ten frames a key: the pixels the fire moves off the bare page,
/// and those of them that read olive (hue 66..180° at a saturation of 0.2 and
/// a value of 0.12, as the lab's `trail-hue` names green, at a CIELAB chroma
/// of 15 or more: five under its visibility floor of 20, for the rounding of
/// the GPU's blend).
fn burst_olive_on_solarized(guarded: bool) -> (usize, usize) {
    use aterm_core::terminal::RenderCell;
    let g = tall_geom();
    let (row, start) = (4u16, 2u16);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let (mut lit_px, mut olive) = (0usize, 0usize);
    for phase_ms in [0u64, 7, 13, 21, 29, 37, 43] {
        let mut host = FireHost::styled(GlowStyle::Fire, SOLARIZED_PAIR, true, false);
        host.fx.glow.flame_grounds_guarded = guarded;
        host.text[0] = '$';
        host.typed = std::iter::repeat_n(' ', 2)
            .chain("echo the quick".chars())
            .collect();
        let mut rows = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(SOLARIZED_PAIR.0), rgb(SOLARIZED_PAIR.1));
            }
        }
        host.rows = Some(rows);
        let mut t = Instant::now() + Duration::from_millis(phase_ms);
        host.frame(t, (row, start), false);
        for k in 1..=14u16 {
            let caret = start + k;
            for sub in 0..10u64 {
                host.frame(t + Duration::from_millis(5 * sub), (row, caret), sub == 0);
                let mut lit = Glass::new(g, SOLARIZED_PAIR.1);
                lit.add_quads(host.fx.glow.under_quads());
                lit.add_fire_patches(host.fx.glow.patches());
                let mut bare = Glass::new(g, SOLARIZED_PAIR.1);
                for col in std::iter::once(0).chain(start..caret) {
                    for (y, x) in glyph(g, row, col).0 {
                        lit.px[y as usize * lit.w + x as usize] = SOLARIZED_PAIR.0;
                        bare.px[y as usize * bare.w + x as usize] = SOLARIZED_PAIR.0;
                    }
                }
                lit.add_quads(&host.fx.glow_scratch);
                lit.add_halos(host.fx.glow.halos());
                for (&p, &q) in lit.px.iter().zip(&bare.px) {
                    let (p, q) = (p & 0x00FF_FFFF, q & 0x00FF_FFFF);
                    if p == q {
                        continue;
                    }
                    lit_px += 1;
                    let (h, s, v) = crate::color_math::rgb2hsv(p);
                    if s >= 0.2
                        && v >= 0.12
                        && (66.0..180.0).contains(&h)
                        && cielab_chroma(p) >= 15.0
                    {
                        olive += 1;
                    }
                }
            }
            t += Duration::from_millis(50);
        }
    }
    (lit_px, olive)
}

/// FIRE IS NEVER GREEN, OVER A LETTER'S RING INCLUDED (looks10 round 2,
/// WIDEN): the flames' night grounds are laid after the letter guard
/// ([`CursorGlow::ground_the_frame`]), which cuts no flame, so the burst's
/// flames over `echo` on Solarized Dark's teal burn over their whole ground
/// and no pixel of them reads olive. Negative control: the grounds join the
/// under stream before the guard, which cuts them to a fifth over the
/// letters' ring, and the foot of a flame over the `o` reads olive on the
/// teal (`#575d3a` in the round's `burst` key3 frame).
#[test]
fn a_flame_s_ground_is_not_cut_by_the_letter_guard() {
    let (lit, olive) = burst_olive_on_solarized(false);
    assert!(lit > 10_000, "fixture: the fire lights the page ({lit} px)");
    assert_eq!(
        olive, 0,
        "{olive} of {lit} fire pixels read olive on Solarized Dark"
    );
    let (lit_cut, olive_cut) = burst_olive_on_solarized(true);
    assert!(
        olive_cut >= 40,
        "control: with its grounds cut by the guard only {olive_cut} of {lit_cut} fire pixels read olive"
    );
}

/// What a dark fire leaves on a ` aterm ` powerline bar beside the caret
/// ([`bar_beside_the_burst`]).
struct BarRead {
    /// The bar letters' worst reading: an ink pixel against a ground pixel of
    /// its row within two of it (the lab's `RING_REACH`).
    worst: f32,
    /// The same on the bare bar.
    bare: f32,
    /// Pixels of the bar the fire lit to a hue the lab's `trail-hue` names
    /// green, as it reads one: a channel more than 24 off the bare bar,
    /// 66..180° at a saturation of 0.2 and a value of 0.12, not within 12° of
    /// the cyan's own hue, at a CIELAB chroma of 15 or more (five under its
    /// visibility floor, for the rounding of the GPU's blend).
    green: usize,
    /// The same on the page past the bar.
    page_green: usize,
    /// Pixels of the row and the bar the fire lit (a channel more than 24
    /// off the bare one).
    lit: usize,
}

/// The burst's first keys (`echo the quick`, a key per 50 ms) typed on the
/// dark page `pair` after a ` aterm ` powerline bar (black on cyan) under a
/// filled powerline row, as the render suite's context scene lays them, at
/// seven phases of the flames' flicker and ten frames a key. `on_bar`: the
/// trail burns back onto the bar ([`CursorGlow::fire_burns_on_filled`]);
/// `grounds_unguarded`: the flames' grounds on the bar pass the letter guard
/// ([`CursorGlow::flame_bar_grounds_unguarded`]); `cores_ungrounded`: the
/// flames' gold cores lay no night ground
/// ([`CursorGlow::fire_cores_ungrounded`]).
fn bar_beside_the_burst(
    pair: (u32, u32),
    (on_bar, grounds_unguarded, cores_ungrounded): (bool, bool, bool),
) -> BarRead {
    use aterm_core::terminal::RenderCell;
    const BAR_INK: u32 = 0x0000_0000;
    const BAR_GROUND: u32 = 0x0000_CDCD;
    // The prompt's own powerline above, filled to the row's end: the
    // flames keep to the caret's row.
    const ABOVE_GROUND: u32 = 0x0030_3030;
    let g = tall_geom();
    let (row, start) = (4u16, 8u16);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let bar = 0..7u16;
    let letters = 1..6u16;
    let worst_ring = |glass: &Glass, col: u16| {
        let ink = glyph(g, row, col).0;
        let mut worst = f32::INFINITY;
        for &(y, x) in &ink {
            for dx in -2..=2 {
                if !ink.contains(&(y, x + dx)) {
                    worst = worst.min(ratio(glass, &[(y, x)], &[(y, x + dx)]));
                }
            }
        }
        worst
    };
    let (x_bar, y_row) = (
        i32::from(g.origin_x) + i32::from(bar.end) * g.cw as i32,
        i32::from(g.origin_y) + i32::from(row) * g.ch as i32,
    );
    let mut read = BarRead {
        worst: f32::INFINITY,
        bare: f32::INFINITY,
        green: 0,
        page_green: 0,
        lit: 0,
    };
    for phase_ms in [0u64, 7, 13, 21, 29, 37, 43] {
        let mut host = FireHost::styled(GlowStyle::Fire, pair, true, false);
        host.fx.glow.fire_burns_on_filled = on_bar;
        host.fx.glow.flame_bar_grounds_unguarded = grounds_unguarded;
        host.fx.glow.fire_cores_ungrounded = cores_ungrounded;
        for (col, ch) in (1usize..).zip("aterm".chars()) {
            host.text[col] = ch;
        }
        host.typed = std::iter::repeat_n(' ', usize::from(start))
            .chain("echo the quick".chars())
            .collect();
        let mut rows = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(pair.0), rgb(pair.1));
            }
        }
        for col in bar.clone() {
            let cell = &mut rows[usize::from(row)][usize::from(col)];
            (cell.fg, cell.bg) = (rgb(BAR_INK), rgb(BAR_GROUND));
        }
        for cell in &mut rows[usize::from(row) - 1] {
            (cell.fg, cell.bg) = (rgb(0x00FF_FFFF), rgb(ABOVE_GROUND));
        }
        host.rows = Some(rows);
        let mut t = Instant::now() + Duration::from_millis(phase_ms);
        // The prompt lands: the caret leaves the row's head for the cell
        // after the bar and rests there, then the keys come.
        host.frame(t, (row, 0), false);
        for rest in 1..=50u64 {
            host.frame(t + Duration::from_millis(16 * rest), (row, start), false);
        }
        t += Duration::from_millis(800);
        for k in 1..=14u16 {
            let caret = start + k;
            for sub in 0..10u64 {
                host.frame(t + Duration::from_millis(5 * sub), (row, caret), sub == 0);
                let mut lit = Glass::new(g, pair.1);
                let mut bare = Glass::new(g, pair.1);
                for glass in [&mut lit, &mut bare] {
                    for y in y_row..y_row + g.ch as i32 {
                        for x in i32::from(g.origin_x)..x_bar {
                            glass.px[y as usize * glass.w + x as usize] = BAR_GROUND;
                        }
                    }
                }
                lit.add_quads(host.fx.glow.under_quads());
                lit.add_fire_patches(host.fx.glow.patches());
                for col in letters.clone() {
                    for (y, x) in glyph(g, row, col).0 {
                        lit.px[y as usize * lit.w + x as usize] = BAR_INK;
                        bare.px[y as usize * bare.w + x as usize] = BAR_INK;
                    }
                }
                lit.add_quads(&host.fx.glow_scratch);
                lit.add_halos(host.fx.glow.halos());
                for col in letters.clone() {
                    read.worst = read.worst.min(worst_ring(&lit, col));
                    read.bare = read.bare.min(worst_ring(&bare, col));
                }
                for y in y_row..y_row + g.ch as i32 {
                    for x in i32::from(g.origin_x)..i32::from(g.win_w) {
                        let (p, q) = (lit.at(y, x) & 0x00FF_FFFF, bare.at(y, x) & 0x00FF_FFFF);
                        if (0..3)
                            .all(|i| ((p >> (8 * i)) & 0xff).abs_diff((q >> (8 * i)) & 0xff) <= 24)
                        {
                            continue;
                        }
                        read.lit += 1;
                        let (h, s, v) = crate::color_math::rgb2hsv(p);
                        let (hq, sq, vq) = crate::color_math::rgb2hsv(q);
                        let own_hue = sq >= 0.2 && vq >= 0.12 && {
                            let d = (h - hq).abs() % 360.0;
                            d.min(360.0 - d) <= 12.0
                        };
                        if s >= 0.2
                            && v >= 0.12
                            && (66.0..180.0).contains(&h)
                            && cielab_chroma(p) >= 15.0
                            && !own_hue
                        {
                            if x < x_bar {
                                read.green += 1;
                            } else {
                                read.page_green += 1;
                            }
                        }
                    }
                }
            }
            t += Duration::from_millis(50);
        }
    }
    read
}

/// FIRE IS NEVER GREEN, BESIDE A BAR (looks10 round 2): a dark flame's trail
/// burns back over its head's own ground and stops at another
/// ([`CursorGlow::flame_cell_off_ground`]), so the burst's first keys after a
/// ` aterm ` bar on dark and on Solarized Dark leave no flame on the bar's
/// cyan, which its additive light would turn mint, and every bar letter
/// keeps its bare reading. Negative control: the trail burns back onto the
/// bar, and its flame over the cyan reads green (1736 px on dark, 7364 on
/// Solarized Dark; `#4afed3` and `#a5fda8` in the render suite's context
/// scene at its first key).
#[test]
fn a_dark_flame_s_trail_keeps_off_a_bar_beside_it() {
    for (name, pair) in [("dark", DARK_PAIR), ("solarized", SOLARIZED_PAIR)] {
        let kept = bar_beside_the_burst(pair, (false, false, false));
        assert!(
            kept.lit > 5_000,
            "fixture: {name}: the fire lights the row ({} px)",
            kept.lit
        );
        assert_eq!(
            kept.green, 0,
            "{name}: {} of {} fire pixels read green on the bar",
            kept.green, kept.lit
        );
        assert!(
            kept.worst >= 4.5,
            "{name}: a bar letter reads {:.2} (bare {:.2})",
            kept.worst,
            kept.bare
        );
        let on_bar = bar_beside_the_burst(pair, (true, false, false));
        assert!(
            on_bar.green >= 40,
            "control: {name}: burning onto the bar, only {} of {} fire pixels read green there",
            on_bar.green,
            on_bar.lit
        );
    }
}

/// A FLAME'S GROUND ON A BAR IS THE LETTER GUARD'S (looks10 round 2): a
/// flame's night ground keeps the page's lightness, so it passes the letter
/// guard on the page ([`a_flame_s_ground_is_not_cut_by_the_letter_guard`]);
/// on a filled cell it would darken the bar round the letters, and there it
/// joins the under stream for the guard to cut. The trail held onto a
/// ` aterm ` bar on Solarized Dark (`fire_burns_on_filled`, as a flame lies
/// on a filled cell a caret typed on), every bar letter keeps the bar
/// (6.14:1 at worst, 10.61 bare). Negative control: the ground on the bar
/// passes the guard too, and the cyan round the `m` under the burst's first
/// flame darkens to 1.67:1 (the render suite's context scene read 2.75:1
/// there).
#[test]
fn a_flame_s_ground_on_a_bar_is_cut_over_its_letters() {
    let kept = bar_beside_the_burst(SOLARIZED_PAIR, (true, false, false));
    assert!(
        kept.bare >= 7.0,
        "fixture: the bare bar's letters read {:.2}",
        kept.bare
    );
    assert!(
        kept.worst >= 4.5,
        "a bar letter under the burst reads {:.2} (bare {:.2})",
        kept.worst,
        kept.bare
    );
    let cut = bar_beside_the_burst(SOLARIZED_PAIR, (true, true, false));
    assert!(
        cut.worst < 4.5,
        "control: with the bar's grounds past the guard its worst letter still reads {:.2}",
        cut.worst
    );
}

/// FIRE IS NEVER GREEN, A CORE INCLUDED (looks10 round 2): a dark flame's
/// gold core lays its night ground as the flame's slices do, so at the
/// burst's first keys after a ` aterm ` bar on Solarized Dark no pixel of the
/// fire on the page reads green. Negative control: the cores lay no ground,
/// and where the slice under a core is held to nothing (the tongue's foot
/// beside the first letter) its dim gold lies on the bare teal and reads
/// olive: 1337 pixels (`#5b7e49`, hue 100, C* 34, in the render suite's
/// context scene).
#[test]
fn a_dark_flame_s_core_lays_its_night_ground() {
    let kept = bar_beside_the_burst(SOLARIZED_PAIR, (false, false, false));
    assert!(
        kept.lit > 5_000,
        "fixture: the fire lights the row ({} px)",
        kept.lit
    );
    assert_eq!(
        kept.page_green, 0,
        "{} of {} fire pixels read green on the page",
        kept.page_green, kept.lit
    );
    let bare = bar_beside_the_burst(SOLARIZED_PAIR, (false, false, true));
    assert!(
        bare.page_green >= 40,
        "control: with ungrounded cores only {} of {} fire pixels read green on the page",
        bare.page_green,
        bare.lit
    );
}

/// CIELAB chroma (C*ab, D65) of `0x00RRGGBB`: how colourful it reads, the
/// trail gallery's `trail-hue` visibility measure.
fn lab_chroma(p: u32) -> f32 {
    let lin = |v: u32| {
        let c = v as f32 / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin((p >> 16) & 0xff), lin((p >> 8) & 0xff), lin(p & 0xff));
    let x = (0.412_4 * r + 0.357_6 * g + 0.180_5 * b) / 0.950_47;
    let y = 0.212_6 * r + 0.715_2 * g + 0.072_2 * b;
    let z = (0.019_3 * r + 0.119_2 * g + 0.950_5 * b) / 1.088_83;
    let f = |t: f32| {
        if t > 0.008_856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    (500.0 * (fx - fy)).hypot(200.0 * (fy - fz))
}

/// Sparkle typing the trail gallery's `echo the quick brown fox` on Solarized
/// Dark on its grid (14 x 26 px cells), then left to fade, a frame every 5 ms
/// to 600 ms after the last key: `(frames lit, lit px, off-gold px, frames
/// with any)`. A pixel is off gold as `trail-hue` reads it (a channel off the
/// bare page's by more than 24, HSV saturation 0.2 and value 0.12 or more, a
/// hue outside 20..66 degrees, not within 12 degrees of the page's own hue,
/// outside the caret's cell), at a chroma floor of
/// 10 (`trail-hue`'s is 20: the CPU glass composes the fade a little greyer
/// than the GPU, and the gallery's rust read 19 to 21 there). With `held`
/// off, the page-matched rail is laid at the least opacity whatever its hue.
fn sparkle_rail_fade_on_solarized(held: bool) -> (usize, usize, usize, usize) {
    let g = Geom {
        cw: 14,
        ch: 26,
        rows: 8,
        cols: 48,
        origin_x: 0,
        origin_y: 0,
        win_w: 672,
        win_h: 208,
        head: 0,
    };
    let (row, start) = (3u16, 2u16);
    let mut host = FireHost::styled(GlowStyle::Sparkle, SOLARIZED_PAIR, true, false);
    host.geom = g;
    host.text = vec![' '; g.cols];
    host.cfg.color = crate::cursor_glow::style::SPARKLE_DEFAULT_COLOR;
    host.fx.glow.rail_hue_free = !held;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut page = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for cell in page.iter_mut().flatten() {
        (cell.fg, cell.bg) = (rgb(SOLARIZED_PAIR.0), rgb(SOLARIZED_PAIR.1));
    }
    host.rows = Some(page);
    let word = "echo the quick brown fox";
    let mut typed = vec![' '; g.cols];
    for (k, c) in word.chars().enumerate() {
        typed[usize::from(start) + k] = c;
    }
    host.typed = typed.clone();
    let mut t = Instant::now();
    host.frame(t, (row, start), false);
    let keys = word.len() as u16;
    for k in 1..=keys {
        t += Duration::from_millis(60);
        host.frame(t, (row, start + k), true);
    }
    let caret = start + keys;
    let ink: Vec<(u16, u16)> = (start..caret)
        .filter(|&c| typed[usize::from(c)] != ' ')
        .map(|c| (row, c))
        .collect();
    let (cx, cy) = (
        usize::from(caret) * g.cw..usize::from(caret + 1) * g.cw,
        usize::from(row) * g.ch..usize::from(row + 1) * g.ch,
    );
    let (mut frames, mut lit_px, mut off_px, mut off_frames) = (0, 0, 0, 0);
    for dt in (0u64..=600).step_by(5) {
        host.frame(t + Duration::from_millis(dt), (row, caret), false);
        let mut lit = Glass::new(g, SOLARIZED_PAIR.1);
        lit.add_quads(host.fx.glow.under_quads());
        let mut bare = Glass::new(g, SOLARIZED_PAIR.1);
        for &(r, c) in &ink {
            for (y, x) in glyph(g, r, c).0 {
                lit.px[y as usize * lit.w + x as usize] = SOLARIZED_PAIR.0;
                bare.px[y as usize * bare.w + x as usize] = SOLARIZED_PAIR.0;
            }
        }
        lit.add_quads(&host.fx.glow_scratch);
        lit.add_halos(host.fx.glow.halos());
        let (mut n, mut off) = (0, 0);
        for (i, (&p, &q)) in lit.px.iter().zip(&bare.px).enumerate() {
            let (x, y) = (i % lit.w, i / lit.w);
            if cx.contains(&x) && cy.contains(&y) {
                continue;
            }
            let (p, q) = (p & 0x00FF_FFFF, q & 0x00FF_FFFF);
            let ch = |c: u32, sh: u32| ((c >> sh) & 0xff) as i32;
            if ![16, 8, 0]
                .iter()
                .any(|&sh| (ch(p, sh) - ch(q, sh)).abs() > 24)
            {
                continue;
            }
            n += 1;
            let (h, s, v) = crate::color_math::rgb2hsv(p);
            let (hq, sq, vq) = crate::color_math::rgb2hsv(q);
            let gap = (h - hq).abs().min(360.0 - (h - hq).abs());
            let own = sq >= 0.2 && vq >= 0.12 && gap <= 12.0;
            if s >= 0.2 && v >= 0.12 && !own && !(20.0..66.0).contains(&h) && lab_chroma(p) >= 10.0
            {
                off += 1;
            }
        }
        frames += usize::from(n > 0);
        lit_px += n;
        off_px += off;
        off_frames += usize::from(off > 0);
    }
    (frames, lit_px, off_px, off_frames)
}

/// SPARKLE'S RAIL FADES BY ITS LIGHT, IN GOLD, TO ITS LAST FRAME ON A TINTED
/// PAGE (looks10 round 3's hue ruling): every 5 ms of the fade after a typed
/// line on Solarized Dark, no pixel the trail lights reads a hue outside the
/// gold family. Negative control: each of the page-matched rail's lights laid
/// at the least opacity that reaches its reading over the page
/// ([`crate::cursor_glow::raster::page_matched`]) carried the teal's missing
/// red, and over the bed under the rail its last frames read rust (the
/// gallery's `#774f42`, hue 14, under the caret 197 ms after the last key).
#[test]
fn sparkles_rail_fades_in_gold_to_its_last_frame_on_a_tinted_page() {
    let (frames, lit, off, off_frames) = sparkle_rail_fade_on_solarized(true);
    assert!(
        frames > 100 && lit > 50_000,
        "fixture: the fade lights {frames} frames, {lit} px"
    );
    assert_eq!(
        (off, off_frames),
        (0, 0),
        "Sparkle's fade on Solarized Dark lit {off} px off gold in {off_frames} of {frames} frames"
    );
    let (_, bare_lit, bare_off, bare_frames) = sparkle_rail_fade_on_solarized(false);
    assert!(
        bare_off >= 40 && bare_frames >= 3,
        "control: at the least opacity only {bare_off} of {bare_lit} px in {bare_frames} frames read off gold"
    );
}
