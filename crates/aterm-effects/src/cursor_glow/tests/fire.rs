// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fire style: meteors, the EMBERFORGE thermal laws, the forge field.

use super::*;

/// FLARE LAW: a real cursor jump slams Fire's flare to full (the white-hot
/// payoff), and the flare cools on its own tau — visibly gone within about a
/// second — so the burst reads as an event, not a new steady state.
#[test]
fn fire_jump_flares_and_cools() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out); // seed
    glow.note_synthetic_move(t0);
    glow.tick(Some((3, 30)), t0, &c, g, &mut out); // a real jump
    // ARRIVAL-TIME FLARE (P2): launch flies a meteor, the eruption fires
    // when its head STRIKES the landing — not at launch.
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
    // The flare cools within ~a second of the strike (the smolder returns).
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

/// THE AUDITED NEWLINE BEHAVIOR, PINNED (P2): a move down one row anchors NO
/// cell-anchored fire (sparks) beyond the single wake spark at the landing, and the
/// landing row stays completely quiet RIGHT of the landing column (the old Bresenham
/// sweep parked a fire bar on the new row's columns 1..8 — cells the cursor never
/// visited). A TYPING WRAP (from the last column) is continued typing, so it flies
/// NO inter-line meteor: a meteor there sweeps the whole line on every wrapped line
/// during fast typing, painting the prompt and other untyped cells. A DELIBERATE
/// Enter/jump from a short column still flies its meteor (the positive control below).
#[test]
fn typing_wrap_no_meteor_deliberate_enter_still_meteors() {
    let g = geom(); // rows:6 cols:40 → the last column is 39
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // A TYPING WRAP: last column → next row's start. No meteor, no smear.
    let mut wrapper = CursorGlow::default();
    wrapper.tick(Some((4, 39)), t0, &c, g, &mut out); // typing at the last column
    wrapper.note_synthetic_typed(t0, 1);
    wrapper.tick(Some((5, 0)), t0, &c, g, &mut out); // WRAP to the next row
    assert!(
        wrapper.fire_meteors.is_empty(),
        "a typing wrap flies no meteor (got {})",
        wrapper.fire_meteors.len()
    );
    // The only wake is a single spark at the landing (5,0) — nothing right of it.
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
    // A second later the landing row right of the cursor is totally dark.
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

    // POSITIVE CONTROL for the morphology: an explicitly scripted jump
    // from a short column still flies its inter-line meteor. A raw Enter
    // timestamp is deliberately not movement provenance.
    let mut enterer = CursorGlow::default();
    enterer.tick(Some((2, 5)), t0, &c, g, &mut out); // cursor mid-line
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

/// ONE-SHOT CAUSALITY: a scripted jump may fly one meteor, and an
/// UNLICENSED second repaint hop neither retargets it nor mints a second
/// one — but it does not KILL it either. The meteor is a pixel-space
/// streak the first, licensed move earned along its own vector; it does
/// not follow the caret, so a program hop 20 ms later has nothing to
/// strand and no standing to destroy it (`move_licensed`, and
/// `docs/design/EFFECTS-LICENSE-REDESIGN.md`: earned light is never
/// destroyed by someone else's output).
#[test]
fn fire_meteor_survives_an_unlicensed_second_hop_without_retargeting() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((6, 11)), t0, &c, g, &mut out); // end of wrapped input
    // Hop 1 is the sole explicitly scripted move.
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
    // Hop 2 is UNLICENSED — the scripted gesture's stamp was consumed by
    // hop 1, so nobody's fingers asked for this one.
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

