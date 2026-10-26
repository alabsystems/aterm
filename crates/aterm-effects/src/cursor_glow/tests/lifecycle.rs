// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Light is born on a move, fades to empty, and keeps its invariants.

use super::*;

#[test]
fn no_light_until_a_move() {
    let mut glow = CursorGlow::default();
    let now = Instant::now();
    let mut out = Vec::new();
    assert_eq!(
        glow.tick(
            Some((2, 0)),
            now,
            &cfg(GlowStyle::Lumen, true),
            geom(),
            &mut out
        ),
        0
    );
    assert!(out.is_empty());
    assert!(!glow.is_active());
}

#[test]
fn jump_spawns_light_and_fades_to_empty() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    let g = geom();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed
    glow.note_synthetic_move(t0);
    let fp = glow.tick(Some((2, 30)), t0, &c, g, &mut out); // jump
    assert_ne!(fp, 0);
    assert!(!out.is_empty());
    assert!(glow.is_active());
    assert_invariants(&out, g);
    let gone = glow.tick(
        Some((2, 30)),
        t0 + Duration::from_millis(600),
        &c,
        g,
        &mut out,
    );
    assert_eq!(gone, 0, "aurora must decay to EXACTLY empty (0% idle)");
    assert!(out.is_empty());
    assert!(!glow.is_active());
}

/// Single-cell advances at a 300 ms cadence chain without going dark mid-gap.
#[test]
fn typing_chain_never_goes_dark() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    let g = geom();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed position
    for k in 1..=3u64 {
        let at = t0 + Duration::from_millis(300 * k);
        glow.note_synthetic_typed(at, 1);
        glow.tick(Some((2, k as u16)), at, &c, g, &mut out);
    }
    let fp = glow.tick(
        Some((2, 3)),
        t0 + Duration::from_millis(300 * 3 + 280),
        &c,
        g,
        &mut out,
    );
    assert!(glow.is_active(), "chained glow stays active mid-gap");
    assert!(
        !out.is_empty() && fp != 0,
        "chained glow still EMITS light 280ms after a 300ms-cadence key"
    );
    assert_invariants(&out, g);
}

/// ConPTY hide-bridge law: a brief hidden frame during keystroke echo still spawns the spark.
#[test]
fn conpty_hide_gap_still_spawns_the_typing_spark() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    let g = geom();
    glow.tick(Some((2, 5)), t0, &c, g, &mut out); // visible at col 5
    glow.tick(None, t0 + Duration::from_millis(15), &c, g, &mut out);
    glow.note_synthetic_typed(t0 + Duration::from_millis(30), 1);
    let fp = glow.tick(
        Some((2, 6)),
        t0 + Duration::from_millis(45),
        &c,
        g,
        &mut out,
    );
    assert!(
        !out.is_empty() && fp != 0 && glow.is_active(),
        "a hide-bridged single-cell advance must spawn the typing spark"
    );
}

/// A far teleport or long hide suppresses the hide bridge.
#[test]
fn hidden_far_teleport_stays_suppressed() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    let g = geom();
    glow.tick(Some((2, 5)), t0, &c, g, &mut out);
    glow.tick(None, t0 + Duration::from_millis(15), &c, g, &mut out);
    let fp = glow.tick(
        Some((5, 30)),
        t0 + Duration::from_millis(45),
        &c,
        g,
        &mut out,
    );
    assert_eq!(fp, 0, "a far teleport out of a hide must not spawn");
    assert!(out.is_empty());
    let mut glow2 = CursorGlow::default();
    glow2.tick(Some((2, 5)), t0, &c, g, &mut out);
    glow2.tick(None, t0 + Duration::from_millis(15), &c, g, &mut out);
    let fp2 = glow2.tick(
        Some((2, 6)),
        t0 + Duration::from_millis(400), // > HIDE_BRIDGE_MS after last visible
        &c,
        g,
        &mut out,
    );
    assert_eq!(fp2, 0, "a long hide must not bridge");
    assert!(out.is_empty());
}

#[test]
fn every_style_keeps_the_invariants() {
    let g = geom();
    for style in [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Classic,
    ] {
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        let c = cfg(style, true);
        glow.tick(Some((3, 5)), t0, &c, g, &mut out);
        glow.note_synthetic_move(t0);
        glow.tick(Some((1, 38)), t0, &c, g, &mut out);
        assert!(
            !out.is_empty() || !glow.halos().is_empty() || !glow.under_quads().is_empty(),
            "{style:?}: expected light on a jump"
        );
        assert_invariants(&out, g);
        for ms in [16u64, 80, 160] {
            glow.tick(
                Some((1, 38)),
                t0 + Duration::from_millis(ms),
                &c,
                g,
                &mut out,
            );
            assert_invariants(&out, g);
        }
    }
}

