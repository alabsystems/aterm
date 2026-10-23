// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The water style.

use super::*;

/// Every emitted Water quad keeps blue at least as strong as red across spawn and flight.
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

/// Sustained fast typing emits far more light and droplet quads than relaxed typing.
#[test]
fn water_scales_from_trickle_to_torpedo() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

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

/// Sustained typing raises heat and emits a contiguous curved wake with the shared beam disabled.
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

/// At a 156 ms typing cadence, the wake trails at least three cells behind the caret as one contiguous run.
#[test]
fn water_wake_trails_behind_the_caret_at_human_cadence() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((2, 1)), t, &c, g, &mut out);
    for col in 2..=20u16 {
        for _ in 0..2 {
            t += ms(52);
            glow.tick(Some((2, col - 1)), t, &c, g, &mut out);
        }
        t += ms(52);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    glow.particles.clear();
    t += ms(78);
    glow.tick(Some((2, 20)), t, &c, g, &mut out);
    let caret_x = 20 * g.cw as u16;
    let lit: std::collections::BTreeSet<u16> = glow
        .under_quads()
        .iter()
        .filter(|q| q.row == 2 && luma_of(q.color) >= 24)
        .flat_map(|q| q.x..q.x.saturating_add(q.w))
        .filter(|&x| x < caret_x)
        .collect();
    let reach = lit
        .iter()
        .next()
        .map_or(0, |first| caret_x.saturating_sub(*first));
    assert!(
        reach >= 3 * g.cw as u16,
        "the wake reaches three cells behind the caret at 156 ms a key: {reach}px"
    );
    let gaps = lit
        .iter()
        .zip(lit.iter().skip(1))
        .filter(|(a, b)| **b != **a + 1);
    assert_eq!(gaps.count(), 0, "the wake is one contiguous run");

    let mut lone = CursorGlow::default();
    let mut t = Instant::now();
    lone.tick(Some((2, 1)), t, &c, g, &mut out);
    t += ms(1000);
    lone.note_synthetic_typed(t, 1);
    lone.tick(Some((2, 2)), t, &c, g, &mut out);
    lone.particles.clear();
    lone.tick(Some((2, 2)), t + ms(250), &c, g, &mut out);
    assert!(
        lone.under_quads()
            .iter()
            .filter(|q| q.row == 2 && luma_of(q.color) >= 24)
            .all(|q| q.x + q.w <= 2 * g.cw as u16 + g.cw as u16),
        "a lone key keeps its short base life"
    );
}

