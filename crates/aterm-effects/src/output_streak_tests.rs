// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 384,
        head: 0,
    }
}

const fn cfg_on() -> StreakConfig {
    StreakConfig {
        enabled: true,
        sound: true,
        ..StreakConfig::DEFAULT
    }
}

const fn span(row: u16) -> [(u16, u16, u16); 1] {
    [(row, 0, 40)]
}

fn run(
    e: &mut OutputStreak,
    t0: Instant,
    steps: u32,
    step: Duration,
    cfg: &StreakConfig,
    mut before_tick: impl FnMut(&mut OutputStreak, Instant, u32),
) -> (Vec<StreakCue>, usize) {
    let mut cues = Vec::new();
    let mut peak = 0;
    for i in 0..steps {
        let now = t0 + step * i;
        before_tick(e, now, i);
        let mut out = Vec::new();
        let f = e.tick(now, geom(), cfg, &mut out);
        peak = peak.max(out.len());
        if let Some(c) = f.cue {
            cues.push(c);
        }
    }
    (cues, peak)
}

#[test]
fn an_unchanged_token_is_no_arrival_and_rebase_baselines() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(7);
    assert!(
        !e.note_output(500, &span(1), t0, false),
        "the first observation only baselines"
    );
    assert!(
        !e.note_output(500, &span(1), t0 + Duration::from_millis(10), false),
        "the same token twice is a repaint, not an arrival"
    );
    assert!(
        e.note_output(501, &span(1), t0 + Duration::from_millis(20), false),
        "a changed token IS an arrival"
    );
    e.rebase();
    assert!(!e.note_output(9_000, &span(1), t0 + Duration::from_millis(30), false));
    assert!(e.note_output(9_001, &span(1), t0 + Duration::from_millis(40), false));
}

#[test]
fn cell_observation_is_gated_by_the_host_token_before_geometry() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(9);
    let mut cells = vec![vec![aterm_core::terminal::RenderCell::default(); 8]; 3];
    cells[1][2].ch = 'a';
    assert!(!e.note_output_cells(&cells, (2, 0), (3, 8), 10, t0, false));

    cells[1][6].ch = 'z';
    assert!(
        !e.note_output_cells(
            &cells,
            (2, 0),
            (3, 8),
            10,
            t0 + Duration::from_millis(20),
            false,
        ),
        "glyph churn without a content-clock edge is only a repaint"
    );
    assert!(e.pending.is_none());

    assert!(e.note_output_cells(
        &cells,
        (2, 0),
        (3, 8),
        11,
        t0 + Duration::from_millis(40),
        false,
    ));
    let pending = e.pending.expect("changed token licenses the ink row");
    assert_eq!(
        (pending.row, pending.left, pending.right),
        (1, 6, 6),
        "unchanged ink and gaps stay outside the newly changed run"
    );
}

#[test]
fn a_comet_retires_when_its_ink_is_cleared() {
    let row_lit = |clear: bool| {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(11);
        let cfg = cfg_on();
        let blank = vec![vec![aterm_core::terminal::RenderCell::default(); 8]; 3];
        let mut cells = blank.clone();
        assert!(!e.note_output_cells(&cells, (2, 0), (3, 8), 10, t0, false));
        cells[1][0].ch = '$';
        let t1 = t0 + Duration::from_millis(40);
        assert!(e.note_output_cells(&cells, (1, 2), (3, 8), 11, t1, false));
        let mut out = Vec::new();
        e.tick(t1, geom(), &cfg, &mut out);
        assert!(
            out.iter().any(|q| q.row == 1),
            "the output earns a comet on its row"
        );
        let mut after = if clear { blank.clone() } else { cells.clone() };
        after[0][0].ch = '$';
        let t2 = t1 + Duration::from_millis(50);
        e.note_output_cells(&after, (0, 2), (3, 8), 12, t2, false);
        out.clear();
        e.tick(t2, geom(), &cfg, &mut out);
        out.iter().any(|q| q.row == 1)
    };
    assert!(!row_lit(true), "a cleared row keeps no ghost of its comet");
    assert!(
        row_lit(false),
        "control: a comet over ink still there sweeps on"
    );
}

