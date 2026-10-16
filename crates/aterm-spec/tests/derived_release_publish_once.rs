// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `ReleasePublishOnce`: a cut publishes once, owns the channel's `latest`,
//! and finishes every head it makes. Tier-1 lives in
//! `crates/aterm-release/tests/it/channel_latest.rs`.

use aterm_spec::derive::release_publish_once_model;
use aterm_spec::{interp, verify};

use crate::conjunct_audit::audit_guard_conjuncts;
use crate::walk::{committed_actions, walk};

#[test]
fn publish_once_proves_and_catches_every_defect_class() {
    let model = release_publish_once_model();
    verify::prove_and_catch_scalar(&model, "release publish once");
    // Every invariant is falsified by a mutant checked alone, and every mutant is an
    // independently caught negative control: dead at `Buggy = 0`, live when added alone to the
    // committed machine, and caught there.
    assert!(
        verify::uncaught_invariants(&model).is_empty(),
        "ghost invariants: {:?}",
        verify::uncaught_invariants(&model)
    );
    let committed = committed_actions(&model);
    let mutants: Vec<&str> = model
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !committed.contains(name))
        .collect();
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(mutants.len()),
        "{mutants:?}"
    );
    assert_eq!(mutants.len(), 11, "{mutants:?}");

    // A head the cut made always reaches completion, and the resume that refused its own head
    // strands the lease (`--abandon` refuses a published release). The liveness half runs without
    // the invariants, so its `Buggy = 1` verdict on both tiers is the wedge itself.
    let mut liveness = model.clone();
    liveness.invariants.clear();
    let is_final = |state: &interp::State| state["phase"] == 5 || state["phase"] == 6;
    verify::deadlock_free_and_catches_tiered(&liveness, is_final, "release publish once");
    // …and the re-sent PATCH is what that takes: a resume that only reads (proves the head,
    // sends nothing) wedges once another release has taken `latest` since.
    let mut read_only = interp::with_buggy(&liveness, 0);
    read_only
        .actions
        .retain(|action| action.name != "ReassertHead");
    let wedge = interp::find_deadlock(&read_only, is_final).expect("the read-only resume wedges");
    assert_eq!(
        (wedge["phase"], wedge["headed"], wedge["latest"]),
        (3, 1, 0),
        "{wedge:?}"
    );

    // The committed order reaches the head, and only through the proof and the PATCH.
    let healthy = walk(
        &model,
        &[
            "PublishSource",
            "AcquireLease",
            "Ratchet",
            "Bind",
            "UploadSig",
            "UploadToml",
            "ProveAssets",
            "Ratchet",
            "MakeHead",
            "ProveHead",
        ],
    );
    assert_eq!(
        (healthy["latest"], healthy["prerelease"], healthy["phase"]),
        (1, 0, 6)
    );
    // The appcast never lands before its signature, nothing is proved before the
    // appcast pair is up, no head is made of unproved bytes, and a newer head is
    // refused.
    let mut order = walk(&model, &["PublishSource", "AcquireLease"]);
    assert!(
        !model.action_enabled("Bind", &order),
        "nothing is bound before the ratchet"
    );
    for action in ["Ratchet", "Bind"] {
        assert!(model.fire(action, &mut order), "{action}");
    }
    assert!(!model.action_enabled("UploadToml", &order));
    assert!(!model.action_enabled("ProveAssets", &order));
    for action in ["UploadSig", "UploadToml"] {
        assert!(model.fire(action, &mut order), "{action}");
    }
    assert!(
        !model.action_enabled("MakeHead", &order),
        "no head PATCH before the proof"
    );
    let mut newer = walk(&model, &["PublishSource", "NewerRelease", "AcquireLease"]);
    assert!(!model.action_enabled("Ratchet", &newer));
    assert!(model.fire("RefuseNewer", &mut newer));
    assert_eq!(newer["latest"], 2, "the newer head keeps `latest`");
    assert_eq!(newer["bound"], 0, "and the refused cut bound nothing");

    // The second floor read: a join during the uploads makes the first read stale, the PATCH
    // waits on a read after it, and that read refuses, from a release carrying this cut's
    // uploads, which an abandon then withdraws, leaving the engine's release.
    let mut joined = walk(
        &model,
        &[
            "PublishSource",
            "AcquireLease",
            "Ratchet",
            "Bind",
            "UploadSig",
            "RosterJoin",
            "UploadToml",
            "ProveAssets",
        ],
    );
    assert!(!model.action_enabled("MakeHead", &joined), "{joined:?}");
    assert!(!model.action_enabled("Ratchet", &joined), "{joined:?}");
    for action in ["RefuseFloor", "Abandon"] {
        assert!(model.fire(action, &mut joined), "{action}: {joined:?}");
    }
    assert_eq!(
        (
            joined["source"],
            joined["prerelease"],
            joined["sig_up"],
            joined["toml_up"],
            joined["latest"]
        ),
        (1, 1, 0, 0, 0),
        "{joined:?}"
    );

    // The resume after the PATCH: the pointer gate ran out of time, another release took
    // `latest` and a join re-dressed the channel since, and the cut still finishes: the PATCH
    // re-sent, the read side proved, no floor read to refuse it.
    let mut resumed = walk(
        &model,
        &[
            "PublishSource",
            "AcquireLease",
            "Ratchet",
            "Bind",
            "UploadSig",
            "UploadToml",
            "ProveAssets",
            "Ratchet",
            "MakeHead",
            "OtherTakesLatest",
            "RosterJoin",
        ],
    );
    assert!(
        !model.action_enabled("Abandon", &resumed),
        "abandon refuses a head"
    );
    assert!(
        !model.action_enabled("ProveHead", &resumed),
        "the pointer names another release"
    );
    for action in ["ReassertHead", "ProveHead"] {
        assert!(model.fire(action, &mut resumed), "{action}: {resumed:?}");
    }
    assert_eq!(resumed["phase"], 6);

    // An abandon withdraws what the cut uploaded and leaves the engine's release.
    let abandoned = walk(
        &model,
        &[
            "PublishSource",
            "AcquireLease",
            "Ratchet",
            "Bind",
            "UploadSig",
            "Abandon",
        ],
    );
    assert_eq!(
        (
            abandoned["source"],
            abandoned["prerelease"],
            abandoned["sig_up"]
        ),
        (1, 1, 0)
    );

    // Each defect, on its own, is caught by the invariant named for it.
    let buggy = interp::with_buggy(&model, 1);
    let caught = |path: &[&str], invariant: &str| {
        let state = walk(&buggy, path);
        assert!(
            !buggy.check_invariant(invariant, &state),
            "{invariant} must catch {path:?}: {state:?}"
        );
    };
    let lease = ["PublishSource", "AcquireLease", "Ratchet", "Bind"];
    let with = |tail: &[&'static str]| -> Vec<&'static str> {
        lease.iter().copied().chain(tail.iter().copied()).collect()
    };
    // `PublishSourceFull`: a full source release takes `latest` empty.
    caught(&["PublishSourceFull"], "NoEmptyHead");
    // The head PATCH before the appcast pair.
    caught(&with(&["MakeHeadEarly"]), "NoEmptyHead");
    // The appcast before its signature: a client could read an unsigned head.
    caught(&with(&["UploadTomlFirst"]), "SignatureBeforeAppcast");
    // A proof taken before the last upload is not a proof of what is published.
    caught(
        &with(&["UploadSig", "ProveBeforeLastUpload"]),
        "ProofAfterTheLastUpload",
    );
    // `MakeHeadUnproved`: bytes made the head that nothing re-read; the channel's own proof
    // is the only line between them and the fleet.
    caught(
        &with(&["UploadSig", "UploadToml", "MakeHeadUnproved"]),
        "NoUnprovedHead",
    );
    // One floor read: a join during the uploads, and the PATCH on the read before it.
    caught(
        &with(&[
            "UploadSig",
            "RosterJoin",
            "UploadToml",
            "ProveAssets",
            "MakeHeadStaleFloors",
        ]),
        "NeverBehindTheFleet",
    );
    // `FinishWithoutHead`: everything uploaded, no PATCH.
    caught(
        &with(&[
            "UploadSig",
            "UploadToml",
            "ProveAssets",
            "FinishWithoutHead",
        ]),
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
            "ProveAssets",
            "MakeHead",
        ],
        "NeverDisplacesNewer",
    );
    // The bind before the ratchet: a cut the floors then refuse has already recorded
    // the adopted release in its journal.
    caught(
        &["PublishSource", "AcquireLease", "BindBlind"],
        "BoundOnAFloorRead",
    );
    // An abandon that deletes the engine's release: the recut has nothing to adopt.
    caught(
        &with(&["AbandonDeletesRelease"]),
        "AbandonLeavesTheSourceRelease",
    );
    // `ResumeRefusesItsOwnHead`: after the PATCH and a join, the write half re-enters and the
    // floors refuse a head the fleet already runs.
    caught(
        &with(&[
            "UploadSig",
            "UploadToml",
            "ProveAssets",
            "Ratchet",
            "MakeHead",
            "RosterJoin",
            "ResumeRefusesItsOwnHead",
        ]),
        "MadeHeadFinishes",
    );
}

