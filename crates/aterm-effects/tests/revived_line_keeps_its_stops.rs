// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A line revived after a rest wears the stops it had.
//!
//! The take: a shell line typed at 12 cps, a rest past the swoosh (every cell gone, inside the
//! five-second chain window), then a key at the line's end. The seam's typed-run revive
//! (`CursorGlow::hand_v2_typed_sweep`) re-lays the retired run on the key's frame; every revived
//! column takes back its stop (C2, `Cohort::anchor_col`) and the key's cell continues the walk
//! one step past the line's last cell. Left alone, the re-lay would mint a cohort continuing
//! `last_walk` at the run's first column, shifting every column `len` stops on. How it holds:
//!
//! * a cell laid beside a run still lit on its row joins that run's walk by position
//!   (`Ribbon::join_cohort`);
//! * when the key's echo lands a frame late the key is held and the re-lay goes first, carrying
//!   the walk origin read off the run's standing cells (`RecentTypedRun::origin`,
//!   `ribbon::Revive`);
//! * a run no later key saw standing has its walk read on the frame that lays it
//!   (`CursorGlow::read_fresh_run_origin`);
//! * a typed fold's landing is remembered as the row's typed run
//!   (`CursorGlow::remember_fold_landing`), so the revive re-lays the fold glyph too.
//!
//! Folds are read behind each shape `walk_census`'s [`Fold`] models.

use crate::walk_census::*;
use aterm_effects::rainbow_kitty::ribbon::{CHAIN_GAP_MAX, WALK_LAY_RATE, walk_t};

/// The line and the rest.
const LINE_CELLS: usize = 30;
const REST_MS: u64 = 2_500;
/// The frames after the key's echo the reading waits (the edge-in).
const SETTLE_MS: u64 = 48;

/// The last snap's `t` per text cell (`None` dark).
fn stops(s: &Shell) -> Vec<Option<f32>> {
    s.snaps
        .last()
        .expect("a frame")
        .pos
        .iter()
        .map(|p| p.lit.then_some(p.t).flatten())
        .collect()
}

/// `n` cells typed, the band read settled; returns the shell and the stops.
fn typed(n: usize) -> (Shell, Vec<Option<f32>>) {
    typed_echo(n, Echo::After(2))
}

/// [`typed`] behind a shell whose echo lands `echo` after each press.
fn typed_echo(n: usize, echo: Echo) -> (Shell, Vec<Option<f32>>) {
    typed_shaped(n, echo, Fold::Readline)
}

/// [`typed_echo`] behind a shell that folds its line as `fold` does.
fn typed_shaped(n: usize, echo: Echo, fold: Fold) -> (Shell, Vec<Option<f32>>) {
    let mut s = Shell::new(60, 83, echo).folding(fold);
    s.type_str(&prose(n));
    s.idle(SETTLE_MS);
    let before = stops(&s);
    (s, before)
}

/// The premise every take stands on: the line was laid on a cold walk —
/// every lit cell at `walk_t(i)`, the `lit` range lit.
fn assert_cold_walk(before: &[Option<f32>], lit: std::ops::Range<usize>, what: &str) {
    for i in lit {
        let t = before[i].unwrap_or_else(|| panic!("{what}: text {i} is lit before the rest"));
        assert!(
            (t - walk_t(i as f32)).abs() < 1e-4,
            "{what}: the premise — text {i} was laid at walk_t({i}) = {}, read {t}",
            walk_t(i as f32)
        );
    }
}

/// The rest: every text cell dark and the ribbon empty at its end, inside
/// the chain window.
fn rest(s: &mut Shell, what: &str) {
    s.idle(REST_MS);
    let dark = s.snaps.last().expect("a frame").pos.iter().all(|p| !p.lit);
    let empty =
        s.h.glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .is_empty();
    assert!(
        dark && empty,
        "{what}: the premise — the swoosh has finished before the next key \
         (dark={dark}, no cell resident={empty})"
    );
    assert!(
        ((REST_MS + SETTLE_MS) as f32 / 1000.0) < CHAIN_GAP_MAX,
        "{what}: the premise — the key is inside the chain window"
    );
}

