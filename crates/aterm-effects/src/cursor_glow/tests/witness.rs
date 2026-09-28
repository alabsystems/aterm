// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ribbon's witness rows and the exact-key credit ring.

use super::*;

/// The stars read the flanking rows through v2's occupancy bitsets
/// alone. The engine's own copies of those rows ([`NbrProbe`]) were
/// dropped for that reason and came back for the seam's content
/// witnesses; this pins that the stars never needed them.
#[test]
fn neighbor_probe_still_gates_star_pixels_after_discarding_duplicate_row_storage() {
    let now = Instant::now();
    let cfg = cfg_for_style_name("rainbow kitty", true);
    let geom = retina_geom();
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    glow.tick(Some((3, 0)), now, &cfg, geom, &mut quads);
    assert!(glow.v2.engaged(), "rainbow kitty owns the next probe");

    glow.observe_row(3, 1, &[' '; 40], now + ms(16));
    glow.observe_neighbor_rows(Some(&['─', 'X', '═']), Some(&[' ', 'Y']));
    let sky = glow.v2.probe();
    assert_eq!(sky.at(2, 0, geom.rows), Some(true));
    assert_eq!(sky.sky_at(2, 0, geom.rows), Some(false));
    assert_eq!(sky.sky_at(2, 1, geom.rows), Some(true));
    assert_eq!(sky.sky_at(2, 2, geom.rows), Some(true));
    assert_eq!(sky.at(4, 0, geom.rows), Some(false));
    assert_eq!(sky.at(4, 1, geom.rows), Some(true));
}

#[test]
fn far_witness_row_fills_its_engine_slot_once_without_a_second_copy() {
    let now = Instant::now();
    let cfg = cfg_for_style_name("rainbow kitty", true);
    let mut glow = CursorGlow::default();
    glow.tick(Some((3, 0)), now, &cfg, retina_geom(), &mut Vec::new());
    assert!(glow.v2.engaged());

    let glyphs = ['A', '界', '\0', ' ', 'Z'];
    let mut fills = 0;
    let mut filled_ptr = std::ptr::null();
    glow.capture_ribbon_row(3, |slot| {
        fills += 1;
        slot.extend_from_slice(&glyphs);
        filled_ptr = slot.as_ptr();
    });
    assert_eq!(fills, 1, "one far row takes exactly one terminal read");
    assert_eq!(glow.witness_rows_n, 1);
    assert_eq!(glow.witness_rows[0].cols, glyphs);
    assert!(
        std::ptr::eq(glow.witness_rows[0].cols.as_ptr(), filled_ptr),
        "the terminal filled the engine's own buffer, not a copied host row"
    );

    let mut term = aterm_core::terminal::Terminal::new(6, 8);
    term.process("\x1b[4;1HA界B".as_bytes());
    glow.capture_ribbon_row(3, |slot| {
        term.row_cols_into(3, slot);
    });
    assert_eq!(
        &glow.witness_rows[0].cols[..4],
        &['A', '界', '\0', 'B'],
        "the direct slot preserves the terminal's wide-cell projection"
    );

    glow.capture_ribbon_row(3, |slot| slot.extend(['B', '\0']));
    assert_eq!(glow.witness_rows_n, 1, "a repeated row replaces its slot");
    assert_eq!(glow.witness_rows[0].cols, ['B', '\0']);

    let mut dark = CursorGlow::default();
    dark.capture_ribbon_row(3, |_| panic!("a dark engine must not read a row"));
    assert_eq!(dark.witness_rows_n, 0);
}