#[test]
fn cell_observation_all_blank_row_mints_nothing() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(9);
    let cells = vec![vec![aterm_core::terminal::RenderCell::default(); 8]; 3];
    assert!(!e.note_output_cells(&cells, (2, 0), (3, 8), 10, t0, false));
    assert!(!e.note_output_cells(
        &cells,
        (2, 0),
        (3, 8),
        11,
        t0 + Duration::from_millis(20),
        false,
    ));
    assert!(e.pending.is_none(), "a blank anchor row has no ink extent");
}

#[test]
fn theme_hue_uses_named_stop_positions_with_bounded_non_wrapping_jitter() {
    let pending = Pending {
        row: 9,
        left: 3,
        right: 12,
    };
    let last = crate::spectrum::SPECTRUM_STOPS - 1;
    for named in 0..crate::spectrum::SPECTRUM_STOPS {
        let cursor = spectrum_stop(named);
        assert_eq!(
            nearest_spectrum_stop_by_hue(cursor),
            named,
            "named stop {named} must select itself"
        );
        let cfg = StreakConfig {
            theme_cursor: cursor,
            ..cfg_on()
        };
        let anchor = spectrum_stop_position(named);
        let lower_midpoint = if named == 0 {
            0.0
        } else {
            (spectrum_stop_position(named - 1) + anchor) * 0.5
        };
        let upper_midpoint = if named == last {
            1.0
        } else {
            (anchor + spectrum_stop_position(named + 1)) * 0.5
        };
        for seed in 0..32 {
            let actual = OutputStreak::new(seed).hue_for(pending, &cfg);
            assert!(
                (lower_midpoint..=upper_midpoint).contains(&actual),
                "jitter escaped stop {named}'s Voronoi interval: {actual} not in \
                 {lower_midpoint}..={upper_midpoint}"
            );
        }
    }

    let violet_edge = StreakConfig {
        theme_cursor: spectrum_stop(crate::spectrum::SPECTRUM_STOPS - 1),
        ..cfg_on()
    };
    assert_eq!(
        nearest_spectrum_stop_by_hue(violet_edge.theme_cursor),
        crate::spectrum::SPECTRUM_STOPS - 1
    );
    let violet_lower = (spectrum_stop_position(last - 1) + spectrum_stop_position(last)) * 0.5;
    for seed in 0..256 {
        let actual = OutputStreak::new(seed).hue_for(pending, &violet_edge);
        assert!(actual >= violet_lower, "violet jitter wrapped: {actual}");
    }

    assert_eq!(nearest_spectrum_stop_by_hue(0x0055_FF00), 3);
}

#[test]
fn clipping_rejects_malformed_and_wholly_off_right_spans() {
    assert_eq!(
        clipped_span(
            Pending {
                row: 0,
                left: 6,
                right: 12,
            },
            8,
        ),
        Some((6, 7)),
        "a partly visible span clips only its right edge"
    );
    assert_eq!(
        clipped_span(
            Pending {
                row: 0,
                left: 8,
                right: 12,
            },
            8,
        ),
        None,
        "a wholly off-right span must not relocate onto the last cell"
    );
    assert_eq!(
        clipped_span(
            Pending {
                row: 0,
                left: 5,
                right: 4,
            },
            8,
        ),
        None,
        "a reversed span is malformed rather than a one-cell flash"
    );
}

#[test]
fn echo_discount_mints_nothing_including_paste() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(1);
    e.note_output(1, &span(2), t0, false); // baseline
    e.note_keystroke(t0 + Duration::from_millis(100));
    assert!(
        !e.note_output(2, &span(2), t0 + Duration::from_millis(200), false),
        "a delta inside the echo window is the user's own typing"
    );
    let mut e2 = OutputStreak::new(1);
    e2.note_output(1, &span(2), t0, false);
    assert!(
        !e2.note_output(2, &span(2), t0 + Duration::from_secs(9), true),
        "input_hot alone must discount output while input is active/spilled"
    );
}

#[test]
fn submitted_turn_boundary_clears_only_older_echo_shadow() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(1);
    e.note_output(1, &span(2), t0, false);
    e.note_keystroke(t0 + Duration::from_millis(10));
    e.note_turn_boundary(t0 + Duration::from_millis(20));
    assert!(e.note_output(2, &span(2), t0 + Duration::from_millis(21), false));

    let mut newer = OutputStreak::new(1);
    newer.note_output(1, &span(2), t0, false);
    newer.note_keystroke(t0 + Duration::from_millis(30));
    newer.note_turn_boundary(t0 + Duration::from_millis(20));
    assert!(
        !newer.note_output(2, &span(2), t0 + Duration::from_millis(31), false),
        "a replayed older boundary must not erase newer accepted input"
    );
}