/// The Rec. 709 luma of a premultiplied additive quad colour.
fn luma_of(color: u32) -> u32 {
    let (r, g, b) = ((color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff);
    (2126 * r + 7152 * g + 722 * b) / 10_000
}

/// A licensed navigation jump spans the full vector without adding heat, splash particles, or a landing ring.
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

/// Repeated crossings of the same corridor deduplicate to the newest owner per cell and stay under glyph ink.
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
    let newest_color = CursorGlow::water_crest_color(newest.pos, heat);
    let previous_color = CursorGlow::water_crest_color(previous.pos, heat);
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

    let mut hits = vec![0u16; usize::from(g.win_w) * usize::from(g.win_h)];
    for q in glow.under_quads() {
        for y in usize::from(q.y)..usize::from(q.y + q.h) {
            for x in usize::from(q.x)..usize::from(q.x + q.w) {
                hits[y * usize::from(g.win_w) + x] += 1;
            }
        }
    }
    assert!(hits.into_iter().max().unwrap_or(0) >= 2);

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

/// A jump reflection transfers into downward-accelerating beads and fades smoothly to idle zero.
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

/// On a light theme, Water emits source-over ink under the glyphs while preserving 4.5:1 foreground contrast.
#[test]
fn water_on_light_is_ink_under_the_letters() {
    let g = geom();
    let mut c = cfg(GlowStyle::Water, true);
    c.dark_theme = false;
    c.theme_fg = 0x001F_2328;
    c.theme_bg = 0x00FF_FFFF;
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let table = crate::color_math::relative_luminance_table();
    let lum = |px: u32| crate::color_math::relative_luminance_with(table, px);
    let fg = lum(c.theme_fg);
    let (mut darkest_cell, mut darkest_run) = (1.0f32, 1.0f32);
    let (mut wake_rows, mut ripple) = (std::collections::BTreeSet::new(), false);
    let mut check = |glow: &CursorGlow, out: &[GlowQuad], typing: bool, ring: bool| {
        for q in out.iter().chain(glow.under_quads()) {
            assert!(q.alpha > 0, "an additive quad on a light ground: {q:?}");
        }
        for h in glow.halos() {
            assert_eq!(h.mode, HaloMode::Over, "an additive halo on light: {h:?}");
        }
        ripple |= ring && out.iter().any(|q| q.alpha > 0);
        let mut glass = Glass::new(g, c.theme_bg);
        glass.add_quads(glow.under_quads());
        if typing {
            darkest_run = glass
                .px
                .iter()
                .map(|&px| lum(px))
                .fold(darkest_run, f32::min);
        }
        for row in 0..g.rows as u16 {
            for col in 0..g.cols as u16 {
                let (x0, y0, x1, _) = glass.cell_rect(row, col);
                let (top, bot) = (y0 + g.ch as i32 * 3 / 10, y0 + g.ch as i32 * 8 / 10);
                let band: Vec<f32> = (top..bot)
                    .flat_map(|y| (x0..x1).map(move |x| (y, x)))
                    .map(|(y, x)| lum(glass.at(y, x)))
                    .collect();
                let mean = band.iter().sum::<f32>() / band.len() as f32;
                darkest_cell = darkest_cell.min(mean);
            }
        }
        wake_rows.extend(
            glow.under_quads()
                .iter()
                .filter(|q| q.row == 2)
                .map(|q| q.y),
        );
    };
    let mut t = t0;
    glow.tick(Some((2, 1)), t, &c, g, &mut out);
    for col in 2..=14u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
        check(&glow, &out, true, false);
    }
    assert!(!glow.under_quads().is_empty(), "the wake paints on white");
    let jump = t + Duration::from_millis(60);
    glow.note_synthetic_move(jump);
    glow.tick(Some((4, 34)), jump, &c, g, &mut out);
    assert!(glow.ring.is_some(), "the jump lands a ring");
    check(&glow, &out, false, true);
    for ms in [40u64, 120, 250, 420] {
        glow.tick(
            Some((4, 34)),
            jump + Duration::from_millis(ms),
            &c,
            g,
            &mut out,
        );
        check(&glow, &out, false, ms <= 120);
    }
    assert!(
        wake_rows.len() >= 3,
        "the wake still travels as a wave on white: {wake_rows:?}"
    );
    assert!(ripple, "the landing lays the inked ripple train");
    assert!(
        darkest_run < 0.5,
        "the wake visibly paints the paper: {darkest_run}"
    );
    let ratio = |l: f32| (l + 0.05) / (fg + 0.05);
    assert!(
        ratio(darkest_cell) >= 4.5,
        "a cell's ground under the wake leaves the foreground {:.2}:1",
        ratio(darkest_cell)
    );
    assert!(
        ratio(darkest_run) >= 4.5,
        "the typed run's darkest pixel leaves the foreground {:.2}:1",
        ratio(darkest_run)
    );
}

/// The largest turn, in degrees, between two consecutive segments of a
/// polyline (zero-length segments skipped).
fn sharpest_turn(points: &[(f32, f32)]) -> f32 {
    let segs: Vec<(f32, f32)> = points
        .windows(2)
        .map(|w| (w[1].0 - w[0].0, w[1].1 - w[0].1))
        .filter(|d| d.0.hypot(d.1) > 1e-3)
        .collect();
    segs.windows(2)
        .map(|w| {
            let (a, b) = (w[0].1.atan2(w[0].0), w[1].1.atan2(w[1].0));
            let d = (b - a).abs().to_degrees();
            d.min(360.0 - d)
        })
        .fold(0.0f32, f32::max)
}