fn classic_jump(glow: &mut CursorGlow, t0: Instant, g: Geom, out: &mut Vec<GlowQuad>) {
    let c = cfg(GlowStyle::Classic, true);
    glow.tick(Some((2, 4)), t0, &c, g, out);
    let fp = glow.tick(Some((4, 34)), t0 + Duration::from_millis(16), &c, g, out);
    assert!(fp != 0 && !out.is_empty(), "fixture: the classic jump lit");
    assert!(glow.is_active(), "fixture: classic light is live");
}

/// Classic light settles when another style or the master switch takes the frame,
/// and the first tick back in Classic only re-observes the caret.
#[test]
fn classic_settles_when_another_style_or_the_master_switch_takes_the_frame() {
    let g = geom();
    let frame = Duration::from_millis(16);
    for case in ["starts-in-classic", "lumen-classic-lumen", "master-off"] {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        let lumen = cfg(GlowStyle::Lumen, true);
        if case == "lumen-classic-lumen" {
            glow.tick(Some((1, 1)), t0, &lumen, g, &mut out);
        }
        classic_jump(&mut glow, t0 + Duration::from_millis(20), g, &mut out);
        let away = if case == "master-off" {
            cfg(GlowStyle::Classic, false)
        } else {
            lumen
        };
        let mut t = t0 + Duration::from_millis(60);
        while t < t0 + Duration::from_millis(700) {
            glow.tick(Some((4, 34)), t, &away, g, &mut out);
            t += frame;
        }
        assert!(
            !glow.is_active(),
            "{case}: classic light outlived its owner"
        );
        assert!(!glow.needs_frame_cadence(), "{case}: no frame train");
        assert!(
            glow.next_change_deadline(t, frame).is_none(),
            "{case}: no deadline"
        );
        assert_eq!(glow.live_sparks(), 0, "{case}: no sparks reported");
        let fp = glow.tick(
            Some((1, 10)),
            t,
            &cfg(GlowStyle::Classic, true),
            g,
            &mut out,
        );
        assert!(
            fp == 0 && out.is_empty() && glow.halos().is_empty(),
            "{case}: the first tick back in classic only re-observes"
        );
    }
}

/// Light left by another style settles when Classic takes the frame.
#[test]
fn light_another_style_left_settles_when_classic_takes_the_frame() {
    let g = geom();
    let frame = Duration::from_millis(16);
    for style in [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::RainbowKitty,
        GlowStyle::Fire,
        GlowStyle::Water,
    ] {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        let c = cfg(style, true);
        glow.tick(Some((2, 0)), t0, &c, g, &mut out);
        glow.note_synthetic_move(t0);
        glow.tick(Some((2, 30)), t0, &c, g, &mut out);
        for k in 1..=4u16 {
            let at = t0 + Duration::from_millis(30 * u64::from(k));
            glow.note_synthetic_typed(at, 1);
            glow.tick(Some((2, 30 + k)), at, &c, g, &mut out);
        }
        assert!(
            glow.is_active() && glow.needs_frame_cadence(),
            "fixture: {style:?} is lit when classic takes the frame"
        );
        let classic = cfg(GlowStyle::Classic, true);
        let mut t = t0 + Duration::from_millis(140);
        while t < t0 + Duration::from_millis(900) {
            glow.tick(Some((2, 34)), t, &classic, g, &mut out);
            t += frame;
        }
        assert!(!glow.is_active(), "{style:?}: its light outlived its owner");
        assert!(!glow.needs_frame_cadence(), "{style:?}: no frame train");
        assert!(
            glow.next_change_deadline(t, frame).is_none(),
            "{style:?}: no deadline"
        );
        assert_eq!(
            glow.live_sparks(),
            0,
            "{style:?}: no frozen sparks reported"
        );
    }
}

/// While Classic draws, cursor memory follows the caret so the next style sweeps from its last cell.
#[test]
fn a_style_after_classic_sweeps_from_the_caret_classic_last_saw() {
    let g = geom();
    let lumen = cfg(GlowStyle::Lumen, true);
    let classic = cfg(GlowStyle::Classic, true);
    let t0 = Instant::now();
    let back = |via_classic: bool| {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((1, 2)), t0, &lumen, g, &mut out);
        let mut t = t0 + Duration::from_millis(16);
        if via_classic {
            for col in [5u16, 10, 20, 30] {
                glow.tick(Some((4, col)), t, &classic, g, &mut out);
                t += Duration::from_millis(100);
            }
            t += Duration::from_millis(1000);
            glow.tick(Some((4, 30)), t, &classic, g, &mut out);
            t += Duration::from_millis(16);
        }
        glow.note_synthetic_move(t);
        glow.tick(Some((4, 31)), t, &lumen, g, &mut out);
        glow.sparks.iter().map(|s| s.row).collect::<Vec<_>>()
    };
    let rows = back(true);
    assert!(!rows.is_empty(), "fixture: the licensed move lights");
    assert!(
        rows.iter().all(|&row| row == 4),
        "a comet from where the caret stood before classic: {rows:?}"
    );
    assert!(
        back(false).iter().any(|&row| row < 4),
        "control: from the stale cell the move sweeps the rows above"
    );
}

