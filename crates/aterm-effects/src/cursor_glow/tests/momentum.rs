// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared momentum and the composed-pane folds.

use super::*;

#[test]
fn momentum_unifies_glow_and_cat_metrics() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut cat = crate::kitty_cursor::CursorCat::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((3, 0)), t, &c, g, &mut out);
    for col in 1..=10u16 {
        t += Duration::from_millis(60);
        cat.on_key(t, true);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((3, col)), t, &c, g, &mut out);
        assert!(
            (glow.typing_momentum(t) - cat.momentum(t)).abs() < 1e-6,
            "one law, one value: glow {} vs cat {}",
            glow.typing_momentum(t),
            cat.momentum(t)
        );
    }
    assert!(!cat.is_active(), "cat wake: unearned ⇒ no cat");
    assert_eq!(
        cat.frame(t).alpha,
        0,
        "cat visibility: an unearned run has no live companion frame"
    );
    t += Duration::from_millis(80);
    let before = glow.typing_momentum(t);
    cat.on_key(t, false);
    arm_exact_backspace(&mut glow, t, (3, 10), (3, 9));
    glow.tick(Some((3, 9)), t, &c, g, &mut out);
    assert!(
        (glow.typing_momentum(t) - cat.momentum(t)).abs() < 1e-6,
        "the delete drain is the same one law"
    );
    assert!(
        glow.typing_momentum(t) < before,
        "deletes drain, never build"
    );
    let (mut row, mut col) = (3u16, 9u16);
    for _ in 0..110 {
        t += Duration::from_millis(60);
        if col >= 39 {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
        cat.on_key(t, true);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((row, col)), t, &c, g, &mut out);
        assert!(
            (glow.typing_momentum(t) - cat.momentum(t)).abs() < 1e-6,
            "the instances never diverge across a long run"
        );
    }
    assert!(cat.is_active(), "cat wake: the earned run summons");
    assert!(
        cat.frame(t).alpha > 0,
        "cat visibility: the earned run carries a live companion frame"
    );
}

#[test]
fn program_output_alone_builds_no_momentum() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((3, 0)), t, &c, g, &mut out);
    for col in 1..=20u16 {
        t += Duration::from_millis(40);
        glow.tick(Some((3, col)), t, &c, g, &mut out);
        assert!(
            glow.take_momentum_pulse().is_none(),
            "pure output pulses nothing (col {col})"
        );
    }
    assert_eq!(
        glow.typing_momentum(t),
        0.0,
        "program output alone earns zero momentum"
    );
    let mut typed = CursorGlow::default();
    let mut t2 = Instant::now();
    typed.tick(Some((3, 0)), t2, &c, g, &mut out);
    for col in 1..=20u16 {
        t2 += Duration::from_millis(40);
        typed.note_synthetic_typed(t2, 1);
        typed.tick(Some((3, col)), t2, &c, g, &mut out);
        assert!(
            typed.take_momentum_pulse() == Some(t2),
            "a correlated typed echo pulses at the echo instant (col {col})"
        );
    }
    assert!(
        typed.typing_momentum(t2) > 0.5,
        "real typing of the same cells earns momentum: {}",
        typed.typing_momentum(t2)
    );
}

