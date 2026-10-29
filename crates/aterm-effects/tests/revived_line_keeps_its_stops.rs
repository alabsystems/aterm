// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that a typed line revived after a rest keeps its rainbow stops.

use crate::trail_host::on_every_ground;
use crate::walk_census::*;
use aterm_effects::rainbow_kitty::ribbon::{CHAIN_GAP_MAX, WALK_LAY_RATE, walk_t};

const LINE_CELLS: usize = 30;
const REST_MS: u64 = 2_500;
const SETTLE_MS: u64 = 48;

fn stops(s: &Shell) -> Vec<Option<f32>> {
    s.snaps
        .last()
        .expect("a frame")
        .pos
        .iter()
        .map(|p| p.lit.then_some(p.t).flatten())
        .collect()
}

fn typed(n: usize) -> (Shell, Vec<Option<f32>>) {
    typed_echo(n, Echo::After(2))
}

fn typed_echo(n: usize, echo: Echo) -> (Shell, Vec<Option<f32>>) {
    typed_shaped(n, echo, Fold::Readline)
}

fn typed_shaped(n: usize, echo: Echo, fold: Fold) -> (Shell, Vec<Option<f32>>) {
    let mut s = Shell::new(60, 83, echo).folding(fold);
    s.type_str(&prose(n));
    s.idle(SETTLE_MS);
    let before = stops(&s);
    (s, before)
}

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

fn revived(
    n: usize,
    row: std::ops::Range<usize>,
    what: &str,
) -> (Shell, Vec<Option<f32>>, Vec<Option<f32>>) {
    revived_echo(n, row, what, Echo::After(2))
}

fn revived_echo(
    n: usize,
    row: std::ops::Range<usize>,
    what: &str,
    echo: Echo,
) -> (Shell, Vec<Option<f32>>, Vec<Option<f32>>) {
    revived_shaped(n, row, what, echo, Fold::Readline)
}

fn revived_shaped(
    n: usize,
    row: std::ops::Range<usize>,
    what: &str,
    echo: Echo,
    fold: Fold,
) -> (Shell, Vec<Option<f32>>, Vec<Option<f32>>) {
    let (mut s, before) = typed_shaped(n, echo, fold);
    assert_cold_walk(&before, row, what);
    rest(&mut s, what);
    s.key(' ');
    s.idle(SETTLE_MS);
    let after = stops(&s);
    (s, before, after)
}

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

const WRAPPED: usize = 80;
const ROW2: std::ops::Range<usize> = 58..80;

const LATE_ECHO_MS: u64 = 20;
const FRAME_MS: u64 = 16;

#[test]
fn a_line_revived_when_the_key_s_echo_lands_a_frame_late_keeps_its_stops() {
    on_every_ground(|| {
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
            let held = s
                .snaps
                .iter()
                .filter(|f| {
                    f.ms > press_ms
                        && f.ms <= press_ms + LATE_ECHO_MS
                        && f.text.chars().count() == n
                })
                .count();
            assert!(
                held > 0,
                "{what}: the premise — a frame presented the press before its echo"
            );
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
    });
}

#[test]
fn a_run_no_key_saw_standing_comes_back_on_its_own_walk() {
    on_every_ground(|| {
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
    });
}

#[test]
fn a_wrapped_line_revived_after_a_rest_lights_its_second_row_from_its_first_glyph() {
    on_every_ground(|| {
        let mut broken = Vec::new();
        let (s, before, after) = revived(LINE_CELLS, 0..LINE_CELLS, "one row");
        let dark: Vec<usize> = (0..LINE_CELLS).filter(|&i| after[i].is_none()).collect();
        assert!(
            dark.is_empty(),
            "the control: a one-row line comes back whole, dark {dark:?}"
        );
        broken.extend(stops_kept(
            &s,
            LINE_CELLS,
            0..LINE_CELLS,
            &before,
            &after,
            "one row",
        ));
        for (what, fold, echo) in [
            ("kernel echo", Fold::Kernel, Echo::After(2)),
            ("readline", Fold::Readline, Echo::After(2)),
            ("zle", Fold::Zle, Echo::After(2)),
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
    });
}

#[test]
fn without_the_revive_the_key_continues_the_walk_and_no_stop_moves() {
    on_every_ground(|| {
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
    });
}

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

#[test]
fn claude_codes_composer_revived_after_a_rest_keeps_its_stops() {
    on_every_ground(|| {
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
    });
}
