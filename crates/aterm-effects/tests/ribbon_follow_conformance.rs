// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE REAL FOLLOW PASS CONFORMS TO THE TWIN CONTRACT** (2026-09-23 — the
//! owner: *"the existing line rainbow should beautifully flow and drift and
//! fade away, not simply abruptly vanish"*). Tier-1 for `aterm-spec`'s
//! `ribbon_follow_twin_model` (`RibbonFollowTwin`): the content witness's
//! ARM writes a twin bit (`Witness::walk`) and its FOLLOW reads it
//! (`Witness::follow_runs`), and nothing tied the two to a model until the
//! review of the twin-neutral landing found two changes to them that the
//! whole suite let through — the trailing edge rider's `gone && trail_open`
//! read as `trail_open`, and the follow counter's accumulation replaced by
//! a no-op.
//!
//! Since 2026-09-26 the bit the reader reads is the ACCUMULATED arrival
//! evidence (`Seen::arrivals`), and a twin at arm time is one of the ways a
//! record comes to have none; how the witness WRITES that evidence through
//! the frames of a record's life is the writer's own model,
//! `RibbonArrivalEvidence`, bound in `tests/ribbon_arrival_conformance.rs`.
//! This file drives the reader through the arm-time way.
//!
//! Every configuration of a six-record run is driven through the REAL
//! witness: each record ARRIVED (its glyph a row up, no twin at arm time),
//! STANDING (its glyph a row up, and there when armed), ABSENT (not there)
//! or ABSENT-TWIN (there when armed, gone since), and each GONE from its own
//! row or not — `4^6 × 2^6` runs, the model read once per `3^6 × 2^6` (it
//! counts both ABSENTs alike) and every invariant checked on it at `N = 6`
//! (the Tier-0 proof is at `N = 4`). The six glyphs are distinct, so no
//! suffix of the run can stand relaid at the start of its own row
//! (`Witness::relaid_suffix` reads `0` throughout: the model has no relay).
//! The real verdict — named or not, its extent, the missed count — must be
//! exactly the model's `Decide` successor at `N = 6`, and every invariant
//! must hold on it. The negative control replays the 2026-09-22 veto (a
//! twin never found, never neutral) against the same configurations: it is
//! the model at `Buggy = 1`, bit for bit, and `AnArrivedBlockFollows`
//! refuses it on hundreds of them — the check is not vacuous.

use std::collections::BTreeMap;
use std::time::Instant;

use aterm_effects::rainbow_kitty::ribbon::{Cell, Layer};
use aterm_effects::rainbow_kitty::witness::{RowSample, Witness};
use aterm_spec::derive::{Model, ribbon_follow_twin_model};

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

/// `(named, lo, hi, missed)`, columns 1-based as the model counts them.
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

/// The REAL verdict: arm `abcdef` on row 5 with row 4 holding each
/// STANDING and ABSENT-TWIN record's glyph, then blank each GONE record's
/// column on row 5 and put each ARRIVED and STANDING record's glyph on
/// row 4.
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
    // Every cell is armed by that walk: `GLYPHS` holds no blank, and a
    // glyph read under a record-less cell is armed (`Witness::len` is
    // test-only inside the crate since the 2026-09 dead-code sweep).
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

/// The 2026-09-22 VETO, replayed: a twin at the target is never found and
/// never neutral — a hole — and the found records must be one block of at
/// least two and at least half of ALL the armed records. No riders, and no
/// missed count (it had none).
fn veto(classes: &[Class; N], gone: &[bool; N]) -> (i64, i64, i64) {
    let gone_n = gone.iter().filter(|g| **g).count();
    if gone_n < 2 || gone_n * 2 < N {
        return (0, 0, 0);
    }
    let (mut found, mut lo, mut hi, mut after) = (0usize, 0i64, 0i64, false);
    for (k, class) in classes.iter().enumerate() {
        if *class == Class::Arrived {
            if after {
                return (0, 0, 0);
            }
            found += 1;
            if lo == 0 {
                lo = k as i64 + 1;
            }
            hi = k as i64 + 1;
        } else if found > 0 {
            after = true;
        }
    }
    if found >= 2 && found * 2 >= N {
        (1, lo, hi)
    } else {
        (0, 0, 0)
    }
}

