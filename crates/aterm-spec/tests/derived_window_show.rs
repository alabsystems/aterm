// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `NativeUpdateWindowShow`
//! (`aterm_spec::derive::native_update_window_show_model`): the window show
//! state a self-update carries (gap #29). Proved at `Buggy = 0` and caught at
//! `Buggy = 1` on every invariant; the update's paths are walked as the
//! owner's schedules. Tier-1 is `aterm-gui/src/window_show_conformance.rs`.

use aterm_spec::derive::{Model, native_update_window_show_model};
use aterm_spec::{interp, verify};

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

#[test]
fn window_show_proves_catches_and_has_no_dead_action() {
    let model = native_update_window_show_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the window show carry must stay enrolled in the spec-link registry"
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "window show: stacked on glass, keyed at Commit, full screen only after Commit and in front",
    );
}

/// The owner's update, in the three ways it meets the user. In aterm: the
/// stack goes on with the proof, Commit, and full screen comes back with aterm
/// in front. In another app: full screen waits for aterm to be in front.
/// Typing into another window before Commit: that window keeps the keyboard.
#[test]
fn window_show_walks_the_update_the_way_the_user_meets_it() {
    let m = native_update_window_show_model();

    let mut in_aterm = m.init_state();
    for action in [
        "Launch",
        "Reveal",
        "Prove",
        "Stack",
        "Activate",
        "Commit",
        "EnterFullScreen",
    ] {
        assert!(m.fire(action, &mut in_aterm), "{action}: {in_aterm:?}");
    }
    assert_eq!(
        (in_aterm["keyed"], in_aterm["fs"], in_aterm["fsfront"]),
        (1, 1, 1)
    );

    let mut elsewhere = m.init_state();
    for action in ["Launch", "Reveal", "Prove", "Stack", "Commit"] {
        assert!(m.fire(action, &mut elsewhere), "{action}: {elsewhere:?}");
    }
    assert!(
        !m.action_enabled("EnterFullScreen", &elsewhere),
        "the user is in another app: no Space is entered"
    );
    assert!(m.fire("Activate", &mut elsewhere));
    assert!(m.action_enabled("EnterFullScreen", &elsewhere));

    let mut typed = m.init_state();
    for action in [
        "Launch", "Reveal", "Prove", "Stack", "Steal", "Type", "Commit",
    ] {
        assert!(m.fire(action, &mut typed), "{action}: {typed:?}");
    }
    assert_eq!(
        (typed["keyed"], typed["typed"]),
        (0, 1),
        "the window typed into keeps the keyboard"
    );

    let mut stolen = m.init_state();
    for action in ["Launch", "Reveal", "Prove", "Stack", "Steal", "Commit"] {
        assert!(m.fire(action, &mut stolen), "{action}: {stolen:?}");
    }
    assert_eq!(stolen["keyed"], 1, "an untyped move is undone at Commit");

    let mut early = m.init_state();
    assert!(m.fire("Launch", &mut early));
    assert!(
        !m.action_enabled("Stack", &early),
        "nothing is raised before its first present"
    );
    assert!(m.fire("Reveal", &mut early));
    assert!(
        !m.action_enabled("Stack", &early),
        "on glass, but the proof is not written: nothing is minimized or raised yet"
    );
    assert!(
        !m.action_enabled("Commit", &early),
        "the outgoing process commits only on a proof"
    );
    for action in ["Prove", "Stack", "Activate"] {
        assert!(m.fire(action, &mut early), "{action}: {early:?}");
    }
    assert!(
        !m.action_enabled("EnterFullScreen", &early),
        "in front, but not committed: no full screen while the parked process shares the PTYs"
    );
}

/// `Buggy = 1` — raise before the reveal or before the proof, full screen at
/// the stack, the key left where the reveal order put it, and a Commit that
/// re-keys over typing — is caught on each invariant by its own schedule.
#[test]
fn window_show_the_defects_are_caught_on_every_invariant() {
    let m = interp::with_buggy(&native_update_window_show_model(), 1);
    let (a, _) = interp::bmc(&only(&m, "StackOnlyOnGlass")).expect_err("raised early");
    assert_eq!((a["stacked"], a["revealed"]), (1, 0), "{a:?}");
    let (p, _) = interp::bmc(&only(&m, "StackOnlyAtProof")).expect_err("before the proof");
    assert_eq!((p["lane"], p["stacked"], p["proof"]), (1, 1, 0), "{p:?}");
    let (b, _) = interp::bmc(&only(&m, "FullScreenOnlyAfterCommit")).expect_err("early fs");
    assert_eq!((b["fs"], b["committed"]), (1, 0), "{b:?}");
    let (c, _) = interp::bmc(&only(&m, "FullScreenOnlyInFront")).expect_err("away fs");
    assert_eq!((c["fs"], c["fsfront"]), (1, 0), "{c:?}");
    let (d, _) = interp::bmc(&only(&m, "KeyWindowAtCommit")).expect_err("reveal order");
    assert_eq!(
        (d["committed"], d["stacked"], d["typed"], d["keyed"]),
        (1, 1, 0, 0),
        "{d:?}"
    );
    let (e, _) = interp::bmc(&only(&m, "TypingKeepsItsWindow")).expect_err("re-keyed");
    assert_eq!((e["committed"], e["typed"], e["keyed"]), (1, 1, 1), "{e:?}");
    assert!(verify::uncaught_invariants(&native_update_window_show_model()).is_empty());
}
