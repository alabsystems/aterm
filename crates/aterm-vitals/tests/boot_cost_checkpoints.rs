// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Checkpoint traces projected from the collector and strict restored document.

use std::collections::BTreeMap;

use aterm_models_misc::boot_cost_checkpoint_model;
use aterm_spec::{derive::Model, interp};
use aterm_vitals::boot_cost::{
    Book, Collector, Coverage, Observation, RawCounters, decode, encode,
};

type State = BTreeMap<&'static str, i64>;
const SECOND: u64 = 1_000_000_000;

fn cut(seconds: u64, own: u64) -> Observation {
    Observation {
        at_ns: seconds * SECOND,
        process_cpu: RawCounters {
            own_ns: Some(own),
            inherited_ns: Some(17),
        },
        coalition_cpu: RawCounters {
            own_ns: Some(41),
            inherited_ns: Some(59),
        },
        ..Observation::default()
    }
}

fn time_class(ns: u64) -> i64 {
    match ns / SECOND {
        0..180 => 0,
        180..900 => 1,
        _ => 2,
    }
}

/// Expected actual captures and saved-write status remain independent of the book being
/// checked. In particular, a corrupted cut cannot change its original capture's anchor.
fn project(book: &Book, anchors: &State) -> State {
    let mut state = anchors.clone();
    let boot = book.boot("boot-a");
    let first = boot.and_then(|b| b.observation(0));
    let second = boot.and_then(|b| b.observation(1));
    state.insert(
        "phase",
        if second.is_some() {
            2
        } else {
            i64::from(first.is_some())
        },
    );
    state.insert("first", first.map_or(0, |o| time_class(o.at_ns)));
    state.insert("second", second.map_or(0, |o| time_class(o.at_ns)));
    state.insert("nodes", boot.map_or(0, |b| b.observations.len() as i64));
    state.insert(
        "own",
        first.and_then(|o| o.process_cpu.own_ns).unwrap_or(0) as i64,
    );
    state.insert("pending", i64::from(state["nodes"] > state["saved"]));
    state.insert(
        "conflict",
        i64::from(boot.is_some_and(|b| b.coverage_for(0).contains(Coverage::CONFLICT))),
    );
    state
}

fn step(model: &Model, state: &mut State, action: &'static str, book: &Book) {
    let before = state.clone();
    assert!(model.fire(action, state), "disabled {action}");
    let actual = project(book, state);
    assert_eq!(actual, *state, "{action}: production projection differs");
    assert_eq!(interp::admits(model, &before, &actual), Some(action));
    for law in &model.invariants {
        assert!(
            model.check_invariant(law.name, &actual),
            "{action}: {}",
            law.name
        );
    }
}

#[test]
fn real_cuts_restore_conflict_and_write_outcomes_conform() {
    let model = boot_cost_checkpoint_model();
    for first_seconds in [180, 901] {
        let mut state = model.init_state();
        let mut collector = Collector::new(Book::default());
        collector.observe(Some("boot-a"), cut(179, 1)).unwrap();
        assert_eq!(project(collector.book(), &state), state);
        collector
            .observe(Some("boot-a"), cut(first_seconds, 5))
            .unwrap();
        step(
            &model,
            &mut state,
            if first_seconds == 180 {
                "Capture180"
            } else {
                "FirstCaptureAfter900"
            },
            collector.book(),
        );

        // A refused write has no acknowledgment to apply to the pure collector. The native
        // store's filesystem-failure tests independently check the atomic-write boundary.
        let pending = collector.book().clone();
        let before_failed = state.clone();
        assert!(model.fire("WriteFailed", &mut state));
        assert_eq!(project(collector.book(), &state), state);
        assert_eq!(collector.book(), &pending);
        assert_eq!(
            interp::admits(&model, &before_failed, &state),
            Some("WriteFailed")
        );

        let saved = decode(&encode(collector.book()).unwrap()).unwrap();
        step(&model, &mut state, "WriteLanded", &saved);
        collector = Collector::new(saved.clone());
        step(&model, &mut state, "Restore", collector.book());
        if first_seconds == 180 {
            collector.observe(Some("boot-a"), cut(900, 23)).unwrap();
            step(&model, &mut state, "Capture900", collector.book());
        }
        let frozen = collector.book().clone();
        collector.observe(Some("boot-a"), cut(950, 999)).unwrap();
        assert_eq!(collector.book(), &frozen);
        step(&model, &mut state, "Repeat", collector.book());

        let mut other = Collector::new(Book::default());
        other
            .observe(Some("boot-a"), cut(first_seconds, 7))
            .unwrap();
        let mut merged = collector.book().clone();
        merged.merge(other.book()).unwrap();
        step(&model, &mut state, "ConflictingSource", &merged);
        assert_eq!(
            merged.boots[0].observation(0).unwrap().process_cpu.own_ns,
            Some(5)
        );
        assert_eq!(
            merged.boots[0]
                .observation(0)
                .unwrap()
                .process_cpu
                .inherited_ns,
            Some(17)
        );
        assert_eq!(
            merged.boots[0].observation(0).unwrap().coalition_cpu.own_ns,
            Some(41)
        );
    }
}

#[test]
fn conformance_rejects_fabricated_cuts_duplicates_and_summed_sources() {
    let model = boot_cost_checkpoint_model();
    let mut anchors = model.init_state();
    assert!(model.fire("FirstCaptureAfter900", &mut anchors));
    let mut collector = Collector::new(Book::default());
    collector.observe(Some("boot-a"), cut(901, 5)).unwrap();
    assert_eq!(project(collector.book(), &anchors), anchors);

    for (law, corrupt) in [
        ("TargetsNameActualCuts", 1),
        ("OneNodePerActualCapture", 2),
        ("CapturedAccountingIsImmutable", 3),
    ] {
        let mut bad = collector.book().clone();
        match corrupt {
            1 => bad.boots[0].observations[0].at_ns = 180 * SECOND,
            2 => {
                let mut duplicate = bad.boots[0].observations[0].clone();
                duplicate.at_ns += SECOND;
                bad.boots[0].observations.push(duplicate);
                bad.boots[0].targets[1] = Some(1);
            }
            _ => bad.boots[0].observations[0].process_cpu.own_ns = Some(12),
        }
        let projected = project(&bad, &anchors);
        assert!(
            !model.check_invariant(law, &projected),
            "negative control escaped {law}"
        );
        assert_ne!(projected, anchors);
    }
    let mut acknowledged_on_failure = anchors.clone();
    acknowledged_on_failure.insert("pending", 0);
    assert!(!model.check_invariant("FailedWriteRetainsPending", &acknowledged_on_failure));
}
