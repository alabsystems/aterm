// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the follow pass (`Witness::follow_runs`) to `ribbon_follow_twin_model`.

use std::collections::BTreeMap;
use std::time::Instant;

use aterm_effects::rainbow_kitty::ribbon::{Cell, Layer};
use aterm_effects::rainbow_kitty::witness::{RowSample, Witness};
use aterm_models_effects::ribbon_follow_twin_model;
use aterm_spec::derive::Model;

const N: usize = 6;
const GLYPHS: [char; N] = ['a', 'b', 'c', 'd', 'e', 'f'];
const INVARIANTS: [&str; 5] = [
    "AllStandingNeverFollows",
    "AnArrivedBlockFollows",
    "OnlyAnArrivedBlockFollows",
    "MissedIsAnArrivalNotNamed",
    "StateBounded",
];

/// A record's class at the target row, one row up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Arrived,
    Standing,
    Absent,
    AbsentTwin,
}

type Verdict = (i64, i64, i64, i64);

fn cell(col: u16, born: Instant) -> Cell {
    Cell {
        row: 5,
        col,
        cohort: 7,
        t: 0.0,
        born,
        attack_at: born,
        life_s: 1.0,
        cov0: 1.0,
        typing: true,
        retract_at: None,
        retire_at: None,
        birth_disp: 0.5,
        edge_cells: 3.0,
        layer: Layer::Base,
        rearm: None,
        released_at: None,
    }
}

fn real(w: &mut Witness, cells: &[Cell], classes: &[Class; N], gone: &[bool; N]) -> Verdict {
    let at_arm: Vec<char> = (0..N)
        .map(|k| match classes[k] {
            Class::Standing | Class::AbsentTwin => GLYPHS[k],
            Class::Arrived | Class::Absent => ' ',
        })
        .collect();
    let now: Vec<char> = (0..N)
        .map(|k| match classes[k] {
            Class::Arrived | Class::Standing => GLYPHS[k],
            Class::Absent | Class::AbsentTwin => ' ',
        })
        .collect();
    let own: Vec<char> = (0..N)
        .map(|k| if gone[k] { ' ' } else { GLYPHS[k] })
        .collect();
    w.clear();
    let (mut retire, mut release, mut out) = (Vec::new(), Vec::new(), Vec::new());
    w.walk(
        cells,
        &[
            RowSample {
                row: 5,
                cols: &GLYPHS,
            },
            RowSample {
                row: 4,
                cols: &at_arm,
            },
        ],
        &[],
        cells[0].born,
        &mut retire,
        &mut release,
    );
    assert!(GLYPHS.iter().all(|&g| g != ' '));
    assert!(retire.is_empty() && release.is_empty());
    let score = w.follow_runs(
        cells,
        &[
            RowSample { row: 5, cols: &own },
            RowSample { row: 4, cols: &now },
        ],
        &mut out,
    );
    let missed = i64::from(score.missed);
    match out.as_slice() {
        [] => (0, 0, 0, missed),
        [run] => {
            assert!(
                run.row == 5 && run.cohort == 7 && run.dr == -1,
                "one run, one row up: {run:?}"
            );
            (1, i64::from(run.lo) + 1, i64::from(run.hi) + 1, missed)
        }
        more => panic!("one run, named once: {more:?}"),
    }
}

struct Scanner<'m> {
    model: &'m Model,
    /// `prefix[k]`: the action fired for record `k` and the state after it.
    prefix: Vec<(&'static str, BTreeMap<&'static str, i64>)>,
}

impl<'m> Scanner<'m> {
    fn new(model: &'m Model) -> Self {
        Self {
            model,
            prefix: Vec::with_capacity(N),
        }
    }