/// One take: `n` cells, the rest, one key at the line's end; the stops
/// before the rest and after the key.
fn revived(
    n: usize,
    row: std::ops::Range<usize>,
    what: &str,
) -> (Shell, Vec<Option<f32>>, Vec<Option<f32>>) {
    revived_echo(n, row, what, Echo::After(2))
}

/// [`revived`] behind a shell whose echo lands `echo` after each press.
fn revived_echo(
    n: usize,
    row: std::ops::Range<usize>,
    what: &str,
    echo: Echo,
) -> (Shell, Vec<Option<f32>>, Vec<Option<f32>>) {
    revived_shaped(n, row, what, echo, Fold::Readline)
}

/// [`revived_echo`] behind a shell that folds its line as `fold` does.
fn revived_shaped(
    n: usize,
    row: std::ops::Range<usize>,
    what: &str,
    echo: Echo,
    fold: Fold,
) -> (Shell, Vec<Option<f32>>, Vec<Option<f32>>) {
    let (mut s, before) = typed_shaped(n, echo, fold);
    // The premise on the row the revive re-lays (a wrapped line's first row
    // has already flowed into its fold by the last key).
    assert_cold_walk(&before, row, what);
    rest(&mut s, what);
    s.key(' ');
    s.idle(SETTLE_MS);
    let after = stops(&s);
    (s, before, after)
}

/// The colour law for one revived take: every lit column of `row` kept its
/// stop, and the key's cell (text `n`) is `walk_t(n)`, one step past the
/// line's last cell on the cold walk ([`assert_cold_walk`]). `None` when it
/// holds, else what broke.
fn stops_kept(
    s: &Shell,
    n: usize,
    row: std::ops::Range<usize>,
    before: &[Option<f32>],
    after: &[Option<f32>],
    what: &str,
) -> Option<String> {
    let relit = row.clone().filter(|&i| after[i].is_some()).count();
    let moved: Vec<(usize, f32, f32)> = row
        .clone()
        .filter_map(|i| {
            let (b, a) = (before[i]?, after[i]?);
            ((a - b).abs() > 1e-4).then_some((i, b, a))
        })
        .collect();
    let key = after[n].expect("the key's own cell is lit");
    assert!(
        before[n - 1].is_some(),
        "{what}: the premise — the line's last cell was lit"
    );
    let want = walk_t(n as f32);
    (!moved.is_empty() || (key - want).abs() >= 1e-4).then(|| {
        format!(
            "{what}: the revived line changed colour under unchanged text — {} of {} lit \
             cells moved, first (text, before, after) {:?}; the key's cell is {key:.4} where \
             the walk one step past the line's last cell is {want:.4}\n{}",
            moved.len(),
            relit,
            moved.first(),
            s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
        )
    })
}

/// A line of 80 cells behind the `$ ` prompt at 60 columns: its second row
/// holds text `58..80`, the fold's own glyph first.
const WRAPPED: usize = 80;
const ROW2: std::ops::Range<usize> = 58..80;