#[test]
fn empty_damage_mints_nothing() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(1);
    e.note_output(1, &span(0), t0, false);
    assert!(!e.note_output(2, &[], t0 + Duration::from_secs(1), false));
    let mut out = Vec::new();
    let f = e.tick(t0 + Duration::from_secs(1), geom(), &cfg_on(), &mut out);
    assert_eq!(f.fp, 0, "nothing was licensed, so nothing is drawn");
    assert!(out.is_empty());
}

#[test]
fn reduced_motion_emits_nothing() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(3);
    let cfg = cfg_on();
    e.note_output(1, &span(4), t0, false);
    e.note_output(2, &span(4), t0 + Duration::from_millis(400), false);
    let mut warm = Vec::new();
    e.tick(t0 + Duration::from_millis(400), geom(), &cfg, &mut warm);
    assert!(!warm.is_empty(), "premise: the effect is live");

    let reduced = StreakConfig {
        intensity: 0.0,
        ..cfg
    };
    let mut out = Vec::new();
    let f = e.tick(t0 + Duration::from_millis(450), geom(), &reduced, &mut out);
    assert_eq!(f.fp, 0);
    assert!(f.cue.is_none());
    assert!(out.is_empty());
    assert!(!e.is_active(), "and the host may disarm the lane");
}

#[test]
fn disabled_is_inert() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(5);
    let cfg = StreakConfig {
        enabled: false,
        ..cfg_on()
    };
    for i in 0..20u64 {
        e.note_output(i, &span(2), t0 + Duration::from_millis(i * 50), false);
        let mut out = Vec::new();
        let f = e.tick(t0 + Duration::from_millis(i * 50), geom(), &cfg, &mut out);
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
    }
}

#[test]
fn a_two_row_alternation_cannot_strobe_the_ribbon() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(17);
    let cfg = cfg_on();
    let g = geom();

    let steps = 960u64;
    let hz = 120u64;
    let mut homes = 0u32;
    let mut prev: Option<(u16, i32, i32)> = None;
    for i in 0..steps {
        let at = t0 + Duration::from_millis(i * 1000 / hz);
        let row = if i % 2 == 0 { 5 } else { 6 };
        e.note_output(i * 4096, &span(row), at, false);
        let mut out = Vec::new();
        e.tick(at, g, &cfg, &mut out);
        let now = e.ribbon.map(|r| (r.row, r.from_col, r.to_col));
        if now.is_some() && now != prev {
            homes += 1;
        }
        prev = now;
    }

    let secs = steps as f32 / hz as f32;
    let per_s = homes as f32 / secs;
    let floor_per_s = 1000.0 / SPAWN_MIN_GAP_MS as f32;
    assert!(
        per_s <= floor_per_s + 0.05,
        "the ribbon re-homed {homes} times in {secs:.1} s = {per_s:.2}/s, above the \
         ignition floor of {floor_per_s:.2}/s. A re-home moves light off one whole row \
         and onto another; unbounded, this is the strobe that measured 60 re-homes per \
         second before the floor covered it."
    );
    assert!(
        homes >= 2,
        "the drive must actually MOVE the ribbon, or this test cannot fail: {homes} \
         re-home(s) means the fixture stopped exercising the alternation"
    );
}

#[test]
fn a_flood_coalesces_to_one_quiet_ribbon() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(11);
    let cfg = cfg_on();
    let step = Duration::from_millis(1);
    let mut seq = 0u64;
    let (cues, peak) = run(&mut e, t0, 3_000, step, &cfg, |e, now, i| {
        seq = u64::from(i) + 1;
        e.note_output(seq, &span((i % 20) as u16), now, false);
    });
    let pips = cues
        .iter()
        .filter(|c| c.sound == StreakSound::Shimmer)
        .count();
    assert!(
        pips <= 1,
        "a three-second flood is ONE pip, not a drip feed: {pips}"
    );
    assert!(
        peak <= 80 * (MAX_COMETS + 1),
        "the flood's quad cost is bounded: {peak}"
    );
}