#[test]
fn cursor_cat_motion_pulse_preserves_licensed_fold_shape() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    let mut ordinary = CursorGlow::default();
    ordinary.tick(Some((2, 5)), t0, &c, g, &mut out);
    let at = t0 + Duration::from_millis(20);
    ordinary.note_synthetic_typed(at, 1);
    ordinary.tick(Some((2, 6)), at, &c, g, &mut out);
    assert_eq!(
        ordinary.take_cursor_cat_motion_pulse(),
        Some(CursorCatMotionPulse {
            at,
            kind: CursorCatMotionKind::Advance,
        })
    );

    let mut next_row = CursorGlow::default();
    next_row.tick(Some((2, 38)), t0, &c, g, &mut out);
    next_row.note_synthetic_typed(at, 1);
    next_row.tick(Some((3, 0)), at, &c, g, &mut out);
    assert_eq!(
        next_row.take_cursor_cat_motion_pulse(),
        Some(CursorCatMotionPulse {
            at,
            kind: CursorCatMotionKind::FoldForward,
        })
    );

    let mut same_row_preview = CursorGlow::default();
    same_row_preview.tick(Some((5, 38)), t0, &c, g, &mut out);
    same_row_preview.note_synthetic_typed(at, 1);
    same_row_preview.tick(Some((5, 0)), at, &c, g, &mut out);
    assert!(
        same_row_preview.take_cursor_cat_motion_pulse().is_none(),
        "the park is held, not yet judged"
    );
    same_row_preview.tick(
        Some((5, 0)),
        at + Duration::from_millis(300),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        same_row_preview.take_cursor_cat_motion_pulse(),
        Some(CursorCatMotionPulse {
            at,
            kind: CursorCatMotionKind::FoldForward,
        }),
        "the explicit same-row reflow preview preserves the fold classifier"
    );

    let mut reverse = CursorGlow::default();
    let reverse_origin = (3, 0);
    let reverse_target = (2, 38);
    reverse.tick(Some(reverse_origin), t0, &c, g, &mut out);
    reverse.note_backspace(at);
    reverse.tick(Some(reverse_target), at, &c, g, &mut out);
    assert_eq!(
        reverse.take_cursor_cat_motion_pulse(),
        Some(CursorCatMotionPulse {
            at,
            kind: CursorCatMotionKind::FoldReverse,
        })
    );

    let mut nav = CursorGlow::default();
    nav.tick(Some((2, 38)), t0, &c, g, &mut out);
    nav.note_navigation(at);
    nav.tick(Some((3, 0)), at, &c, g, &mut out);
    assert!(
        nav.take_cursor_cat_motion_pulse().is_none(),
        "a navigation-licensed jump is not a typed fold"
    );
}

#[test]
fn composed_physical_margin_fold_keeps_ribbon_material_and_kitty_direction() {
    let mut g = geom();
    g.cols = 100;
    g.win_w = u16::try_from(g.cols.saturating_mul(g.cw)).unwrap_or(u16::MAX);
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = t0 + Duration::from_millis(20);
    let origin = (2, 51);
    let target = (3, 12);
    let mut out = Vec::new();

    let mut physical = CursorGlow::default();
    physical.note_pane_columns(12, 40);
    physical.tick(Some(origin), t0, &c, g, &mut out);
    physical.note_synthetic_typed(at, 1);
    physical.tick(Some(target), at, &c, g, &mut out);
    let lit: Vec<(u16, u16)> = (2..=3u16)
        .flat_map(|row| {
            v2_cols(&physical, row)
                .into_iter()
                .map(move |col| (row, col))
        })
        .collect();
    assert_eq!(
        lit,
        vec![origin],
        "only the final pane cell that received ink enters the ribbon"
    );
    assert_eq!(
        physical.take_cursor_cat_motion_pulse(),
        Some(CursorCatMotionPulse {
            at,
            kind: CursorCatMotionKind::FoldForward,
        })
    );

    let mut unwired = CursorGlow::default();
    unwired.tick(Some(origin), t0, &c, g, &mut out);
    unwired.note_synthetic_typed(at, 1);
    unwired.tick(Some(target), at, &c, g, &mut out);
    assert!(
        !v2_cols(&unwired, origin.0).contains(&origin.1),
        "without the pane morphology, generic re-anchor behavior cannot invent margin material"
    );
}

#[test]
fn composed_wide_wrap_folds_at_the_focused_pane_bounds() {
    let mut g = geom();
    g.cols = 100;
    g.win_w = u16::try_from(g.cols.saturating_mul(g.cw)).unwrap_or(u16::MAX);
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = t0 + Duration::from_millis(20);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();

    glow.note_pane_columns(12, 40);
    glow.tick(Some((2, 50)), t0, &c, g, &mut out);
    glow.note_synthetic_typed(at, 2);
    glow.tick(Some((3, 13)), at, &c, g, &mut out);

    let cells: Vec<(u16, u16)> = (2..=3u16)
        .flat_map(|row| v2_cols(&glow, row).into_iter().map(move |col| (row, col)))
        .collect();
    assert_eq!(
        cells,
        vec![(2, 51), (3, 12)],
        "the fold finishes and begins at the focused pane edges only (the caret's \
         own cell, (3, 13), is never a glyph cell)"
    );
    assert!(
        cells.iter().all(|(_, col)| (12..52).contains(col)),
        "no ribbon cell may leak into either neighboring pane"
    );
}