/// The colour law: after a rest past the swoosh, the key typed at the line's end
/// brings the line back in its stops, and its own cell is the walk one step past
/// the line's last cell, on one row and on a wrapped line's second row.
#[test]
fn a_line_revived_after_a_rest_keeps_its_stops_and_the_key_continues_its_walk() {
    let mut broken = Vec::new();
    for (what, n, row) in [
        ("one row", LINE_CELLS, 0..LINE_CELLS),
        ("a wrapped line's second row", WRAPPED, ROW2),
    ] {
        let (s, before, after) = revived(n, row.clone(), what);
        // Premise: the revive fired and the whole row is lit again on the key's
        // frame ([`stops_kept`] compares only cells lit before and after, so a
        // dark cell would slip past it).
        let relit = row.clone().filter(|&i| after[i].is_some()).count();
        assert!(
            relit == row.len(),
            "{what}: the premise — the seam revived the retired run on the key's frame \
             ({relit} of {} lit)",
            row.len()
        );
        let moved: Vec<(usize, f32, f32)> = row
            .clone()
            .filter_map(|i| {
                let (b, a) = (before[i]?, after[i]?);
                ((a - b).abs() > 1e-4).then_some((i, b, a))
            })
            .collect();
        let key = after[n].expect("the key's own cell is lit");
        let want = before[n - 1].expect("the line's last cell was lit") + WALK_LAY_RATE;
        if !moved.is_empty() || (key - want).abs() >= 1e-4 {
            broken.push(format!(
                "{what}: the revived line changed colour under unchanged text — {} of {} \
                 lit cells moved, first (text, before, after) {:?}; the key's cell is \
                 {key:.4} where the walk one step past the line's last cell is {want:.4}\n{}",
                moved.len(),
                relit,
                moved.first(),
                s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
            ));
        }
    }
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

/// Echo lag of a shell whose echo lands on the frame after its key's: longer
/// than one 16 ms frame, so a frame always presents the press alone first.
const LATE_ECHO_MS: u64 = 20;
/// The frame train's period.
const FRAME_MS: u64 = 16;

/// The same law when the key's echo lands a frame late: the engine holds the
/// key (`Ribbon::hold_typed`), the re-lay mints with no run lit on its row, so
/// the position law cannot place it and the seam's named revive must.
#[test]
fn a_line_revived_when_the_key_s_echo_lands_a_frame_late_keeps_its_stops() {
    let mut broken = Vec::new();
    for (what, n, row, fold) in [
        (
            "one row, echo a frame late",
            LINE_CELLS,
            0..LINE_CELLS,
            Fold::Readline,
        ),
        (
            "a wrapped line's second row, echo a frame late",
            WRAPPED,
            ROW2,
            Fold::Readline,
        ),
        // The fold glyph laid by the terminal's own wrap: the revive re-lays the
        // continuation row from column 0 behind a held key too.
        (
            "a wrapped line's second row, kernel echo a frame late",
            WRAPPED,
            ROW2,
            Fold::Kernel,
        ),
    ] {
        let (mut s, before) = typed_shaped(n, Echo::After(LATE_ECHO_MS), fold);
        assert_cold_walk(&before, row.clone(), what);
        rest(&mut s, what);
        let press_ms = s.h.ms() + s.key_ms;
        s.key(' ');
        s.idle(SETTLE_MS);
        let after = stops(&s);
        // The premise: the key was held — a frame presented between its
        // press and its echo, the line's text not yet grown.
        let held = s
            .snaps
            .iter()
            .filter(|f| {
                f.ms > press_ms && f.ms <= press_ms + LATE_ECHO_MS && f.text.chars().count() == n
            })
            .count();
        assert!(
            held > 0,
            "{what}: the premise — a frame presented the press before its echo"
        );
        // Premise: the revive fired and the whole row is lit again, the fold
        // glyph included ([`stops_kept`] alone compares only cells lit before
        // and after).
        let relit = row.clone().filter(|&i| after[i].is_some()).count();
        assert!(
            relit == row.len(),
            "{what}: the premise — the seam revived the retired run on the echo's frame \
             ({relit} of {} lit)",
            row.len()
        );
        broken.extend(stops_kept(&s, n, row, &before, &after, what));
    }
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

/// A run no key saw standing (one key; the kernel fold glyph alone, or two or
/// three under a coalesced fold) has its walk read on the frame that lays it
/// (`CursorGlow::read_fresh_run_origin`), so it comes back on its stops even
/// behind a frame-late echo, where the re-lay is laid before the key's cell.
#[test]
fn a_run_no_key_saw_standing_comes_back_on_its_own_walk() {
    // (what, line, the run's text range, fold, echo ms, coalesced fold)
    let takes = [
        ("one key", 1, 0..1, Fold::Readline, 2, false),
        (
            "one key, echo a frame late",
            1,
            0..1,
            Fold::Readline,
            LATE_ECHO_MS,
            false,
        ),
        (
            "the kernel echo's fold glyph alone",
            59,
            58..59,
            Fold::Kernel,
            2,
            false,
        ),
        (
            "the kernel echo's fold glyph alone, echo a frame late",
            59,
            58..59,
            Fold::Kernel,
            LATE_ECHO_MS,
            false,
        ),
        // Readline folds on the key that fills the margin: text 58 is the
        // continuation row's first key, a run of one.
        (
            "readline's first key on the continuation row, echo a frame late",
            59,
            58..59,
            Fold::Readline,
            LATE_ECHO_MS,
            false,
        ),
        (
            "a coalesced kernel-echo fold",
            60,
            58..60,
            Fold::Kernel,
            2,
            true,
        ),
        (
            "a coalesced kernel-echo fold, echo a frame late",
            60,
            58..60,
            Fold::Kernel,
            LATE_ECHO_MS,
            true,
        ),
    ];
    let mut broken = Vec::new();
    for (what, n, row, fold, echo, coalesced) in takes {
        let mut s = Shell::new(60, 83, Echo::After(echo)).folding(fold);
        let line = prose(n);
        if coalesced {
            // Three keys 3 ms apart across the margin, each echoed 40 ms
            // late: their echoes land in one frame.
            let (a, b) = line.split_at(n - 3);
            s.type_str(a);
            s.type_lagged(b, 3, 40);
            s.idle(40);
        } else {
            s.type_str(&line);
        }
        s.idle(SETTLE_MS);
        let before = stops(&s);
        assert_cold_walk(&before, row.clone(), what);
        rest(&mut s, what);
        let press_ms = s.h.ms() + s.key_ms;
        s.key(' ');
        s.idle(SETTLE_MS);
        let after = stops(&s);
        if echo > FRAME_MS {
            let held = s
                .snaps
                .iter()
                .filter(|f| {
                    f.ms > press_ms && f.ms <= press_ms + echo && f.text.chars().count() == n
                })
                .count();
            assert!(
                held > 0,
                "{what}: the premise — a frame presented the press before its echo"
            );
        }
        let dark: Vec<usize> = row.clone().filter(|&i| after[i].is_none()).collect();
        if !dark.is_empty() {
            broken.push(format!(
                "{what}: the revived run is dark under text {dark:?}\n{}",
                s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
            ));
        }
        broken.extend(stops_kept(&s, n, row, &before, &after, what));
    }
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

/// No hole at the fold. Behind the kernel's cooked-mode echo ([`Fold::Kernel`]) the glyph after
/// the margin is written at the next row's column 0 by the terminal's wrap, and that landing is
/// remembered as the row's typed run; readline and zle fold on the key that fills the margin, so
/// their run already starts at column 0. Each shape behind a 2 ms echo and a frame-late one.
/// Control: the single-row line.
#[test]
fn a_wrapped_line_revived_after_a_rest_lights_its_second_row_from_its_first_glyph() {
    let (_, _, after) = revived(LINE_CELLS, 0..LINE_CELLS, "one row");
    let dark: Vec<usize> = (0..LINE_CELLS).filter(|&i| after[i].is_none()).collect();
    assert!(
        dark.is_empty(),
        "the control: a one-row line comes back whole, dark {dark:?}"
    );
    let mut broken = Vec::new();
    for (what, fold, echo) in [
        ("kernel echo", Fold::Kernel, Echo::After(2)),
        ("readline", Fold::Readline, Echo::After(2)),
        ("zle", Fold::Zle, Echo::After(2)),
        // …and each behind an echo a frame late (§43: the key held, the
        // revive's re-lay laid before its cell).
        (
            "kernel echo a frame late",
            Fold::Kernel,
            Echo::After(LATE_ECHO_MS),
        ),
        (
            "readline, echo a frame late",
            Fold::Readline,
            Echo::After(LATE_ECHO_MS),
        ),
        (
            "zle, echo a frame late",
            Fold::Zle,
            Echo::After(LATE_ECHO_MS),
        ),
    ] {
        let what = format!("a wrapped line's second row, {what}");
        let what = what.as_str();
        let (s, before, after) = revived_shaped(WRAPPED, ROW2, what, echo, fold);
        let dark: Vec<usize> = ROW2.filter(|&i| after[i].is_none()).collect();
        if !dark.is_empty() {
            broken.push(format!(
                "{what}: the revived second row is dark under text {dark:?} (text 58 is the \
                 row's column 0, the fold's own glyph)\n{}",
                s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
            ));
        }
        broken.extend(stops_kept(&s, WRAPPED, ROW2, &before, &after, what));
    }
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

/// Controls: the same key inside the grace (band live) keeps every stop and
/// continues the walk; after the same rest with the prefix's first glyph
/// rewritten by the program (no revive) the key's cell is exactly the walk
/// continued one step past the line's last cell, and the prefix stays dark.
#[test]
fn without_the_revive_the_key_continues_the_walk_and_no_stop_moves() {
    // Inside the grace.
    let (mut s, before) = typed(LINE_CELLS);
    assert_cold_walk(&before, 0..LINE_CELLS, "grace");
    s.idle(600);
    s.key(' ');
    s.idle(SETTLE_MS);
    let after = stops(&s);
    for i in 0..LINE_CELLS {
        let (b, a) = (before[i].expect("lit"), after[i].expect("still lit"));
        assert!(
            (a - b).abs() < 1e-4,
            "grace: text {i} kept its stop ({b} -> {a})"
        );
    }
    let key = after[LINE_CELLS].expect("the key's cell is lit");
    let want = before[LINE_CELLS - 1].expect("lit") + WALK_LAY_RATE;
    assert!(
        (key - want).abs() < 1e-4,
        "grace: the key continues the walk ({key} against {want})"
    );

    // Past the swoosh, the prefix's first glyph rewritten during the rest.
    let (mut s, before) = typed(LINE_CELLS);
    s.idle(REST_MS / 2);
    s.h.term.process(b"\x1b[3;3HT\x1b[3;33H");
    s.text[0] = 'T';
    rest(&mut s, "rewritten");
    s.key(' ');
    s.idle(SETTLE_MS);
    let after = stops(&s);
    assert!(
        after[..LINE_CELLS].iter().all(Option::is_none),
        "rewritten: the premise — no revive, the prefix stays dark: {}",
        s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
    );
    let key = after[LINE_CELLS].expect("the key's cell is lit");
    let want = before[LINE_CELLS - 1].expect("lit") + WALK_LAY_RATE;
    assert!(
        (key - want).abs() < 1e-4,
        "rewritten: the rebirth continues the retired walk one step past its last \
         cell ({key} against {want})"
    );
}

/// Claude Code's composer from real bytes
/// (`fixtures/claude-wrap-2026-09-23-end.ptylog`: v2.1.280, 40×120, a line typed
/// at 8–12 cps, the box growing a row at `[image`), replayed at the host seam
/// with `rest_ms` of rest before the 62nd key (`however`, 62 cells into the
/// composer's first row).
fn composer(rest_ms: u64) -> Vec<Snap> {
    replay(&Capture {
        name: "claude-wrap-end",
        src: include_str!("fixtures/claude-wrap-2026-09-23-end.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        text_rows: (33, 37),
        indent: 2,
        pause: (rest_ms > 0).then_some((62, rest_ms)),
    })
}

/// The same in Claude Code's composer: after a 2.5 s rest the first key revives
/// the line (62 cells lit on its frame) with every glyph at its own stop. The
/// take as captured (no rest) shows no recolour or discontinuity (the control).
#[test]
fn claude_codes_composer_revived_after_a_rest_keeps_its_stops() {
    let control = composer(0);
    let flagged = runs(&control);
    assert!(
        recolours(&control).is_empty() && flagged.is_empty(),
        "the control: the captured take walks clean ({} recolours, runs {:?})",
        recolours(&control).len(),
        flagged.first()
    );
    let rested = composer(REST_MS);
    let lit_after_rest = rested
        .iter()
        .filter(|f| f.label == "key ' '" && f.pos.iter().filter(|p| p.lit).count() >= 60)
        .count();
    assert!(
        lit_after_rest > 0,
        "the premise: the key after the rest brings the line back lit"
    );
    let moved = recolours(&rested);
    assert!(
        moved.is_empty(),
        "{} text cells of the composer's line came back in another colour under \
         unchanged text after the rest; first (text, frame, before, after) {:?}\n{}",
        moved.len(),
        moved.first(),
        moved
            .first()
            .map(|&(_, k, _, _)| dump(&rested, k, k))
            .unwrap_or_default()
    );
}