/// SUSPENSE ARC: the same single-cell advance emits far more fire light
/// mid-blaze than from a cold keyboard — the smolder→blaze build-up (taller
/// tongues, brighter head, an ember column instead of a lone mote).
#[test]
fn fire_smolders_cold_and_blazes_hot() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // Cold: two advances 500 ms apart — no heat accumulates.
    let mut cold = CursorGlow::default();
    cold.tick(Some((2, 0)), t0, &c, g, &mut out);
    cold.note_synthetic_typed(t0 + Duration::from_millis(500), 1);
    cold.tick(
        Some((2, 1)),
        t0 + Duration::from_millis(500),
        &c,
        g,
        &mut out,
    );
    let cold_lum = lum(&out);
    assert!(cold_lum > 0, "a cold keystroke still smolders");

    // Hot: sustained 40 ms cadence builds full blaze first.
    let mut hot = CursorGlow::default();
    let mut t = t0;
    hot.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=14u16 {
        t += Duration::from_millis(40);
        hot.note_synthetic_typed(t, 1);
        hot.tick(Some((2, i)), t, &c, g, &mut out);
    }
    let hot_lum = lum(&out);
    assert_invariants(&out, g);
    assert!(
        hot_lum > cold_lum * 2,
        "blazing fire far outshines the smolder (hot {hot_lum} vs cold {cold_lum})"
    );
}

/// WINDOW-SPACE EFFECTS LAYER: with a real head band (origin_y = pad 8 +
/// head 40 = 48) a row-0 blaze climbs ABOVE the grid into the chrome band —
/// patches with `y < origin_y` exist, tagged with damage-hint row 0 — while
/// the flame keeps its NATURAL cell-top root (`base_y == origin_y + 2`; the
/// origin_y==0 `.max(ch)` clamp is a no-op here) and the engulf gate keeps
/// every charred key a real grid row (no u16 wrap from a negative band).
#[test]
fn fire_rises_into_the_chrome_band() {
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 8,
        origin_y: 48, // pad 8 + a 40px head band
        win_w: 336,   // 40·8 + 2·8
        win_h: 152,   // 6·16 + 2·8 + 40
        head: 40,     // the chrome band alone (origin_y minus pad)
    };
    let c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    // Row-0 blaze at full heat (the smolder-test hot loop, on the top row).
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
    for q in glow.patches() {
        assert!((q.row as usize) < g.rows, "row tag in grid: {q:?}");
        assert_eq!(
            q.base_y,
            g.origin_y + 2,
            "natural cell-top root under a real band: {q:?}"
        );
    }
    assert!(
        glow.charred().iter().all(|cf| (cf.row as usize) < g.rows),
        "the engulf gate never keys a wrapped/above-grid charred row"
    );
    // The engulf gate now materializes through the CONTRAST-HALO stream
    // (charred stays empty under the no-recolor law) — same grid-row law,
    // non-vacuously.
    assert!(
        !glow.halo_cells().is_empty(),
        "a row-0 blaze emits contrast-halo cells"
    );
    assert!(
        glow.halo_cells().iter().all(|c| (c.row as usize) < g.rows),
        "the engulf gate never keys a wrapped/above-grid halo row"
    );
}

/// THE IDENTITY LAW at a REAL host origin (adversarial review): with a
/// nonzero origin but `head == 0` — the GUI's layout when no chrome band
/// exists (Linux, `ATERM_NO_FULLSIZE_CONTENT`, fullscreen) — the fire must
/// reproduce the historical pad-relative behavior EXACTLY, translated by
/// origin: the row-0 root stays forced one cell down (`origin_y + ch`), and
/// no patch pixel ever rises above the grid top (`y >= origin_y`). The
/// window-clamped clamps this pins against rooted 12px high and painted the
/// pad strip.
#[test]
fn fire_identity_at_origin_without_head() {
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 12,
        origin_y: 12, // pad only — no chrome band
        win_w: 344,   // 40·8 + 2·12
        win_h: 120,   // 6·16 + 2·12
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

// ---- EMBERFORGE thermal laws (P1) -------------------------------------

/// The bounded thermal-tier machine (COLD/WARM/HOT/SMOKING as tiers
/// 0..=3): stoking climbs one tier at a time, quenching and cooling only
/// descend. `Buggy=1` plants the audited defect shape — a quench that
/// HEATS — which the invariant must catch (prove-and-catch non-vacuity).
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

/// Tier-1 conformance: drive the REAL fire integrators frame by frame
/// (16 ms ticks — the animation cadence) through a stoke burst and a
/// backspace quench run, project `disp_t` onto the model's tier, and check
/// every observed tier transition against the derived machine. Includes
/// the negative control: no single frame may ever climb two tiers.
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

    // Stoke: 60 forward keys at 40 ms with a tick per frame (16 ms grid).
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
    // Quench: 8 causally proven one-cell deletes at 60 ms.
    for _ in 0..8 {
        t += Duration::from_millis(60);
        arm_exact_backspace(&mut glow, t, (row, col), (row, col - 1));
        col -= 1;
        glow.tick(Some((row, col)), t, &c, g, &mut out);
        check(&glow, &mut state);
    }
    // Idle: frame ticks with no moves until fully cold (the coal bed's
    // 6 s τ needs ~30 s to fall under the snap threshold).
    for _ in 0..2000 {
        t += Duration::from_millis(16);
        glow.tick(Some((row, col)), t, &c, g, &mut out);
        check(&glow, &mut state);
    }
    assert_eq!(prev_tier.get(), 0, "idle returns to COLD");
    assert_eq!(glow.disp_t, 0.0, "display temperature snaps to exactly 0");
}

