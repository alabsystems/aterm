// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_spec::derive::{Model, harness_upgrade_notice_owner_model};
use aterm_spec::{interp, verify};

type S = BTreeMap<&'static str, i64>;

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

fn run(m: &Model, actions: &[&str]) -> S {
    let mut s = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut s), "{action} at {s:?}");
    }
    s
}

/// Every state `m` reaches, invariants unchecked.
fn reachable(m: &Model) -> Vec<S> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            let mut next = s.clone();
            if m.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        out.push(s);
    }
    out
}

/// The defects the notice's owner was fixed for, one knob each, and the
/// invariant that catches each alone.
const KNOBS: [(&str, &str); 2] = [
    ("NoReask", "NeverWaitsOnAGoneNotice"),
    ("KeepRound", "OnlyOnItsOwnAnswer"),
];

#[test]
fn ready_from_tab_a_never_authorizes_tab_b_or_duplicate_owners() {
    let model = harness_upgrade_notice_owner_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade notice owner");
    // Each invariant is load-bearing on its own: the reducers before the fix
    // break every one.
    for invariant in [
        "OnlyIssuerSignaled",
        "NoDuplicateOwnerSignal",
        "NoPartialScanSignal",
        "OnlyOnItsOwnAnswer",
        "NeverWaitsOnAGoneNotice",
    ] {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 0)).is_ok(),
            "{invariant}"
        );
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 1)).is_err(),
            "{invariant} catches the reducers before the fix"
        );
    }

    let mut foreign = model.init_state();
    assert!(model.fire("Announce", &mut foreign));
    assert!(model.fire("Ready", &mut foreign));
    assert!(model.fire("OtherTab", &mut foreign));
    assert!(!model.action_enabled("Terminate", &foreign));

    let mut duplicate = model.init_state();
    assert!(model.fire("Announce", &mut duplicate));
    assert!(model.fire("Ready", &mut duplicate));
    assert!(model.fire("DuplicateOwner", &mut duplicate));
    assert!(!model.action_enabled("Terminate", &duplicate));

    let mut partial = model.init_state();
    assert!(model.fire("Announce", &mut partial));
    assert!(model.fire("Ready", &mut partial));
    assert!(model.fire("IncompleteScan", &mut partial));
    assert!(!model.action_enabled("Terminate", &partial));

    let buggy = interp::with_buggy(&model, 1);
    let mut stolen = buggy.init_state();
    assert!(buggy.fire("Announce", &mut stolen));
    assert!(buggy.fire("Ready", &mut stolen));
    assert!(buggy.fire("OtherTab", &mut stolen));
    assert!(buggy.fire("Terminate", &mut stolen));
    assert!(!buggy.check_invariant("OnlyIssuerSignaled", &stolen));
    assert!(!buggy.check_invariant("OnlyOnItsOwnAnswer", &stolen));

    let mut partial = buggy.init_state();
    assert!(buggy.fire("Announce", &mut partial));
    assert!(buggy.fire("Ready", &mut partial));
    assert!(buggy.fire("IncompleteScan", &mut partial));
    assert!(buggy.fire("Terminate", &mut partial));
    assert!(!buggy.check_invariant("NoPartialScanSignal", &partial));
}

/// ONE KNOB PER DEFECT, EACH CAUGHT ON ITS OWN: main before 2026-09-27
/// (`NoReask`) strands the upgrade on a notice whose process is gone, and the
/// tempting fix that asks afresh in the SAME round (`KeepRound`) signals the
/// new process on the gone one's READY. The committed configuration breaks
/// neither, and neither knob breaks the other's invariant.
#[test]
fn each_notice_owner_defect_is_caught_on_its_own() {
    let model = harness_upgrade_notice_owner_model();
    for (knob, invariant) in KNOBS {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_consts(&alone, &[(knob, 0)])).is_ok(),
            "{knob} off"
        );
        assert!(
            interp::bmc(&interp::with_consts(&alone, &[(knob, 1)])).is_err(),
            "{knob} is caught by {invariant}"
        );
        for (other, _) in KNOBS.iter().filter(|(k, _)| *k != knob) {
            assert!(
                interp::bmc(&interp::with_consts(&alone, &[(other, 1)])).is_ok(),
                "{other} does not break {invariant}"
            );
        }
    }
}