#[test]
fn full_ribbon_row_budget_keeps_pending_source_and_caret_samples() {
    let now = Instant::now();
    let glow_cfg = cfg_for_style_name("rainbow kitty", true);
    let cfg = rk::Config::from_glow(&glow_cfg, false);
    let geom = Geom {
        rows: 24,
        win_h: 24 * u16::try_from(retina_geom().ch).unwrap(),
        ..retina_geom()
    };
    let mut glow = CursorGlow::default();
    glow.v2.set_engaged(true);
    let (mut under, mut out, mut halos, mut beams, mut cues) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut tick = |glow: &mut CursorGlow, at| {
        let mut frame = rk::Frame {
            under: &mut under,
            out: &mut out,
            halos: &mut halos,
            beams: &mut beams,
            cues: &mut cues,
            caret: rk::CaretSeam::default(),
            companion: None,
            fp: 0,
        };
        glow.v2.tick(at, geom, &cfg, &mut frame);
    };

    // Three separated live runs request exactly eight distinct rows:
    // the current run, its two follow destinations, two older run rows,
    // and three of their follow destinations. None may be traded for a
    // pending key's source when the fixed ribbon budget fills.
    glow.v2.on_event(
        rk::Event::Move {
            from: (3, 0),
            to: (3, 4),
            licence: rk::Licence::Typed,
            dir: rk::Dir::Right,
        },
        now,
    );
    tick(&mut glow, now);
    for (i, row) in [3u16, 5, 10].into_iter().enumerate() {
        let at = now + ms(i as u64 + 1);
        if row != 3 {
            glow.v2.observe_caret((row, 4));
            tick(&mut glow, at);
        }
        glow.v2.on_event(
            rk::Event::Sweep {
                row,
                col0: 2,
                col1: 4,
            },
            at,
        );
        tick(&mut glow, at);
    }
    let mut engine_rows = [0u16; rk::witness::WITNESS_ROWS];
    assert_eq!(
        glow.v2.ribbon_rows_for(None, &mut engine_rows),
        rk::witness::WITNESS_ROWS,
        "the ribbon really fills all eight content slots"
    );

    // A temporarily parked caret can be outside every ribbon row and
    // the unspent press can originate on a third row. The host samples
    // the caret first, then the engine's list: the bands' eight rows,
    // then the source and the rows one above and below it (its arming
    // rows). All twelve rows must reach the same-frame witness and the
    // source-prefix classifier.
    let caret_row = 18;
    let source_row = 20;
    glow.last = Some((source_row, 4));
    glow.type_hint.stamp(now);
    let mut short = [0u16; rk::witness::WITNESS_ROWS];
    assert_eq!(glow.ribbon_rows(&mut short), rk::witness::WITNESS_ROWS);
    assert_eq!(
        short, engine_rows,
        "a short caller must fail closed, never evict a content row"
    );
    let mut wanted = [0u16; CURSOR_WITNESS_ROWS];
    let n = glow.ribbon_rows(&mut wanted);
    assert_eq!(n, rk::witness::WITNESS_ROWS + rk::witness::ARMING_ROWS);
    assert_eq!(&wanted[..engine_rows.len()], &engine_rows);
    assert_eq!(
        wanted[engine_rows.len()..n],
        [source_row, source_row - 1, source_row + 1],
        "the source leads its arming rows"
    );

    glow.observe_ribbon_row(caret_row, &['c']);
    for &row in &wanted[..n] {
        glow.observe_ribbon_row(row, &['s']);
    }
    assert_eq!(glow.witness_rows_n, CURSOR_WITNESS_ROWS);
    assert!(
        glow.witness_rows[..glow.witness_rows_n]
            .iter()
            .any(|sample| sample.row == source_row),
        "the pending key's source must be present for the cross-row proof"
    );
    assert!(
        engine_rows
            .iter()
            .all(|row| glow.witness_rows[..glow.witness_rows_n]
                .iter()
                .any(|sample| sample.row == *row)),
        "sourcing a pending key must not discard any ribbon content row"
    );
}