/// A hot typed run draws a smooth swell through cell crests, while a cross-row leap forms a straight line.
#[test]
fn the_water_surface_is_a_swell_and_a_leap_is_straight() {
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Water, true);
    c.intensity = 1.0;
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    let row = 3u16;
    glow.tick(Some((row, 2)), t, &c, g, &mut out);
    for col in 3..=26u16 {
        t += Duration::from_millis(45);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((row, col)), t, &c, g, &mut out);
    }
    let samples = glow.water_samples.clone();
    assert!(
        samples.len() >= 8,
        "fixture: a run of water ({})",
        samples.len()
    );
    let mut curve = Vec::new();
    CursorGlow::water_curve(&samples, &mut curve);
    let smooth = sharpest_turn(&curve.iter().map(|v| (v.x, v.y)).collect::<Vec<_>>());
    let chords = sharpest_turn(
        &samples
            .iter()
            .map(|s| (s.surface.x, s.surface.y))
            .collect::<Vec<_>>(),
    );
    assert!(
        smooth * 2.5 < chords,
        "the crest turns {smooth:.1}° at a knot (its chords {chords:.1}°)"
    );
    assert!(chords >= 20.0, "control: the chords turn only {chords:.1}°");
    let stair: Vec<WaterSample> = samples
        .iter()
        .enumerate()
        .map(|(i, s)| WaterSample {
            row: s.row + (i / 3) as u16,
            line_y: s.line_y + (i / 3) as f32 * g.ch as f32,
            ..*s
        })
        .collect();
    CursorGlow::water_curve(&stair, &mut curve);
    let (a, b) = (curve[0], curve[curve.len() - 1]);
    let off = curve
        .iter()
        .map(|v| {
            let u = (v.x - a.x) / (b.x - a.x);
            (v.y - (a.y + (b.y - a.y) * u)).abs()
        })
        .fold(0.0f32, f32::max);
    assert!(
        off <= 1.0,
        "a leap's surface strays {off:.1} px off its line"
    );
}

/// On a light theme, a falling drop renders as a round, saturated blue ink bead.
#[test]
fn a_light_water_drop_is_clear_blue() {
    let g = tall_geom();
    let at = Instant::now();
    let drop = |dark: bool| {
        let mut c = cfg(GlowStyle::Water, true);
        c.intensity = 1.0;
        c.dark_theme = dark;
        if !dark {
            (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
        }
        let mut glow = CursorGlow::default();
        let (x, y) = g.cell_center(1, 20);
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
        let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
        glow.emit_particles(at, &c, g, &mut out, &mut halos, &mut under);
        (out, halos)
    };
    let (ink, halos) = drop(false);
    assert!(halos.is_empty() && !ink.is_empty(), "the light drop is ink");
    for q in &ink {
        let a = u32::from(q.alpha.max(1));
        let un = |sh: u32| (((q.color >> sh) & 0xff) * 255 / a).min(255);
        let rgb = (un(16) << 16) | (un(8) << 8) | un(0);
        let (hue, sat, _) = crate::color_math::rgb2hsv(rgb);
        assert!(
            (190.0..=215.0).contains(&hue) && sat >= 0.6,
            "the drop's ink is not clear water blue: {rgb:06x} ({hue:.0}°, {sat:.2})"
        );
    }
    let rows: std::collections::BTreeMap<u16, u32> =
        ink.iter().fold(Default::default(), |mut m, q| {
            *m.entry(q.y).or_default() += u32::from(q.w);
            m
        });
    let widths: Vec<u32> = rows.values().copied().collect();
    assert!(
        widths.len() >= 3 && widths[widths.len() / 2] > widths[0],
        "the drop is round: row widths {widths:?}"
    );
    let (lit, light) = drop(true);
    assert!(
        lit.iter().all(|q| q.alpha == 0) && !light.is_empty(),
        "control: the dark drop is light"
    );
}

/// Round 2's judge (2026-10-23, the trail gallery's `enter-bottom`): water's
/// Enter streak ran from the echo's end down across the line the command
/// printed, and its drops sat in that line's word gaps, reading as
/// punctuation. An Enter's leap keeps to the new prompt's row: no spark it
/// lays stands on another. The control: a navigation jump of the same vector
/// still spans the rows between.
#[test]
fn water_s_enter_keeps_its_leap_to_the_prompt_row() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let at = t0 + Duration::from_millis(5);
    let laid = |enter: bool| {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        glow.tick(Some((2, 24)), t0, &c, g, &mut out);
        if enter {
            glow.note_return(at);
        } else {
            glow.note_navigation(at);
        }
        glow.tick(Some((4, 2)), at, &c, g, &mut out);
        glow.sparks
            .iter()
            .filter(|s| s.born == at)
            .map(|s| s.row)
            .collect::<Vec<u16>>()
    };
    let rows = laid(true);
    assert!(
        !rows.is_empty() && rows.iter().all(|&r| r == 4),
        "the Enter's leap laid sparks on rows {rows:?}"
    );
    let rows = laid(false);
    assert!(
        rows.iter().any(|&r| r != 4),
        "the control (a navigation jump) laid sparks on rows {rows:?} only"
    );
}
