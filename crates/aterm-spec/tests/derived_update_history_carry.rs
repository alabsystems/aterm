// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateHistoryCarry` (2026-09-26): a self-update carries
//! every history line of a tab, or counts it. The committed machine proves
//! over its whole bounded space, and each of the four defects its `Buggy`
//! member stands for is caught on its own path: a fallback that counts
//! nothing (the silent loss the carry was written to end), a sidecar named
//! over a history that moved under it, a failed sidecar dropped without a
//! count, and an import that puts back a history the successor's pane cleared
//! before it landed. Tier-1 — the real export, park, join, sidecar and import — is
//! `aterm-gui/src/handoff_history_conformance.rs`.

use aterm_spec::derive::{Model, native_update_history_carry_model};
use aterm_spec::{interp, verify};

type State = std::collections::BTreeMap<&'static str, i64>;

fn run(m: &Model, path: &[&str]) -> State {
    let mut s = m.init_state();
    for action in path {
        assert!(m.fire(action, &mut s), "{action} is enabled on {s:?}");
    }
    s
}

#[test]
fn the_history_carry_proves_catches_and_has_no_dead_action() {
    let model = native_update_history_carry_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the history carry must stay enrolled in the spec-link registry"
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "history carry: every line crosses or is counted, and a sidecar only over its own history",
    );
}

#[test]
fn a_quiet_handoff_carries_everything_and_counts_nothing() {
    let m = native_update_history_carry_model();
    let s = run(&m, &["Export", "Park", "Settle"]);
    assert_eq!((s["take"], s["dropped"], s["failed"]), (1, 0, 0), "{s:?}");
    assert_eq!(s["held"], s["total"], "{s:?}");
    // Output the checkpoint still carries meets the export.
    let s = run(&m, &["Export", "Output", "Park", "Settle"]);
    assert_eq!((s["held"], s["total"], s["dropped"]), (3, 3, 0), "{s:?}");
}

#[test]
fn every_fallback_is_counted() {
    let m = native_update_history_carry_model();
    for path in [
        &["Export", "Rewrap", "Park", "Settle"][..],
        &["Export", "Output", "Output", "Park", "Settle"][..],
        &["Export", "Park", "Corrupt", "Settle"][..],
    ] {
        let s = run(&m, path);
        assert!(
            s["dropped"] + s["failed"] > 0,
            "{path:?} leaves lines behind: {s:?}"
        );
        assert_eq!(
            s["held"] + s["dropped"] + s["failed"],
            s["total"],
            "{path:?}: {s:?}"
        );
    }
}

#[test]
fn the_three_defects_are_caught_each_on_its_own_path() {
    let m = interp::with_buggy(&native_update_history_carry_model(), 1);
    // The silent fallback: more output than the checkpoint carries, and the
    // export is thrown away with nothing counted.
    let s = run(&m, &["Export", "Output", "Output", "Park", "Settle"]);
    assert!(!m.check_invariant("NothingSilentlyLost", &s), "{s:?}");
    assert_eq!((s["dropped"], s["held"], s["total"]), (0, 1, 4), "{s:?}");
    // The sidecar named over a history that moved under it.
    let s = run(&m, &["Export", "Rewrap", "Park"]);
    assert!(
        !m.check_invariant("SidecarOnlyOverItsOwnHistory", &s),
        "{s:?}"
    );
    assert_eq!((s["moved"], s["take"]), (1, 1), "{s:?}");
    // The sidecar that failed its sha, dropped without a count.
    let s = run(&m, &["Export", "Park", "Corrupt", "Settle"]);
    assert!(!m.check_invariant("NothingSilentlyLost", &s), "{s:?}");
    assert_eq!((s["failed"], s["held"], s["total"]), (0, 1, 2), "{s:?}");
    // The import that lands after the pane cleared its scrollback.
    let s = run(&m, &["Export", "Park", "Clear", "Settle"]);
    assert!(!m.check_invariant("ClearedStaysCleared", &s), "{s:?}");
    assert_eq!((s["cleared"], s["held"]), (1, 1), "{s:?}");
}

#[test]
fn a_pane_that_clears_before_the_import_holds_nothing_and_loses_nothing() {
    let m = native_update_history_carry_model();
    for path in [
        &["Export", "Park", "Clear", "Settle"][..],
        &["Export", "Park", "Corrupt", "Clear", "Settle"][..],
        &["Export", "Output", "Output", "Park", "Clear", "Settle"][..],
    ] {
        let s = run(&m, path);
        assert_eq!((s["held"], s["failed"]), (0, 0), "{path:?}: {s:?}");
        assert!(m.check_invariant("ClearedStaysCleared", &s), "{path:?}");
        assert!(m.check_invariant("NothingSilentlyLost", &s), "{path:?}");
    }
}
