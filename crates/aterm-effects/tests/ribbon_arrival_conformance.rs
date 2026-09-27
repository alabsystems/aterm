// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE REAL WITNESS WRITES THE ARRIVAL EVIDENCE ITS MODEL STATES**
//! (2026-09-26). Tier-1 for `aterm-spec`'s `ribbon_arrival_evidence_model`
//! (`RibbonArrivalEvidence`): the writer half of the follow pass. The
//! reader (`tests/ribbon_follow_conformance.rs`) decides a follow from
//! whether each record's glyph ARRIVED at the target; this binds what the
//! witness has WRITTEN by the time it asks — through the arm and every walk
//! of the record's life, the target row seen holding the record's glyph,
//! something else, or not sampled, at frames one or two time units apart.
//!
//! Every trace of the model up to its bound (`T = 5` units of 20 ms,
//! `TWIN_MIN` being 40 ms) is replayed on the REAL witness: a three-glyph
//! run armed on row 5 with row 4 sampled per the trace's arm, walked at each
//! frame's wall time with row 4 per the frame, then the run's own row
//! blanked and its text standing on row 4 — the move, or the erase under a
//! copy. The follow pass must name the run exactly where the model's writer
//! says the glyphs ARRIVED (`clear == 1 && twin == 0`), and every invariant
//! must hold on the model's state. The negative control replays the fzf
//! false follow the 2026-09-23 writer made (`Buggy = 1`): on the traces
//! where a copy landed beside the standing line and stood 40 ms, that
//! writer says ARRIVED where the real witness refuses — the bind is not
//! vacuous, and it would have caught the defect.

use std::time::{Duration, Instant};

use aterm_effects::rainbow_kitty::ribbon::{Cell, Layer};
use aterm_effects::rainbow_kitty::witness::{RowSample, Witness};
use aterm_spec::derive::{Model, ribbon_arrival_evidence_model};
use aterm_spec::interp;

const TEXT: [char; 3] = ['a', 'b', 'c'];
const OTHER: [char; 3] = ['x', 'y', 'z'];
const BLANK: [char; 3] = [' ', ' ', ' '];
/// One model time unit: `TWIN_MIN` (40 ms) is the model's `TwinMin = 2`.
const UNIT: Duration = Duration::from_millis(20);

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

/// What the row one up holds on a frame: the record's glyphs, other text,
/// or nothing sampled.
#[derive(Clone, Copy, Debug)]
enum Beside {
    Same,
    Diff,
    Unsampled,
}

/// A trace: the arm's `Beside`, then each frame's `(Beside, units)`.
type Trace = (Beside, Vec<(Beside, u32)>);

fn arm_action(b: Beside) -> &'static str {
    match b {
        Beside::Same => "ArmSame",
        Beside::Diff => "ArmDiff",
        Beside::Unsampled => "ArmUnsampled",
    }
}

fn frame_action(b: Beside, units: u32) -> &'static str {
    match (b, units) {
        (Beside::Same, 1) => "SameAfterOne",
        (Beside::Same, _) => "SameAfterTwo",
        (Beside::Diff, 1) => "DiffAfterOne",
        (Beside::Diff, _) => "DiffAfterTwo",
        (Beside::Unsampled, 1) => "UnsampledAfterOne",
        (Beside::Unsampled, _) => "UnsampledAfterTwo",
    }
}

/// Every trace the model can take up to its bound `t`.
fn every_trace(bound: u32) -> Vec<Trace> {
    const ALL: [Beside; 3] = [Beside::Same, Beside::Diff, Beside::Unsampled];
    fn grow(
        prefix: &mut Vec<(Beside, u32)>,
        t: u32,
        bound: u32,
        out: &mut Vec<Vec<(Beside, u32)>>,
    ) {
        out.push(prefix.clone());
        for b in ALL {
            for dt in [1, 2] {
                if t + dt <= bound {
                    prefix.push((b, dt));
                    grow(prefix, t + dt, bound, out);
                    prefix.pop();
                }
            }
        }
    }
    let mut frames = Vec::new();
    grow(&mut Vec::new(), 0, bound, &mut frames);
    let mut out = Vec::new();
    for arm in ALL {
        for f in &frames {
            out.push((arm, f.clone()));
        }
    }
    out
}