/// The model's state after scanning the configuration and deciding.
fn modelled(model: &Model, classes: &[Class; N], gone: &[bool; N]) -> BTreeMap<&'static str, i64> {
    let mut state = model.init_state();
    for k in 0..N {
        let action = match (classes[k], gone[k]) {
            (Class::Arrived, false) => "ScanArrived",
            (Class::Arrived, true) => "ScanArrivedGone",
            (Class::Standing, false) => "ScanStanding",
            (Class::Standing, true) => "ScanStandingGone",
            (Class::Absent | Class::AbsentTwin, false) => "ScanAbsent",
            (Class::Absent | Class::AbsentTwin, true) => "ScanAbsentGone",
        };
        assert!(model.fire(action, &mut state), "{action} is enabled");
    }
    assert!(model.fire("Decide", &mut state), "Decide is enabled");
    state
}

/// `state` with the verdict `v` projected onto it — the extent only where
/// a run was named (unnamed, the model's `lo`/`hi` are the scan's own).
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

fn at_n(mut model: Model, buggy: i64) -> Model {
    for c in &mut model.consts {
        match c.0 {
            "N" => c.1 = N as i64,
            "Buggy" => c.1 = buggy,
            _ => {}
        }
    }
    model
}

/// Every configuration of the model's three classes and the gone bits:
/// `f(classes, gone)` — `3^6 × 2^6` runs.
fn every_configuration(mut f: impl FnMut(&[Class; N], &[bool; N])) {
    const CLASSES: [Class; 3] = [Class::Arrived, Class::Standing, Class::Absent];
    for code in 0..3usize.pow(N as u32) {
        let mut classes = [Class::Absent; N];
        let mut c = code;
        for slot in &mut classes {
            *slot = CLASSES[c % 3];
            c /= 3;
        }
        for bits in 0..(1u32 << N) {
            let mut gone = [false; N];
            for (k, g) in gone.iter_mut().enumerate() {
                *g = bits & (1 << k) != 0;
            }
            f(&classes, &gone);
        }
    }
}

/// Every way of making some of `classes`' ABSENT records ABSENT-TWIN.
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
    let model = at_n(ribbon_follow_twin_model(), 0);
    let t0 = Instant::now();
    let cells: Vec<Cell> = (0..N as u16).map(|c| cell(c, t0)).collect();
    let mut w = Witness::new();
    let (mut runs, mut named, mut missed) = (0usize, 0usize, 0usize);
    every_configuration(|classes, gone| {
        // The model at N = 6 (Tier-0 proves it at 4): every Decide state
        // of the sweep keeps every invariant.
        let state = modelled(&model, classes, gone);
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

/// **THE NEGATIVE CONTROL**: the 2026-09-22 veto the owner's vanish came
/// from is exactly the model at `Buggy = 1` — so the dial the Tier-0 check
/// catches is this code — and projected onto the correct model it breaks
/// `AnArrivedBlockFollows` wherever a twin stood inside or flush against an
/// arrived block, and nowhere without a twin.
#[test]
fn the_2026_09_22_veto_is_the_buggy_model_and_the_contract_refuses_it() {
    let model = at_n(ribbon_follow_twin_model(), 0);
    let buggy = at_n(ribbon_follow_twin_model(), 1);
    let (mut refused, mut total) = (0usize, 0usize);
    every_configuration(|classes, gone| {
        total += 1;
        let (named, lo, hi) = veto(classes, gone);
        let b = modelled(&buggy, classes, gone);
        assert_eq!(
            b["named"], named,
            "{classes:?} gone {gone:?}: the Buggy model is the veto"
        );
        if named == 1 {
            assert_eq!(
                (b["lo"], b["hi"]),
                (lo, hi),
                "{classes:?} gone {gone:?}: the Buggy model's extent is the veto's"
            );
        }
        let state = modelled(&model, classes, gone);
        let p = project(&state, (named, lo, hi, state["missed"]));
        if !model.check_invariant("AnArrivedBlockFollows", &p) {
            refused += 1;
            assert!(
                classes.contains(&Class::Standing),
                "{classes:?} gone {gone:?}: only a twin separates the veto from the contract"
            );
        }
    });
    assert!(
        refused > 100,
        "the veto is refused on {refused} of {total} runs: the check is not vacuous"
    );
}