/// The guard-conjunct audit (`common/conjunct_audit.rs`), pinned: for every conjunct of every
/// guard the committed machine takes, which invariants dropping it breaks (each checked alone)
/// and whether it changes a real transition. Every conjunct that carries nothing is named below
/// with the reason it is there.
#[test]
fn publish_once_guard_conjuncts_carry_the_order_they_claim() {
    let model = release_publish_once_model();
    let audited: Vec<&str> = committed_actions(&model).into_iter().collect();
    let report = audit_guard_conjuncts(&model, Some(&audited));
    let none: &[&str] = &[];
    // (action, conjunct, the invariants it carries, changes a transition)
    let declared: &[(&str, usize, &[&str], bool)] = &[
        // Sequencing: the phase a writer acts in. Each carries what acting out of
        // phase would break.
        (
            "PublishSource",
            0,
            &["NeverDisplacesNewer", "MadeHeadFinishes"],
            true,
        ),
        (
            "NewerRelease",
            0,
            &["NeverDisplacesNewer", "PublishedMeansHead"],
            true,
        ),
        ("NewerRelease", 1, none, false),
        // When a join can land (any time until the cut finishes): environment, bound at
        // Tier-1 by the fake's join between the real calls.
        ("RosterJoin", 0, none, true),
        ("RosterJoin", 1, none, true),
        ("RosterJoin", 2, none, false),
        (
            "AcquireLease",
            0,
            &["AbandonLeavesTheSourceRelease", "MadeHeadFinishes"],
            true,
        ),
        // The floors: the head half and the roster half of the real ratchet.
        ("Ratchet", 0, &["NeverDisplacesNewer"], true),
        ("Ratchet", 1, &["NeverDisplacesNewer"], true),
        ("Ratchet", 2, &["NeverBehindTheFleet"], true),
        ("RefuseNewer", 0, none, true),
        ("RefuseNewer", 1, none, true),
        // A join is refused only before the head: from phase 3 it is the resume that refused
        // its own head (`channel::ChannelRelease::head_made` is the real guard).
        ("RefuseFloor", 0, &["MadeHeadFinishes"], true),
        ("RefuseFloor", 1, none, true),
        // The bind after the floors.
        ("Bind", 0, none, true),
        ("Bind", 1, &["BoundOnAFloorRead"], true),
        ("Bind", 2, none, false),
        // Uploads go to a bound release, in phase; the signature is one-shot.
        ("UploadSig", 0, none, true),
        ("UploadSig", 1, none, true),
        ("UploadSig", 2, none, true),
        ("UploadToml", 0, none, true),
        // The signature before the appcast.
        (
            "UploadToml",
            1,
            &["NoEmptyHead", "SignatureBeforeAppcast"],
            true,
        ),
        ("UploadToml", 2, none, true),
        ("ProveAssets", 0, none, true),
        // The proof after the last upload.
        (
            "ProveAssets",
            1,
            &["NoEmptyHead", "ProofAfterTheLastUpload", "NoUnprovedHead"],
            true,
        ),
        ("ProveAssets", 2, none, false),
        ("MakeHead", 0, none, true),
        // The PATCH on a floor read after the uploads.
        ("MakeHead", 1, &["NeverBehindTheFleet"], true),
        // The PATCH on a proof.
        ("MakeHead", 2, &["NoEmptyHead", "NoUnprovedHead"], true),
        // Another release takes `latest` only from a head this cut made, before it is
        // proved (environment, bound at Tier-1 by the fake's full `atpkg-index` release).
        ("OtherTakesLatest", 0, &["PublishedMeansHead"], true),
        ("OtherTakesLatest", 1, none, false),
        // The re-sent PATCH only over a head this cut made.
        (
            "ReassertHead",
            0,
            &["NoEmptyHead", "AbandonLeavesTheSourceRelease"],
            true,
        ),
        // Only a head this cut made holds `latest` at `Buggy = 0`, so the phase adds
        // nothing to the pointer: the proof reads as the step after the head.
        ("ProveHead", 0, none, false),
        // The pointer names this release before the cut is done.
        ("ProveHead", 1, &["PublishedMeansHead"], true),
        ("Abandon", 0, &["AbandonLeavesTheSourceRelease"], true),
        // `--abandon` refuses a published release. At `Buggy = 0` no refused cut holds a head,
        // so this changes nothing; its weight is the liveness catch above: with it, the resume
        // that refused its own head is a wedge, the stranded lease.
        ("Abandon", 1, none, false),
        ("Done", 0, none, false),
    ];
    let measured: Vec<(&str, usize, &[&str], bool)> = report
        .iter()
        .map(|row| {
            (
                row.action,
                row.index,
                row.carries.as_slice(),
                row.changes_behaviour,
            )
        })
        .collect();
    assert_eq!(measured, declared, "{report:#?}");
}