#[test]
fn the_spawn_gap_clamps_eye_and_ear_together() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(2);
    let cfg = cfg_on();
    let mut spawns = 0;
    for i in 0..20u64 {
        let now = t0 + Duration::from_millis(i * 100);
        e.note_output(i + 1, &span((i % 20) as u16), now, false);
        let before = e.last_spawn;
        let mut out = Vec::new();
        e.tick(now, geom(), &cfg, &mut out);
        if e.last_spawn != before {
            spawns += 1;
        }
    }
    assert!(
        spawns <= 4,
        "≤2 ignitions/second across 2 s of steady output: {spawns}"
    );
}

#[test]
fn an_episode_is_one_pip_and_one_exhale() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(4);
    let cfg = cfg_on();
    let (mut cues, _) = run(
        &mut e,
        t0,
        120,
        Duration::from_millis(50),
        &cfg,
        |e, now, i| {
            if i % 16 == 0 {
                e.note_output(u64::from(i) + 1, &span((i % 12) as u16), now, false);
            }
        },
    );
    let (tail, _) = run(
        &mut e,
        t0 + Duration::from_secs(6),
        200,
        Duration::from_millis(50),
        &cfg,
        |_, _, _| {},
    );
    cues.extend(tail);
    let pips = cues
        .iter()
        .filter(|c| c.sound == StreakSound::Shimmer)
        .count();
    let exhales = cues
        .iter()
        .filter(|c| c.sound == StreakSound::Settle)
        .count();
    assert_eq!(pips, 1, "one pip opens the episode: {pips}");
    assert_eq!(exhales, 1, "one exhale closes it: {exhales}");
}

#[test]
fn idle_settles_to_fingerprint_zero_and_no_deadline() {
    let t0 = Instant::now();
    let mut e = OutputStreak::new(6);
    let cfg = StreakConfig {
        idle_secs: 2.0,
        ..cfg_on()
    };
    e.note_output(1, &span(5), t0, false);
    e.note_output(2, &span(5), t0 + Duration::from_millis(300), false);
    run(
        &mut e,
        t0,
        40,
        Duration::from_millis(50),
        &cfg,
        |_, _, _| {},
    );
    let late = t0 + Duration::from_secs(30);
    let mut out = Vec::new();
    let f = e.tick(late, geom(), &cfg, &mut out);
    assert_eq!(f.fp, 0);
    assert!(out.is_empty());
    assert!(!e.is_active());
    assert_eq!(e.next_change_deadline(late, cfg.idle_secs), None);
}

#[test]
fn parked_episode_wakes_at_idle_cutoff_before_later_settle() {
    let t0 = Instant::now();
    let cfg = StreakConfig {
        idle_secs: 2.0,
        ..cfg_on()
    };
    let mut e = OutputStreak::new(71);
    assert!(!e.note_output(1, &span(4), t0, false));
    let spawned = t0 + Duration::from_millis(300);
    assert!(e.note_output(2, &span(4), spawned, false));
    let mut out = Vec::new();
    assert_eq!(
        e.tick(spawned, geom(), &cfg, &mut out)
            .cue
            .map(|cue| cue.sound),
        Some(StreakSound::Shimmer)
    );
    e.momentum.set_value(spawned, 1.0);
    for step in 1..=4 {
        out.clear();
        e.tick(
            spawned + Duration::from_millis(250 * step),
            geom(),
            &cfg,
            &mut out,
        );
    }
    let parked = spawned + Duration::from_secs(1);
    assert!(!e.is_active());
    let idle = spawned + Duration::from_secs(2);
    assert_eq!(e.next_change_deadline(parked, cfg.idle_secs), Some(idle));

    out.clear();
    let drained = e.tick(idle, geom(), &cfg, &mut out);
    assert_eq!(drained.fp, 0);
    assert!(
        drained.cue.is_none(),
        "momentum still owes its later Settle"
    );
    let settle = e
        .next_change_deadline(idle, cfg.idle_secs)
        .expect("the episode still owes its analytic Settle");
    assert!(
        settle > idle,
        "the consumed idle cutoff cannot spin overdue"
    );
}