/// MOMENTUM IS EARNED: a short fast burst stays well under full blaze
/// (today's 7-keys-to-inferno is the audited bug), a sustained run climbs
/// high, and a lone keystroke leaves the engine essentially cold.
#[test]
fn fire_momentum_is_earned_not_instant() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    let mut run = |keys: usize, gap_ms: u64| -> f32 {
        let mut glow = CursorGlow::default();
        let mut t = t0;
        glow.tick(Some((2, 0)), t, &c, g, &mut out);
        for i in 1..=keys {
            t += Duration::from_millis(gap_ms);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((2, i as u16)), t, &c, g, &mut out);
        }
        glow.disp_t
    };
    let lone = run(1, 40);
    let burst6 = run(6, 40);
    let burst40 = run(40, 40);
    assert!(lone < 0.05, "a lone key stays cold ({lone})");
    assert!(
        burst6 < 0.5,
        "six fast keys must NOT reach full blaze ({burst6})"
    );
    assert!(
        burst40 > burst6 + 0.2,
        "the ramp keeps climbing over a sustained run ({burst6} -> {burst40})"
    );
    assert!(
        burst40 > 0.6,
        "a sustained run earns a real blaze ({burst40})"
    );
}

/// HUMAN CADENCE KEEPS A LIVE EMBER FLOOR: relaxed 320 ms/key writing must
/// hold a visible standing temperature (the coal bed) — the audit found
/// today's fire perceptually absent at exactly this cadence.
#[test]
fn fire_human_cadence_keeps_a_live_ember_floor() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=30u16 {
        t += Duration::from_millis(320);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    assert!(
        glow.disp_t > 0.12,
        "30 human-cadence keys keep a standing ember floor ({})",
        glow.disp_t
    );
    // Mid-gap the wake is still alight (the chained ember line).
    let fp = glow.tick(
        Some((2, 30)),
        t + Duration::from_millis(200),
        &c,
        g,
        &mut out,
    );
    assert_ne!(fp, 0, "the ember line is alive between keystrokes");
    assert!(!out.is_empty());
}

/// BACKSPACE QUENCHES, NEVER STOKES: a delete run monotonically cools the
/// display temperature, the quench meter escalates with consecutive
/// deletes, and arrow-left navigation (no key-hint) earns no heat at all.
#[test]
fn fire_backspace_quenches_and_navigation_never_heats() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // Heat up with a sustained forward burst.
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

    // Delete run: exact one-cell evidence + leftward echo, 60 ms cadence.
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

    // Arrow-left navigation (no hint): heat stays untouched at zero.
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

