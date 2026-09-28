// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared momentum and the composed-pane folds.

use super::*;

/// UNIFIED-READERS PROOF: ONE canonical metric ([`crate::typing_momentum`])
/// drives all three rainbow kitty "earned drama" consumer groups. The glow engine's
/// instance and the cursor cat's instance evolve IDENTICALLY under one
/// keystream (same law, same stamps), and each consumer keys off it:
/// (1) the star envelope/density via the eased `rainbow.disp` spine,
/// (2) the fresh-ink pop via the spine frozen at birth (`InkPop::mom`),
/// (3) the one-body cat wake via its band+dwell over the same value.
#[test]
fn momentum_unifies_glow_and_cat_metrics() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut cat = crate::kitty_cursor::CursorCat::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((3, 0)), t, &c, g, &mut out);
    // Phase 1 — a casual word: the two instances read the SAME value at
    // every stamp, and every consumer agrees it is unearned.
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
    // RETUNED 2026-08-29: ten keys at human cadence is a WORD, and a word
    // now earns its air — the star onset moved to ~one word so the
    // starfield appears in ordinary typing (owner: "where are the
    // sparkles?"). What this phase still pins is the LAW the two engines
    // share: the momentum value agrees at every stamp above, and the CAT
    // — whose earn bar is its own, higher one — stays unearned here.
    assert!(!cat.is_active(), "cat wake: unearned ⇒ no cat");
    assert_eq!(
        cat.frame(t).alpha,
        0,
        "cat visibility: an unearned run has no live companion frame"
    );
    // A delete drains BOTH instances identically (never builds).
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
    // Phase 2 — sustained flow (~6.6 s): the shared value crosses the
    // star onset and then the (raised 0.90) cat band, so the shower AND the
    // one-body cat are earned together off the one metric. The cat's earn law
    // was raised (band 0.90, dwell 3.0 s in kitty_cursor.rs) so this run must
    // be longer than the old ~4.5 s to summon. Line ends continue via the
    // wrap-shaped move, which classifies as typing — every key builds BOTH
    // instances.
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
        // A real keystroke arms the committed-press hint the correlated
        // advance now REQUIRES (M1): forward/wrap echoes credit momentum
        // only when paired with it, so a program printing the same cells
        // would build nothing here.
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

/// EARNED BY REAL TYPING ONLY (M1): a program printing one glyph per frame
/// walks the caret forward +1 col every tick — the exact `forward` shape —
/// yet arms NO key-time press hint, so the correlated advance never fires:
/// the metric stays flat at zero and no momentum pulse is ever offered to
/// the cat. The same forward moves WITH a real press behind them build
/// normally, so this is the correlation gate, not a dead advance. (The
/// advance keys on the PRESS-HINT half — v0.43.0 law — not on candidate
/// admission; see `classify_move`'s momentum comment and
/// `momentum_climbs_through_streaming_collisions_that_retire_every_candidate`.)
#[test]
fn program_output_alone_builds_no_momentum() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((3, 0)), t, &c, g, &mut out);
    // 20 forward advances as PURE OUTPUT — no `note_typed` behind any of
    // them (a `printf` loop, a spinner, a streaming log).
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
    // Flip on explicit correlation: the identical forward shape now
    // builds, proving the gate is provenance, not geometry.
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

/// Appendix-A seam: the classifier already knows whether a licensed
/// cursor move is a fold. The cursor-cat channel must preserve that verdict
/// for the ordinary next-row shape, an explicit preview-only same-row
/// reflow projection, and the exact Backspace inverse — while a
/// navigation-shaped relocation mints nothing. Shipping bottom-scroll
/// reachability is bound by the real-App ContentScrollState test in
/// `app_input`; this synthetic classifier test makes no such claim.
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
    // A same-row backward typed-paired move is HELD for its return
    // (the Ink rewrite's park, 2026-09-12) and judged — the fold pulse
    // included, dated at the move — on the next tick past the window.
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
