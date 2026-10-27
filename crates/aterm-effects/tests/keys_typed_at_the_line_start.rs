// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keys typed at the start of a lit line step one stop a key.

use crate::trail_host::on_every_ground;
use crate::walk_census::*;

const LINE_CELLS: usize = 30;
const INSERT: &str = "XYZ ";
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

fn take(keys: &str, at_start: bool) -> (Shell, Vec<Option<f32>>, Vec<Option<f32>>) {
    let mut s = Shell::new(60, 83, Echo::After(2));
    s.type_str(&prose(LINE_CELLS));
    s.idle(SETTLE_MS);
    let before = stops(&s);
    if at_start {
        s.home();
    }
    s.type_str(keys);
    s.idle(SETTLE_MS);
    let after = stops(&s);
    (s, before, after)
}

fn not_forward(ts: &[Option<f32>], a: usize, b: usize) -> Vec<(usize, f32)> {
    (a..b)
        .filter_map(|i| {
            let dt = ts[i + 1]? - ts[i]?;
            (dt <= EPS).then_some((i, dt))
        })
        .collect()
}

#[test]
fn keys_typed_at_the_start_of_a_lit_line_step_one_stop_a_key() {
    on_every_ground(|| {
        let (_, _, after) = take(INSERT, false);
        let n = LINE_CELLS + INSERT.len();
        assert!(
            (0..n).all(|i| after[i].is_some()),
            "the control: the whole line is lit"
        );
        let bad = not_forward(&after, LINE_CELLS - 1, n - 1);
        assert!(bad.is_empty(), "the control: typed at the end, {bad:?}");
        let (_, _, after) = take("X", true);
        let bad = not_forward(&after, 0, 1);
        assert!(bad.is_empty(), "the control: one key at the start, {bad:?}");

        let (s, before, after) = take(INSERT, true);
        let k = INSERT.len();
        assert!(
            (0..LINE_CELLS + k).all(|i| after[i].is_some()),
            "the premise: the whole line is lit\n{}",
            s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
        );
        let bad = not_forward(&after, 0, k);
        assert!(
            bad.is_empty(),
            "the keys typed at the line's start do not step forward — (left text index, \
             dt) {bad:?}: {:?}\n{}",
            &after[..=k],
            s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
        );
        for i in 0..LINE_CELLS {
            let (b, a) = (before[i].expect("lit"), after[i].expect("lit"));
            assert!(
                (a - b).abs() < 1e-4,
                "§41's law: column {i} kept its stop under the pushed text ({b} -> {a}) — the \
                 colour stays with the column\n{}",
                s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
            );
        }
        let bad = not_forward(&after, 0, LINE_CELLS + k - 1);
        assert!(
            bad.is_empty(),
            "the line after the insert does not step forward — (left text index, dt) \
             {bad:?}\n{}",
            s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
        );
    });
}