    fn modelled(&mut self, classes: &[Class; N], gone: &[bool; N]) -> BTreeMap<&'static str, i64> {
        let actions: [&'static str; N] = std::array::from_fn(|k| match (classes[k], gone[k]) {
            (Class::Arrived, false) => "ScanArrived",
            (Class::Arrived, true) => "ScanArrivedGone",
            (Class::Standing, false) => "ScanStanding",
            (Class::Standing, true) => "ScanStandingGone",
            (Class::Absent | Class::AbsentTwin, false) => "ScanAbsent",
            (Class::Absent | Class::AbsentTwin, true) => "ScanAbsentGone",
        });
        let keep = self
            .prefix
            .iter()
            .zip(&actions)
            .take_while(|(done, action)| done.0 == **action)
            .count();
        self.prefix.truncate(keep);
        let mut state = match self.prefix.last() {
            Some((_, scanned)) => scanned.clone(),
            None => self.model.init_state(),
        };
        for &action in &actions[keep..] {
            assert!(self.model.fire(action, &mut state), "{action} is enabled");
            self.prefix.push((action, state.clone()));
        }
        assert!(self.model.fire("Decide", &mut state), "Decide is enabled");
        state
    }
}

fn project(state: &BTreeMap<&'static str, i64>, v: Verdict) -> BTreeMap<&'static str, i64> {
    let mut p = state.clone();
    p.insert("named", v.0);
    if v.0 == 1 {
        p.insert("lo", v.1);
        p.insert("hi", v.2);
    }
    p.insert("missed", v.3);
    p
}

fn at_n(mut model: Model) -> Model {
    for c in &mut model.consts {
        match c.0 {
            "N" => c.1 = N as i64,
            "Buggy" => c.1 = 0,
            _ => {}
        }
    }
    model
}

fn every_configuration(mut f: impl FnMut(&[Class; N], &[bool; N])) {
    const CLASSES: [Class; 3] = [Class::Arrived, Class::Standing, Class::Absent];
    for code in 0..6usize.pow(N as u32) {
        let mut classes = [Class::Absent; N];
        let mut gone = [false; N];
        let mut c = code;
        for k in (0..N).rev() {
            let digit = c % 6;
            classes[k] = CLASSES[digit / 2];
            gone[k] = digit % 2 == 1;
            c /= 6;
        }
        f(&classes, &gone);
    }
}

fn with_absent_twins(classes: &[Class; N], mut f: impl FnMut(&[Class; N])) {
    let absent: Vec<usize> = (0..N).filter(|&k| classes[k] == Class::Absent).collect();
    for sub in 0..(1u32 << absent.len()) {
        let mut twinned = *classes;
        for (j, &k) in absent.iter().enumerate() {
            if sub & (1 << j) != 0 {
                twinned[k] = Class::AbsentTwin;
            }
        }
        f(&twinned);
    }
}

#[test]
fn the_real_follow_pass_is_the_twin_contract_on_every_six_record_run() {
    let model = at_n(ribbon_follow_twin_model());
    let t0 = Instant::now();
    let cells: Vec<Cell> = (0..N as u16).map(|c| cell(c, t0)).collect();
    let mut w = Witness::new();
    let mut scan = Scanner::new(&model);
    let (mut runs, mut named, mut missed) = (0usize, 0usize, 0usize);
    every_configuration(|classes, gone| {
        let state = scan.modelled(classes, gone);
        for inv in INVARIANTS {
            assert!(
                model.check_invariant(inv, &state),
                "{classes:?} gone {gone:?}: the model at N = {N} breaks {inv}"
            );
        }
        with_absent_twins(classes, |twinned| {
            let v = real(&mut w, &cells, twinned, gone);
            assert_eq!(
                project(&state, v),
                state,
                "{twinned:?} gone {gone:?}: the real verdict {v:?} is not the model's"
            );
            runs += 1;
            named += usize::from(v.0 == 1);
            missed += usize::from(v.3 > 0);
        });
    });
    assert_eq!(runs, 4usize.pow(N as u32) << N);
    assert!(
        named > 1000 && missed > 100,
        "the sweep reaches both verdicts: {named} named, {missed} missed of {runs}"
    );
}