/// THE FORGE CURSOR: metal heats under sustained blaze, holds its heat
/// longer than the flames (hysteresis), yields a black-body fill (red
/// dominant), and cools back to the DULL REST EMBER + inactive (idle-zero law;
/// the fire cursor is never the theme green — owner v0.31).
#[test]
fn fire_forge_cursor_heats_holds_and_cools_to_ember() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    // The fire cursor is a DULL EMBER at rest (never the theme green) —
    // warm-ordered (R ≥ G ≥ B) and dim (low red).
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
    assert!(
        cr < 0xC0,
        "cold metal is a DULL ember, not hot: {cold_fill:#08x}"
    );
    // ~3 s of fast sustained typing on a 16 ms frame grid.
    let mut col = 0u16;
    for i in 1..=180 {
        t += Duration::from_millis(16);
        if i % 3 == 0 {
            col += 1; // a keystroke every ~48 ms
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
    // After stopping, the metal first SOAKS residual heat (temp lags the
    // display temperature), then cools — by 2.5 s cooling dominates, yet
    // real heat remains (far slower than the flames' sub-second decay).
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
    // Long idle: the metal cools to exactly 0 and the fill returns to the
    // dull rest ember (== the cold fill above); the timer disarms and the
    // idle fingerprint returns to 0 (the constant rest ember is NOT folded).
    let fp = glow.tick(Some((2, col)), t + Duration::from_secs(40), &c, g, &mut out);
    assert_eq!(glow.cursor_temp, 0.0, "metal cools to exactly 0");
    assert_eq!(
        glow.forge_fill(),
        Some(cold_fill),
        "the fill cools back to the dull rest ember"
    );
    assert_eq!(
        fp, 0,
        "idle fingerprint is 0 (constant rest ember not folded)"
    );
    assert!(!glow.is_active(), "the animation timer disarms");
}

/// P4 DETERMINISM LAW: the flame field is a pure function of injected
/// clocks — two engines driven with identical synthetic instants emit
/// identical quads (same fp), frame after frame.
#[test]
fn fire_field_is_deterministic() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let drive = || {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut fps = Vec::new();
        let mut t = t0;
        glow.tick(Some((2, 0)), t, &c, g, &mut out);
        for i in 1..=25u16 {
            t += Duration::from_millis(40);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((2, i)), t, &c, g, &mut out);
            fps.push(glow.tick(Some((2, i)), t, &c, g, &mut out));
        }
        fps
    };
    assert_eq!(drive(), drive(), "same clocks ⇒ same fire, always");
}