#[test]
fn exact_coalesced_keys_retire_older_space_but_refuse_stale_or_interleaved_runs() {
    let t0 = Instant::now();
    let mut keys = PressCredits::default();
    keys.bank(t0, 1, Some(' ')); // Earlier wrap-fill, already seen blank.
    for (dt, ch) in [(600, 'a'), (688, 'n'), (771, 'd'), (850, ' ')] {
        keys.bank(t0 + ms(dt), 1, Some(ch));
    }
    let now = t0 + ms(950);
    let selected = keys
        .exact_unpaid_run(now, t0 + ms(500), &['a', 'n', 'd', ' '])
        .expect("later exact phrase has a fresh tail");
    keys.retire_before_slot(selected);
    assert_eq!(
        keys.cells_within(now),
        4,
        "old wrap-fill credit was retired"
    );
    assert_eq!(keys.spend_counting(now, 4), 4);
    assert!(
        keys.exact_unpaid_run(now, t0 + ms(500), &['a', 'n', 'd', ' '])
            .is_none(),
        "keyless later paint cannot reuse the spent phrase"
    );

    let mut stale = PressCredits::default();
    for (dt, ch) in [(0, 'a'), (80, 'n'), (160, 'd'), (240, ' '), (990, 'x')] {
        stale.bank(t0 + ms(dt), 1, Some(ch));
    }
    assert!(
        stale
            .exact_unpaid_run(t0 + ms(1000), t0 + ms(500), &['a', 'n', 'd', ' '])
            .is_none(),
        "fresh unrelated key cannot fund an old exact phrase"
    );

    let mut interleaved = PressCredits::default();
    for (dt, ch) in [
        (0, ' '),
        (600, 'a'),
        (688, 'x'),
        (771, 'n'),
        (850, 'd'),
        (900, ' '),
    ] {
        interleaved.bank(t0 + ms(dt), 1, Some(ch));
    }
    assert!(
        interleaved
            .exact_unpaid_run(t0 + ms(950), t0 + ms(500), &['a', 'n', 'd', ' '])
            .is_none(),
        "no live mismatched credit may be skipped within the selected run"
    );

    let mut repeated_a = PressCredits::default();
    for (dt, ch) in [(0, 'a'), (600, 'a'), (688, 'n'), (771, 'd'), (850, ' ')] {
        repeated_a.bank(t0 + ms(dt), 1, Some(ch));
    }
    let selected = repeated_a
        .exact_unpaid_run(t0 + ms(950), t0 + ms(500), &['a', 'n', 'd', ' '])
        .expect("a stale earlier a must not hide the complete later run");
    repeated_a.retire_before_slot(selected);
    assert_eq!(repeated_a.cells_within(t0 + ms(950)), 4);
}

#[test]
fn two_orphan_exact_keys_keep_press_order_across_the_credit_ring_wrap() {
    let t0 = Instant::now();
    let mut credits = PressCredits::default();
    for i in 0..TYPED_STAMP_DEPTH - 1 {
        credits.bank(t0 + ms(i as u64), 1, Some('x'));
    }
    // The first eligible key occupies the last physical slot and the
    // second the first. Storage order would reverse their exact glyphs.
    credits.bank(t0 + ms(200), 1, Some('k'));
    credits.bank(t0 + ms(201), 1, Some('m'));
    let (keys, n) = credits
        .exact_run_between(t0 + ms(300), t0 + ms(199), t0 + ms(202))
        .expect("two delivered one-cell keys remain within the patience");
    assert_eq!(n, 2);
    assert_eq!(keys, [Some((t0 + ms(200), 'k')), Some((t0 + ms(201), 'm'))]);

    credits.bank(t0 + ms(202), 1, Some('q'));
    assert!(
        credits
            .exact_run_between(t0 + ms(300), t0 + ms(199), t0 + ms(203))
            .is_none(),
        "a third ambiguous key cannot escape into generic licensing"
    );
}

#[test]
fn delayed_single_exact_key_skips_only_a_prior_stale_credit() {
    let t0 = Instant::now();
    let now = t0 + ms(1_100);
    let prior_probe = t0 + ms(500);
    let mut keys = PressCredits::default();
    keys.bank(t0, 1, Some(' '));
    keys.bank(t0 + ms(600), 1, Some('a'));
    let selected = keys
        .exact_unpaid_run_with_tail_window(now, prior_probe, &['a'], IN_FLIGHT_PATIENCE_S)
        .expect("the delayed exact key follows a stale, already-probed space");
    assert!(
        keys.exact_unpaid_run(now, prior_probe, &['a']).is_none(),
        "the moving-prefix lane retains its short tail window"
    );
    assert!(
        keys.exact_unpaid_run_with_tail_window(now, prior_probe, &['x'], IN_FLIGHT_PATIENCE_S)
            .is_none(),
        "an ambient other glyph cannot spend the key"
    );
    keys.retire_before_slot(selected);
    assert_eq!(keys.cells_within(now), 1, "the older space was retired");
    assert_eq!(keys.spend_counting(now, 1), 1);
    assert!(
        keys.exact_unpaid_run_with_tail_window(now, prior_probe, &['a'], IN_FLIGHT_PATIENCE_S)
            .is_none(),
        "a keyless repaint cannot reuse the spent key"
    );

    let mut unproven_prefix = PressCredits::default();
    unproven_prefix.bank(t0 + ms(550), 1, Some(' '));
    unproven_prefix.bank(t0 + ms(600), 1, Some('a'));
    assert!(
        unproven_prefix
            .exact_unpaid_run_with_tail_window(now, prior_probe, &['a'], IN_FLIGHT_PATIENCE_S,)
            .is_none(),
        "a credit newer than the prior probe cannot be skipped"
    );
    assert!(
        unproven_prefix
            .exact_unpaid_run_with_tail_window(
                t0 + ms(10_601),
                prior_probe,
                &['a'],
                IN_FLIGHT_PATIENCE_S,
            )
            .is_none(),
        "the exact key is bounded by the in-flight patience"
    );
}