/// Classic light reports live sparks and frame cadence until decayed.
#[test]
fn classic_light_is_reported_and_paced() {
    let g = geom();
    let frame = Duration::from_millis(16);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    classic_jump(&mut glow, t0, g, &mut out);
    assert!(glow.live_sparks() > 0, "sparks= counts the classic comet");
    assert!(glow.needs_frame_cadence(), "the comet moves every frame");
    let at = t0 + Duration::from_millis(32);
    assert_eq!(glow.next_change_deadline(at, frame), Some(at + frame));
    let gone = t0 + Duration::from_millis(700);
    let fp = glow.tick(
        Some((4, 34)),
        gone,
        &cfg(GlowStyle::Classic, true),
        g,
        &mut out,
    );
    assert_eq!(fp, 0, "fixture: the wake decayed");
    assert!(!glow.needs_frame_cadence() && glow.next_change_deadline(gone, frame).is_none());
    assert_eq!(glow.live_sparks(), 0);
}

/// On a light theme, Classic emits under-ink quads, its landing ring's square as source-over ink and
/// source-over halo veils, with a non-zero fingerprint, and no additive quad.
#[test]
fn classic_on_a_light_ground_reaches_the_host_as_ink_and_veils() {
    let g = geom();
    let c = GlowConfig {
        dark_theme: false,
        theme_fg: 0x001F_2328,
        theme_bg: 0x00FF_FFFF,
        ..cfg(GlowStyle::Classic, true)
    };
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    glow.tick(Some((2, 4)), t0, &c, g, &mut out);
    let fp = glow.tick(
        Some((4, 34)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert!(
        !out.is_empty() && out.iter().all(|q| q.alpha > 0),
        "no additive quad on a light ground: the landing ring is source-over ink"
    );
    assert!(
        !glow.under_quads().is_empty() && glow.under_quads().iter().all(|q| q.alpha > 0),
        "the jump is visible as source-over ink under the letters"
    );
    assert!(!glow.halos().is_empty(), "the landing ring is a veil");
    assert!(
        glow.halos()
            .iter()
            .all(|h| matches!(h.mode, aterm_core::render::HaloMode::Over)),
        "every veil is source-over"
    );
    assert_ne!(fp, 0, "a frame of ink and veils fingerprints");
}

/// Classic rides `note_scroll` and `note_band_move` along the content-true diagonal.
#[test]
fn classic_rides_note_scroll_and_note_band_move() {
    let g = geom_codex();
    let c = cfg(GlowStyle::Classic, true);
    let t0 = Instant::now();
    let enter = |carry: &dyn Fn(&mut CursorGlow)| {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((13, 15)), t0, &c, g, &mut out);
        carry(&mut glow);
        glow.tick(
            Some((13, 2)),
            t0 + Duration::from_millis(16),
            &c,
            g,
            &mut out,
        );
        glow
    };
    let on_prompt_row = |glow: &CursorGlow| {
        glow.classic
            .spark_cells()
            .into_iter()
            .filter(|&(row, col)| row == 13 && (6..=15).contains(&col))
            .count()
    };
    let scroll = |glow: &mut CursorGlow| glow.note_scroll(2);
    let band = |glow: &mut CursorGlow| glow.note_band_move(0, 13, -2);
    for (name, carry) in [
        ("scroll", &scroll as &dyn Fn(&mut CursorGlow)),
        ("band", &band),
    ] {
        let glow = enter(carry);
        assert_eq!(
            on_prompt_row(&glow),
            0,
            "{name}: a comet along the prompt row"
        );
        assert!(
            glow.classic.spark_cells().iter().any(|&(row, _)| row == 11),
            "{name}: the comet starts where the command's text now is"
        );
        assert_eq!(
            glow.classic.ring_cell(),
            Some((13, 2)),
            "{name}: the ring lands"
        );
    }
    assert!(
        on_prompt_row(&enter(&|_: &mut CursorGlow| {})) > 0,
        "control: untold, the move is a horizontal comet on the prompt row"
    );
}