/// P4 BUDGET LAW: full blaze + a huge landing burst on 4K-class cell
/// geometry stays under MAX_QUADS (both backends see identical truncation).
#[test]
fn fire_field_full_blaze_respects_quad_budget() {
    let g = Geom {
        cw: 22,
        ch: 44,
        rows: 60,
        cols: 200,
        origin_x: 0,
        origin_y: 0,
        win_w: 4400,
        win_h: 2640,
        head: 0,
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
    glow.tick(Some((30, 10)), t, &c, g, &mut out); // monster jump mid-blaze
    t += Duration::from_millis(120);
    glow.tick(Some((30, 10)), t, &c, g, &mut out); // strike frame
    assert!(out.len() <= CursorGlow::MAX_QUADS);
    assert_invariants(&out, g);
}

/// The BUDGET LAW again, on a WINDOW-SPACE layout (a real head band +
/// padding): full blaze + the monster jump still stays under MAX_QUADS
/// with `origin > 0` and window extents wider than the grid.
#[test]
fn fire_field_full_blaze_respects_quad_budget_with_head_band() {
    let g = Geom {
        cw: 22,
        ch: 44,
        rows: 60,
        cols: 200,
        origin_x: 8,
        origin_y: 56, // pad 8 + a 48px chrome band
        win_w: 4416,  // 200·22 + 2·8
        win_h: 2704,  // 60·44 + 2·8 + 48
        head: 48,     // the chrome band alone
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
    glow.tick(Some((30, 10)), t, &c, g, &mut out); // monster jump mid-blaze
    t += Duration::from_millis(120);
    glow.tick(Some((30, 10)), t, &c, g, &mut out); // strike frame
    assert!(out.len() <= CursorGlow::MAX_QUADS);
    // Window-space invariants: every quad inside the window, single-band.
    for q in &out {
        assert!((q.row as usize) < g.rows, "row tag in grid: {q:?}");
        assert!(
            (q.x + q.w) as u32 <= g.win_w as u32 && (q.y + q.h) as u32 <= g.win_h as u32,
            "quad inside the window: {q:?}"
        );
    }
}

/// P3 ROUND LIGHT: fire's embers, crown, and impact flash are RADIAL
/// halos — round by construction — and the halo stream obeys the same
/// invariants as the quads (per-row bands, grid-interior, radii ≥ 1,
/// decays to empty, folded into the fingerprint).
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
    // A jump's strike flashes a halo at the landing (the ring conversion).
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
    // Idle: the halo stream decays to exactly empty with fp 0.
    t += Duration::from_secs(40);
    let fp = glow.tick(Some((5, 30)), t, &c, g, &mut out);
    assert!(glow.halos().is_empty(), "halos decay to empty");
    assert_eq!(fp, 0);
}

/// P3: the halo stream is folded into the tick fingerprint — an ember-only
/// change (same quads) must still change the fp so the frame presents.
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
/// record coverage, and the scheduler's exact idle sentinel. Shared render
/// record schemas are exhaustively pinned beside their canonical encoders
/// in `aterm-core`.
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

    // This one-record stream is the exact FNV-1a preimage of zero after
    // the CharFg tag and length. A live stream must still never borrow the
    // scheduler's reserved `0 == idle` value.
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

/// P5 STEAM LAW: deleting douses with steam that ESCALATES over a run —
/// more consecutive backspaces, more (and stronger) pale vapor — and the
/// vapor decays to exactly empty.
#[test]
fn backspace_steam_escalates_and_decays() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=20u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    // One delete: a subtle hiss.
    t += Duration::from_millis(60);
    arm_exact_backspace(&mut glow, t, (2, 20), (2, 19));
    glow.tick(Some((2, 19)), t, &c, g, &mut out);
    let after_one = glow.vapor.len();
    assert!(after_one >= 1, "a delete flashes steam");
    // A run of deletes: visibly more vapor in flight.
    let mut col = 19u16;
    for _ in 0..6 {
        t += Duration::from_millis(60);
        arm_exact_backspace(&mut glow, t, (2, col), (2, col - 1));
        col -= 1;
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    assert!(
        glow.vapor.len() > after_one,
        "a backspace run steams harder ({} -> {})",
        after_one,
        glow.vapor.len()
    );
    // Steam is round light: it rides the halo stream.
    assert!(!glow.halos().is_empty());
    // And it is short-lived: two seconds later the vapor is gone.
    let fp = glow.tick(Some((2, col)), t + Duration::from_secs(2), &c, g, &mut out);
    assert!(
        glow.vapor.iter().all(|v| v.kind != VaporKind::Steam),
        "steam fully dispersed"
    );
    let _ = fp;
}

/// P5 SMOKE LAW: a cursor forged hot SMOKES — wisps shed continuously
/// while the metal is above the smoking point — and the smoke ceases
/// (vapor drains, timer disarms) once it cools.
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
    // ~5 s of fast sustained typing on a 16 ms frame grid forges the metal
    // well past the smoking point (attack τ 1.5 s).
    for i in 1..=300 {
        t += Duration::from_millis(16);
        if i % 3 == 0 && col < 38 {
            col += 1;
            glow.note_synthetic_typed(t, 1);
        } else if i % 60 == 0 {
            col = 2; // wrap back to keep moves flowing
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
    // Long idle: the metal cools, the smoke drains, everything disarms.
    for _ in 0..40 {
        t += Duration::from_secs(1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    assert!(glow.vapor.is_empty(), "smoke ceases once the metal cools");
    assert!(!glow.is_active(), "idle returns to zero work");
}

/// P6 DARK-CORE LAW (campaign 2): at a towering blaze the flame BODY is
/// emitted as per-pixel [`FirePatch`]es (drawn at the under-ink seam), the
/// cells it saturates get charred ink darker than warm char (the
/// silhouette source), and everything decays back to exactly empty.
#[test]
fn towering_blaze_chars_the_engulfed_rows_and_decays() {
    // Renamed contract, kept name (git archaeology): a towering blaze must
    // NOT recolor any ink at all — the owner vetoed charring twice on the
    // live terminal (v0.41 near-black char read as invisible letters;
    // v0.42's medium char read as "characters getting lagged"). Plain ink
    // under the amber-capped flame is the law; the ceiling on ember-glow
    // stacking (not ink tricks) carries legibility.
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
    // THE NO-RECOLOR LAW: ink never changes — nothing to mistake for lag.
    assert!(
        glow.charred().is_empty(),
        "a blaze never recolors ink (the owner's no-char verdict)"
    );
    assert!(
        glow.halos().iter().all(|h| h.mode == HaloMode::Add),
        "no Over-mode veils ride the fire (the cool-pocket experiment is retired)"
    );
    // What the engulfment DOES drive is the colour-free CONTRAST-HALO
    // stream — legibility without touching a single ink byte.
    assert!(
        !glow.halo_cells().is_empty(),
        "a towering blaze emits contrast-halo strengths for its engulfed cells"
    );
    // Decay: the field still dies to exactly empty.
    let fp = glow.tick(Some((4, 39)), t + Duration::from_secs(30), &c, g, &mut out);
    assert!(glow.patches().is_empty(), "fire patches decay");
    assert!(glow.halo_cells().is_empty(), "contrast-halo cells decay");
    assert_eq!(fp, 0);
}

/// THE CONTRAST-HALO LAW (the owner's "I can't read the white text over
/// the very bright flame"): a hot blaze emits colour-free halo STRENGTHS
/// for the cells the flame engulfs — rising toward the head, where the
/// fire is brightest — while the ink itself is NEVER recoloured (no
/// charred entries; the no-recolor law). Row 0 included: the halo
/// protects text everywhere the flame covers, so the v0.42
/// cursor-row/freshness gates deliberately do NOT apply — the halo does
/// not recolor, so there is nothing to read as lag. The stream honours the
/// renderer's walk invariant: per-(row, col) sorted, unique cells.
#[test]
fn hot_blaze_emits_halo_strengths_rising_toward_the_head_and_never_recolors() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    // A hot ROW-0 blaze (the smolder-test hot loop, on the top row): even
    // the top row — no cursor-row gate — earns its halo cells.
    let mut t = Instant::now();
    glow.tick(Some((0, 0)), t, &c, g, &mut out);
    // Key-repeat cadence, LONG sustain: the v0.45 feel dials slowed the
    // attack (DISP_ATTACK_S 0.34 — the blaze is EARNED), so reaching a
    // multi-cell engulfment takes a real run, exactly as on glass.
    for i in 1..=34u16 {
        t += Duration::from_millis(33);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((0, i)), t, &c, g, &mut out);
    }
    let cells = glow.halo_cells();
    assert!(!cells.is_empty(), "a hot blaze emits contrast-halo cells");
    // THE NO-RECOLOR LAW stays intact beside the halo: zero ink overrides.
    assert!(
        glow.charred().is_empty(),
        "the halo never recolors ink — charred stays empty (the no-recolor law)"
    );
    // Sorted unique (row, col): the renderers' lockstep merge-walk contract.
    assert!(
        cells
            .windows(2)
            .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col)),
        "halo cells are per-(row, col) sorted with unique cells: {cells:?}"
    );
    // Strength RISES toward the head: the freshly-typed cells burn hotter
    // (steep head→tail spark falloff), so on the typed row the head-most
    // engulfed cell out-rims the tail-most one.
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

/// Campaign 2: on a LIGHT theme the fire patches switch to the ink-fire
/// ([`FireMode::Over`]) so the flames READ on white.
#[test]
fn light_theme_fire_is_ink_over_mode() {
    let g = geom();
    let mut c = cfg(GlowStyle::Fire, true);
    c.dark_theme = false;
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=20u16 {
        t += Duration::from_millis(30);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    assert!(!glow.patches().is_empty(), "light-theme fire still emits");
    assert!(
        glow.patches().iter().all(|q| q.mode == FireMode::Over),
        "light-theme fire is source-over ink"
    );
}

/// RE-JUDGE (fire head, dark AND light): the OCCUPIED-CELL discipline pins
/// the empty-cursor-cell punch above the glyph-cell ceiling, so the freshest
/// letters keep contrast THROUGH the flame while the light still visibly
/// leaves the cursor.
#[test]
#[allow(
    clippy::assertions_on_constants,
    reason = "pins a compile-time-constant invariant between two coverage caps"
)]
fn fire_streak_caps_are_ordered_for_the_head_bridge() {
    assert!(
        CursorGlow::FIRE_STREAK_COV_CAP < CursorGlow::FIRE_HEAD_COV_CAP,
        "the streak over glyph cells is capped BELOW the empty-cursor-cell bridge"
    );
}