/// A NOTICE WHOSE PROCESS IS GONE IS ASKED AFRESH OF THE LIVE HOLDER, in the
/// notice's own tab, in another, and under the notice's pid recycled — in a
/// new round, the gone process's READY no answer to it, so the new process is
/// signalled only on its own READY. While the notified process lives, or two
/// hold the conversation, it waits, and that wait is no strand.
#[test]
fn a_notice_whose_process_is_gone_is_asked_afresh_in_a_new_round() {
    let model = harness_upgrade_notice_owner_model();
    for holder in ["Resume", "OtherTab", "Recycled"] {
        let s = run(&model, &["Announce", "Ready", "OwnerExits", holder]);
        assert!(model.action_enabled("Reask", &s), "{holder}: {s:?}");
        assert!(!model.action_enabled("Terminate", &s), "{holder}: {s:?}");
        assert!(!model.action_enabled("Look", &s), "{holder}: no wait");
        let mut s = s;
        assert!(model.fire("Reask", &mut s));
        assert_eq!(
            (s["phase"], s["ready"], s["owner_pid"]),
            (0, 0, 0),
            "{holder}: pending, the READY no answer, the fence gone"
        );
        assert!(model.fire("Announce", &mut s));
        assert!(
            !model.action_enabled("Terminate", &s),
            "{holder}: the gone process's READY signals nothing"
        );
        assert!(model.fire("Ready", &mut s));
        assert!(model.fire("Terminate", &mut s));
        for invariant in ["OnlyIssuerSignaled", "OnlyOnItsOwnAnswer"] {
            assert!(
                model.check_invariant(invariant, &s),
                "{holder}: {invariant}"
            );
        }
    }
    // While the notified process lives it is waited for, in either tab: a
    // wait the visit says, and no strand.
    for holder in ["Resume", "OtherTab"] {
        let s = run(&model, &["Announce", "Ready", holder]);
        assert!(!model.action_enabled("Reask", &s), "{holder}");
        assert!(!model.action_enabled("Terminate", &s), "{holder}");
        let mut looked = s.clone();
        assert!(model.fire("Look", &mut looked));
        assert_eq!(looked["stranded"], 0, "{holder}");
    }
    // Two live holders, or a partial scan: no re-ask either.
    for proof in ["DuplicateOwner", "IncompleteScan"] {
        let s = run(
            &model,
            &["Announce", "Ready", "OwnerExits", "OtherTab", proof],
        );
        assert!(!model.action_enabled("Reask", &s), "{proof}");
        assert!(!model.action_enabled("Terminate", &s), "{proof}");
    }
    // Main before the fix: the visit waits for good.
    let stuck = interp::with_consts(&model, &[("NoReask", 1)]);
    let mut s = run(&stuck, &["Announce", "Ready", "OwnerExits", "Resume"]);
    assert!(!stuck.action_enabled("Reask", &s));
    assert!(stuck.fire("Look", &mut s));
    assert!(!stuck.check_invariant("NeverWaitsOnAGoneNotice", &s));
    // The wrong fix: the new process signalled on the gone one's READY.
    let kept = interp::with_consts(&model, &[("KeepRound", 1)]);
    let mut s = run(
        &kept,
        &[
            "Announce",
            "Ready",
            "OwnerExits",
            "OtherTab",
            "Reask",
            "Announce",
        ],
    );
    assert!(kept.fire("Terminate", &mut s));
    assert!(!kept.check_invariant("OnlyOnItsOwnAnswer", &s));
}

/// THE VISIT'S WAIT IS THE REDUCER'S OWN GUARDS NEGATED: on every reachable
/// state with its proofs complete, `Look` is enabled exactly where neither
/// `Reask` nor `Terminate` is, at the committed configuration, under `Buggy`
/// and under each knob.
#[test]
fn the_notice_owners_wait_is_derived_from_its_guards() {
    let model = harness_upgrade_notice_owner_model();
    let configs: Vec<(&str, Model)> = std::iter::once(("committed", model.clone()))
        .chain(std::iter::once(("Buggy", interp::with_buggy(&model, 1))))
        .chain(
            KNOBS
                .iter()
                .map(|(knob, _)| (*knob, interp::with_consts(&model, &[(knob, 1)]))),
        )
        .collect();
    for (name, m) in &configs {
        let mut looked = 0;
        for s in reachable(m) {
            let visit = s["phase"] == 1
                && s["pid"] > 0
                && s["owners"] == 1
                && s["scan_complete"] == 1
                && s["stranded"] == 0;
            let look = m.action_enabled("Look", &s);
            if visit {
                assert_eq!(
                    look,
                    !m.action_enabled("Reask", &s) && !m.action_enabled("Terminate", &s),
                    "{name}: {s:?}"
                );
            } else {
                assert!(!look, "{name}: {s:?}");
            }
            looked += usize::from(look);
        }
        assert!(looked > 0, "{name}: a visit waits somewhere");
    }
}
