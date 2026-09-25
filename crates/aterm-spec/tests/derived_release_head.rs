// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `ReleaseChannelHead`: the cut owns the public channel's `latest`.
//! Tier-1 lives in `crates/aterm-release/tests/channel_latest.rs`.

use aterm_spec::{derive::release_channel_head_model, interp, verify};

#[test]
fn the_cut_owns_latest_proves_and_catches_every_defect_class() {
    let model = release_channel_head_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the channel-head contract must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "release channel head");

    // The shipped order reaches the head, and only through the head PATCH.
    let mut healthy = model.init_state();
    for action in [
        "PublishSource",
        "AcquireLease",
        "Ratchet",
        "Bind",
        "UploadSig",
        "UploadToml",
        "MakeHead",
    ] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
    }
    assert_eq!((healthy["latest"], healthy["prerelease"]), (1, 0));
    // The appcast never lands before its signature, and a newer head is refused.
    let mut order = model.init_state();
    for action in ["PublishSource", "AcquireLease"] {
        assert!(model.fire(action, &mut order), "{action}");
    }
    assert!(
        !model.action_enabled("Bind", &order),
        "nothing is bound before the ratchet"
    );
    for action in ["Ratchet", "Bind"] {
        assert!(model.fire(action, &mut order), "{action}");
    }
    assert!(!model.action_enabled("UploadToml", &order));
    let mut newer = model.init_state();
    for action in ["PublishSource", "NewerRelease", "AcquireLease"] {
        assert!(model.fire(action, &mut newer), "{action}");
    }
    assert!(!model.action_enabled("Ratchet", &newer));
    assert!(model.fire("RefuseNewer", &mut newer));
    assert_eq!(newer["latest"], 2, "the newer head keeps `latest`");
    assert_eq!(newer["bound"], 0, "and the refused cut bound nothing");

    // Each defect, on its own, is caught by the invariant named for it.
    let buggy = interp::with_buggy(&model, 1);
    let caught = |path: &[&str], invariant: &str| {
        let mut state = buggy.init_state();
        for action in path {
            assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
        }
        assert!(
            !buggy.check_invariant(invariant, &state),
            "{invariant} must catch {path:?}: {state:?}"
        );
    };
    // The engine before 2026-09-23: a full source release takes `latest` empty.
    caught(&["PublishSourceFull"], "NoEmptyHead");
    // The head PATCH before the appcast pair.
    caught(
        &[
            "PublishSource",
            "AcquireLease",
            "Ratchet",
            "Bind",
            "MakeHeadEarly",
        ],
        "NoEmptyHead",
    );
    // The adopt path before 2026-09-23: everything uploaded, no PATCH.
    caught(
        &[
            "PublishSource",
            "AcquireLease",
            "Ratchet",
            "Bind",
            "UploadSig",
            "UploadToml",
            "FinishWithoutHead",
        ],
        "PublishedMeansHead",
    );
    // A ratchet that does not look: `latest` taken from a newer release.
    caught(
        &[
            "PublishSource",
            "NewerRelease",
            "AcquireLease",
            "RatchetBlind",
            "Bind",
            "UploadSig",
            "UploadToml",
            "MakeHead",
        ],
        "NeverDisplacesNewer",
    );
    // The bind before the ratchet: a cut refused by the floors has already recorded
    // the adopted release in its journal.
    caught(
        &[
            "PublishSource",
            "NewerRelease",
            "AcquireLease",
            "BindBlind",
            "RefuseNewer",
        ],
        "RefusedUnuploadedBindsNothing",
    );
}