/// RE-JUDGE (finding 1/4): FIRE embers obey the occupied-cell discipline at
/// the head. The dense pile of freshly-born near-white ember halos that
/// clusters over the freshest typed glyph cells was THE head white-out; an
/// ember whose centre sits over a cell the row probe shows as a GLYPH dims
/// hard so the letters read THROUGH the flame, while blank cells (the cursor
/// cell, gaps, the risen airspace) keep full punch. Same injected clock ⇒ the
/// TWO runs spawn identical embers, so the only delta is the glyph dimming.
#[test]
fn fire_embers_dim_over_probed_glyph_cells() {
    let g = geom();
    let mut c = cfg(GlowStyle::Fire, true);
    c.radius = 0.0; // no crown — isolate the ember halos
    c.ring = false;
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    // Sum the premultiplied luminance of every ADDITIVE (Add-mode) halo — the
    // ember shower — over a hot typed run.
    let ember_light = |probe: Option<&str>| -> u64 {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = Instant::now();
        if let Some(s) = probe {
            glow.observe_row(2, 0, &row(s), t);
        }
        glow.tick(Some((2, 0)), t, &c, g, &mut out);
        for col in 1..=16u16 {
            t += Duration::from_millis(28);
            if let Some(s) = probe {
                glow.observe_row(2, col, &row(s), t);
            }
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((2, col)), t, &c, g, &mut out);
        }
        glow.halos()
            .iter()
            .filter(|h| h.mode == HaloMode::Add)
            .map(|h| {
                (h.w as u64)
                    * (h.h as u64)
                    * (((h.color >> 16) & 0xff) + ((h.color >> 8) & 0xff) + (h.color & 0xff)) as u64
            })
            .sum()
    };
    let over_glyphs = ember_light(Some("aaaaaaaaaaaaaaaaaaaa"));
    let over_blank = ember_light(Some(""));
    assert!(over_blank > 0, "the ember shower must actually be lit");
    assert!(
        (over_glyphs as f64) < (over_blank as f64) * 0.8,
        "embers dim over probed glyph cells (glyphs {over_glyphs} vs blank {over_blank})"
    );
}

