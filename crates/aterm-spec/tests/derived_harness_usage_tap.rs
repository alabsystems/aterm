// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-0 of `HarnessUsageTap`: proved at `Buggy = 0`, caught at `Buggy = 1` on each invariant,
//! and one knob per defect caught alone by the invariant it breaks. Tier-1 is aterm-agent's
//! `conformance_usage_tap`, over the real gate, freshness rule, silence rule and recorder.

use crate::walk::{only, reachable, walk};
use aterm_spec::derive::harness_usage_tap_model;
use aterm_spec::{interp, verify};

const INVARIANTS: [&str; 5] = [
    "NeverNotesStale",
    "NeverArmsOverTheOwnersLine",
    "NoTapOnlyAfterServe",
    "NothingNotedUnattributed",
    "NoMarkOutlivesItsProcess",
];

/// The defects the rules keep out, one knob each, and the invariant that catches each alone.
const KNOBS: [(&str, &str); 5] = [
    ("NoFresh", "NeverNotesStale"),
    ("NoChainLook", "NeverArmsOverTheOwnersLine"),
    ("NoServeGate", "NoTapOnlyAfterServe"),
    ("NoAttrib", "NothingNotedUnattributed"),
    ("NoEndAtRelaunch", "NoMarkOutlivesItsProcess"),
];

#[test]
fn the_usage_tap_model_proves_and_catches() {
    let model = harness_usage_tap_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the model must stay enrolled in the spec-link registry"
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "harness usage tap: no stale reading recorded, never over the owner's line, silent only \
         after a served turn, nothing recorded off the machine's own login, no silent mark told \
         past its process",
    );
    for invariant in INVARIANTS {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 0)).is_ok(),
            "{invariant}"
        );
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 1)).is_err(),
            "{invariant} catches the defects"
        );
    }
}

/// Each knob alone breaks the invariant it names; the committed configuration breaks none.
#[test]
fn each_defect_is_caught_on_its_own() {
    let model = harness_usage_tap_model();
    for (knob, invariant) in KNOBS {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_consts(&alone, &[(knob, 0)])).is_ok(),
            "{knob} off"
        );
        let caught = interp::bmc(&interp::with_consts(&alone, &[(knob, 1)]));
        assert!(caught.is_err(), "{knob} is caught by {invariant}");
    }
}

/// The always-fresh read (the Tier-1 bind's negative control): a tapped process is served
/// once, read, then only re-reports what it holds; read again under `NoFresh` the idle repeat is
/// recorded, the stale record the rule keeps out, while the committed model leaves its books as
/// they were. At every reachable state the record is the largest figure read, never above the
/// account's.
#[test]
fn the_always_fresh_read_records_an_idle_repeat() {
    let m = harness_usage_tap_model();
    let ran = ["Launch", "Reply", "BgUsage", "Read", "IdleReport", "Read"];
    let s = walk(&m, &ran);
    assert_eq!((s["noted"], s["stale"], s["lpid"]), (1, 0, 1), "{s:?}");
    let bad = interp::with_consts(&m, &[("NoFresh", 1)]);
    let s = walk(&bad, &ran);
    assert_eq!(s["stale"], 1, "{s:?}");
    assert!(!bad.check_invariant("NeverNotesStale", &s));
    for s in reachable(&m) {
        assert!(s["figs"] >= s["noted"], "{s:?}");
        assert!(s["tapped"] == 0 || s["pid"] > 0, "{s:?}");
    }
}