#[test]
fn fractional_idle_deadline_is_consumed_at_the_exact_promised_boundary() {
    let t0 = Instant::now();
    let cfg = StreakConfig {
        idle_secs: 0.1,
        ..cfg_on()
    };
    let mut e = OutputStreak::new(73);
    assert!(!e.note_output(1, &span(4), t0, false));
    let spawned = t0 + Duration::from_millis(300);
    assert!(e.note_output(2, &span(4), spawned, false));
    let mut out = Vec::new();
    e.tick(spawned, geom(), &cfg, &mut out);
    e.comets = Default::default();
    e.drawing = false;

    let exact = spawned + Duration::from_secs_f32(cfg.idle_secs);
    assert_eq!(e.next_change_deadline(spawned, cfg.idle_secs), Some(exact));
    out.clear();
    e.tick(exact, geom(), &cfg, &mut out);
    assert!(e.last_output.is_none(), "the exact cutoff was consumed");
    assert_ne!(
        e.next_change_deadline(exact, cfg.idle_secs),
        Some(exact),
        "float rounding cannot manufacture an overdue wake loop"
    );
}

#[test]
fn huge_finite_idle_value_drains_immediately_without_panicking() {
    let last = Instant::now();
    let cfg = StreakConfig {
        idle_secs: f32::MAX,
        ..cfg_on()
    };
    let mut e = OutputStreak::new(79);
    e.in_episode = true;
    e.last_output = Some(last);

    assert_eq!(idle_duration(f32::MAX), None);
    assert_eq!(e.pending_idle_deadline(f32::MAX), Some(last));
    assert!(e.idle_drained(last, &cfg));
}

#[test]
fn no_configuration_can_exceed_the_amplitude_ceiling() {
    let t0 = Instant::now();
    let ceiling = (ALPHA_CLAMP * 255.0).round() as u32;
    for dark in [true, false] {
        let cfg = StreakConfig {
            intensity: 9.9,
            dark_theme: dark,
            theme_bg: if dark { 0x0000_0000 } else { 0x00FF_FFFF },
            ..cfg_on()
        };
        let mut e = OutputStreak::new(8);
        let mut peak = 0;
        for i in 0..40u64 {
            let now = t0 + Duration::from_millis(i * 40);
            e.note_output(i + 1, &span((i % 6) as u16), now, false);
            let mut out = Vec::new();
            e.tick(now, geom(), &cfg, &mut out);
            for q in &out {
                for sh in [16, 8, 0] {
                    peak = peak.max((q.color >> sh) & 0xff);
                }
            }
        }
        assert!(
            peak <= ceiling,
            "dark={dark}: peak channel {peak} exceeds the {ceiling} ceiling"
        );
    }
}

#[test]
fn light_themes_get_shadow_not_invisible_additive_light() {
    let t0 = Instant::now();
    let sample = |dark: bool| {
        let cfg = StreakConfig {
            dark_theme: dark,
            theme_bg: if dark { 0x0000_0000 } else { 0x00FD_F6E3 },
            ..cfg_on()
        };
        let mut e = OutputStreak::new(9);
        let mut modes = Vec::new();
        for i in 0..30u64 {
            let now = t0 + Duration::from_millis(i * 40);
            e.note_output(i + 1, &span((i % 6) as u16), now, false);
            let mut out = Vec::new();
            e.tick(now, geom(), &cfg, &mut out);
            modes.extend(out.iter().map(|q| q.alpha));
        }
        modes
    };
    let dark = sample(true);
    let light = sample(false);
    assert!(!dark.is_empty() && !light.is_empty(), "both themes drew");
    assert!(
        dark.iter().all(|&a| a == 0),
        "a dark theme's streak is additive light"
    );
    assert!(
        light.iter().any(|&a| a > 0),
        "a light theme's streak composites source-over so it can darken"
    );
}

#[test]
fn a_comet_paints_one_contiguous_run() {
    let t0 = Instant::now();
    let cfg = cfg_on();
    let mut e = OutputStreak::new(21);
    e.note_output(1, &span(4), t0, false);
    e.note_output(2, &span(4), t0 + Duration::from_millis(50), false);
    for i in 0..8u64 {
        let now = t0 + Duration::from_millis(50 + i * 30);
        let mut out = Vec::new();
        e.tick(now, geom(), &cfg, &mut out);
        if out.is_empty() {
            continue;
        }
        let mut xs: Vec<u16> = out.iter().map(|q| q.x / 8).collect();
        xs.sort_unstable();
        xs.dedup();
        for pair in xs.windows(2) {
            assert_eq!(pair[1] - pair[0], 1, "the sweep must be gapless: {xs:?}");
        }
    }
}
