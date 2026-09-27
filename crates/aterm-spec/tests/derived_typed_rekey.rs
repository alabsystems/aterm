// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `TypedRekey` (`aterm_spec::derive::typed_rekey_model`): the typed
//! re-key of a shell spawned before the re-key channel. Proved at `Buggy = 0`
//! and caught at `Buggy = 1` on every invariant; the heal, the take-back and
//! the typed upgrade (2026-09-26) are walked as the owner's schedules. Tier-1 is
//! `aterm-gui/src/typed_rekey_conformance.rs`.

use aterm_spec::derive::{Model, typed_rekey_model};
use aterm_spec::{interp, verify};

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

#[test]
fn typed_rekey_proves_catches_and_has_no_dead_action() {
    let model = typed_rekey_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the typed re-key must stay enrolled in the spec-link registry"
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "typed re-key: a working key never moves, and no unread key outlives its file",
    );
}

/// The owner's two tabs: lost nonce, key issued, the relaunch line reads it,
/// the window settles it by its file — the shell and the engine share the key.
/// And the same tab whose line never ran: settled with the file still there,
/// the engine is back to no key, and the file is gone.
#[test]
fn typed_rekey_heals_a_read_key_and_takes_back_an_unread_one() {
    let m = typed_rekey_model();
    let mut healed = m.init_state();
    for action in ["Lose", "Issue", "Read", "Settle"] {
        assert!(m.fire(action, &mut healed), "{action}: {healed:?}");
    }
    assert_eq!((healed["sh"], healed["eng"], healed["file"]), (2, 2, 0));

    let mut unread = m.init_state();
    for action in ["Lose", "Issue", "Settle"] {
        assert!(m.fire(action, &mut unread), "{action}: {unread:?}");
    }
    assert_eq!((unread["sh"], unread["eng"], unread["file"]), (1, 0, 0));
    assert!(
        !m.action_enabled("Read", &unread),
        "a line after the settle finds nothing to read"
    );

    let mut healthy = m.init_state();
    assert!(m.fire("Keep", &mut healthy));
    assert!(
        !m.action_enabled("Issue", &healthy),
        "a tab whose marks verify is never re-keyed"
    );
}

/// THE TYPED UPGRADE (2026-09-26): a healthy tab whose shell predates loaders
/// is issued a file naming the loader, with the key it signs with first — read
/// or settled unread, the shell and the engine share that key and the file is
/// gone. A degraded tab is never issued the upgrade-only file (it is healed).
#[test]
fn typed_rekey_upgrades_a_healthy_shell_without_moving_its_key() {
    let m = typed_rekey_model();
    for tail in [&["Read", "Settle"][..], &["Settle"][..]] {
        let mut s = m.init_state();
        for action in ["Keep", "Upgrade"].iter().chain(tail) {
            assert!(m.fire(action, &mut s), "{action}: {s:?}");
        }
        assert_eq!(
            (s["sh"], s["eng"], s["file"], s["upg"]),
            (1, 1, 0, 1),
            "{tail:?}"
        );
    }
    let mut lost = m.init_state();
    assert!(m.fire("Lose", &mut lost));
    assert!(
        !m.action_enabled("Upgrade", &lost),
        "a degraded tab is healed, never upgraded without a key"
    );
}

/// `Buggy = 1` — the channel's semantics on a shell with no hook, and a
/// posture-blind issue — is caught on each invariant by its own schedule.
#[test]
fn typed_rekey_the_no_way_back_design_is_caught_on_every_invariant() {
    let m = interp::with_buggy(&typed_rekey_model(), 1);
    let (a, _) = interp::bmc(&only(&m, "WorkingKeyNeverMoved")).expect_err("moved");
    assert_eq!((a["working"], a["sh"], a["eng"]), (1, 1, 2), "{a:?}");
    let (b, _) = interp::bmc(&only(&m, "UnreadKeyTakenBack")).expect_err("standing");
    assert_eq!((b["settled"], b["sh"], b["eng"]), (1, 1, 2), "{b:?}");
    let (c, _) = interp::bmc(&only(&m, "NoLiveKeyInAFile")).expect_err("in a file");
    assert_eq!((c["settled"], c["file"], c["eng"]), (1, 1, 2), "{c:?}");
    assert!(verify::uncaught_invariants(&typed_rekey_model()).is_empty());
    // The upgrade's own defect: an EMPTY first line, which an older sweep's
    // key-only line reads as the key — the working shell left signing nothing.
    let mut empty = m.init_state();
    for action in ["Keep", "Upgrade", "Read"] {
        assert!(m.fire(action, &mut empty), "{action}: {empty:?}");
    }
    assert_eq!((empty["working"], empty["sh"], empty["eng"]), (1, 0, 1));
    assert!(!m.check_invariant("WorkingKeyNeverMoved", &empty));
}