/// RE-JUDGE (finding 4): FIRE drops its ADDITIVE "mouth of fire" crown on a
/// LIGHT theme — the broad hot heart reached left over the just-typed row
/// and, being additive, lifted the trailing glyphs toward the white ground.
/// The tell is the crown-sized Add halo (rx spanning multiple cells, far
/// wider than any ember): present on dark, gone on light, where the flame
/// body + the streak's source-over veil rails carry the look.
#[test]
fn fire_light_drops_the_additive_crown() {
    let g = geom();
    let crown_rx = 2 * g.ch as u16; // ≫ the ≤ ~1-cell ember halos
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

/// The residual trail of TYPED text is snuffed by a line change: type hot
/// on one row (long chained spark lives), press Enter, and within a beat
/// the old row emits nothing — the fire follows the cursor to its line.
#[test]
fn fire_row_change_snuffs_the_old_lines_trail() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    // Hot typing across row 2 — chained lives keep the trail burning.
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
    // A scripted line change retires the old row and owns the landing.
    t += Duration::from_millis(60);
    glow.note_synthetic_move(t);
    glow.tick(Some((3, 0)), t, &c, g, &mut out);
    // Within ~a quarter second the old row's flames have guttered out.
    t += Duration::from_millis(250);
    glow.tick(Some((3, 0)), t, &c, g, &mut out);
    assert!(
        glow.sparks.iter().all(|s| s.row == 3),
        "the old line's trail is snuffed after a line change"
    );
    // The landing cell's own curtain may tower into the band above it —
    // that's the fire's height, not a lingering trail — so only columns
    // away from the landing neighbourhood must be dark on the old row.
    assert!(
        out.iter().all(|q| q.row != 2 || (q.x as usize) < 8 * g.cw),
        "the old row is dark away from the landing after a line change"
    );
}