/// The model's state after the trace, every invariant checked on the way.
fn modelled(model: &Model, trace: &Trace) -> std::collections::BTreeMap<&'static str, i64> {
    let mut st = model.init_state();
    assert!(model.fire(arm_action(trace.0), &mut st));
    for &(b, dt) in &trace.1 {
        let a = frame_action(b, dt);
        assert!(model.fire(a, &mut st), "{a} is enabled on {trace:?}");
    }
    st
}

/// The REAL witness over the trace: `true` when the follow pass names the
/// run onto the copy a row up once the run's own row is blank.
fn real_follows(trace: &Trace) -> bool {
    let t0 = Instant::now();
    let cells: Vec<Cell> = (0..3).map(|c| cell(c, t0)).collect();
    let mut w = Witness::new();
    let (mut retire, mut release, mut out) = (Vec::new(), Vec::new(), Vec::new());
    let mut walk_at = |w: &mut Witness, at: Instant, beside: Beside| {
        let above: &[char] = match beside {
            Beside::Same => &TEXT,
            Beside::Diff => &OTHER,
            Beside::Unsampled => &BLANK,
        };
        let own = RowSample {
            row: 5,
            cols: &TEXT,
        };
        let up = RowSample {
            row: 4,
            cols: above,
        };
        let rows: Vec<RowSample<'_>> = match beside {
            Beside::Unsampled => vec![own],
            _ => vec![own, up],
        };
        w.walk(&cells, &rows, &[], at, &mut retire, &mut release);
        assert!(
            retire.is_empty() && release.is_empty(),
            "{trace:?}: nothing changed"
        );
    };
    walk_at(&mut w, t0, trace.0);
    let mut t = 0u32;
    for &(b, dt) in &trace.1 {
        t += dt;
        walk_at(&mut w, t0 + UNIT * t, b);
    }
    w.follow_runs(
        &cells,
        &[
            RowSample {
                row: 5,
                cols: &BLANK,
            },
            RowSample {
                row: 4,
                cols: &TEXT,
            },
        ],
        &mut out,
    );
    !out.is_empty()
}

#[test]
fn the_real_witness_writes_the_arrival_evidence_on_every_trace() {
    let model = ribbon_arrival_evidence_model();
    let bound = u32::try_from(model.consts.iter().find(|c| c.0 == "T").expect("T").1)
        .expect("a small bound");
    let invariants: Vec<&str> = model.invariants.iter().map(|i| i.name).collect();
    let (mut follows, mut refused) = (0usize, 0usize);
    for trace in every_trace(bound) {
        let st = modelled(&model, &trace);
        for inv in &invariants {
            assert!(model.check_invariant(inv, &st), "{trace:?}: {inv}");
        }
        let arrived = st["clear"] == 1 && st["twin"] == 0;
        assert_eq!(
            real_follows(&trace),
            arrived,
            "{trace:?}: the real follow is not the model's arrival ({st:?})"
        );
        if arrived {
            follows += 1;
        } else {
            refused += 1;
        }
    }
    assert!(
        follows > 100 && refused > 100,
        "both verdicts reached: {follows} followed, {refused} refused"
    );
}

/// **THE NEGATIVE CONTROL**: the 2026-09-23 writer (`Buggy = 1`, a twin only
/// at arm) disagrees with the real witness on the traces where a copy
/// appeared beside the standing line and stood `TWIN_MIN` — fzf's list
/// landing under the query — and on no trace where the copy was there at
/// arm: the real witness is the contract, not the old writer.
#[test]
fn the_2026_09_23_writer_is_caught_on_the_copies_that_stood() {
    let model = ribbon_arrival_evidence_model();
    let old = interp::with_buggy(&model, 1);
    let bound = u32::try_from(model.consts.iter().find(|c| c.0 == "T").expect("T").1)
        .expect("a small bound");
    let mut caught = 0usize;
    for trace in every_trace(bound) {
        let st = modelled(&old, &trace);
        let says = st["clear"] == 1 && st["twin"] == 0;
        if says != real_follows(&trace) {
            caught += 1;
            assert_eq!(
                st["stood"], 1,
                "{trace:?}: only a copy that stood tells them apart"
            );
            assert_eq!(st["first"], 0, "{trace:?}");
        }
    }
    assert!(caught > 10, "the old writer is caught on {caught} traces");
}