#[test]
fn exact_coalesced_run_of_32_keys_remains_one_shot_and_exact() {
    let t0 = Instant::now();
    let now = t0 + ms(1300);
    let previous_probe = t0 + ms(500);
    let glyphs: Vec<char> = "abcdefghijklmnopqrstuvwxyzABCDEF".chars().collect();
    assert_eq!(glyphs.len(), 32);
    let mut keys = PressCredits::default();
    keys.bank(t0 + ms(100), 1, Some(' ')); // Old wrap-fill, already blank on glass.
    for (i, &glyph) in glyphs.iter().enumerate() {
        keys.bank(t0 + ms(600 + i as u64 * 20), 1, Some(glyph));
    }
    let forged = keys;
    let selected = keys
        .exact_unpaid_run(now, previous_probe, &glyphs)
        .expect("a 32-cell exact run with a fresh tail has one first glyph");
    keys.retire_before_slot(selected);
    assert_eq!(keys.cells_within(now), glyphs.len());
    assert_eq!(keys.spend_counting(now, glyphs.len()), glyphs.len());
    assert!(
        keys.exact_unpaid_run(now, previous_probe, &glyphs)
            .is_none(),
        "a later keyless paint cannot spend the phrase again"
    );

    let mut wrong = glyphs.clone();
    wrong[16] = '!';
    assert!(
        forged
            .exact_unpaid_run(now, previous_probe, &wrong)
            .is_none(),
        "one changed glyph inside the larger run must refuse the whole proof"
    );
    assert!(
        forged
            .exact_unpaid_run(t0 + ms(1600), previous_probe, &glyphs)
            .is_none(),
        "an old phrase cannot borrow a later fresh key's clock"
    );
    let mut too_long = glyphs.clone();
    too_long.resize(EXACT_COALESCED_PREFIX_MAX + 1, '!');
    assert!(
        forged
            .exact_unpaid_run(now, previous_probe, &too_long)
            .is_none(),
        "the exceptional proof still refuses a run wider than its fixed cap"
    );

    let mut interleaved = PressCredits::default();
    for (i, &glyph) in glyphs.iter().enumerate() {
        interleaved.bank(t0 + ms(600 + i as u64 * 20), 1, Some(glyph));
        if i == 12 {
            interleaved.bank(t0 + ms(600 + i as u64 * 20 + 10), 1, Some('!'));
        }
    }
    assert!(
        interleaved
            .exact_unpaid_run(now, previous_probe, &glyphs)
            .is_none(),
        "an unrelated key inside the delayed batch cannot be skipped"
    );
}

#[test]
fn swept_path_retains_only_the_bounded_landing_suffix() {
    let mut path = Vec::new();
    line_cells_tail(&mut path, (0, 0), (0, 5), 3, false);
    assert_eq!(path, [(0, 2), (0, 3), (0, 4)]);

    line_cells_tail(&mut path, (0, 0), (65_535, 65_535), 512, false);
    assert_eq!(path.len(), 512);
    assert_eq!(path.first(), Some(&(65_023, 65_023)));
    assert_eq!(path.last(), Some(&(65_534, 65_534)));
    assert!(
        path.capacity() <= CursorGlow::MAX_SPARKS,
        "outlier jump scratch remains resident-cap bounded"
    );
}
