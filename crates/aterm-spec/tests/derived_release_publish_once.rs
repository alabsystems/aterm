// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `ReleasePublishOnce`: a cut publishes once, owns the channel's `latest`,
//! and finishes every head it makes. Tier-1 lives in
//! `crates/aterm-release/tests/it/channel_latest.rs`.

use std::collections::BTreeSet;

use aterm_spec::derive::{Model, release_publish_once_model};
use aterm_spec::{interp, verify};

#[path = "common/conjunct_audit.rs"]
mod conjunct_audit;
use conjunct_audit::audit_guard_conjuncts;

/// Every action the committed machine (`Buggy = 0`) can take — the audit's subject.
/// The others are the negative controls, each audited below as independently caught.
fn committed_actions(m: &Model) -> BTreeSet<&'static str> {
    interp::fired_actions(&interp::with_buggy(m, 0))
}

fn walk(m: &Model, path: &[&str]) -> interp::State {
    let mut state = m.init_state();
    for action in path {
        assert!(m.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

#[test]
fn publish_once_proves_and_catches_every_defect_class() {
    let model = release_publish_once_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the publish-once contract must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "release publish once");
    // Every invariant is falsified by a mutant checked ALONE, and every mutant is an
    // independently caught negative control: dead at `Buggy = 0`, live when added alone
    // to the committed machine, and caught there.
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
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "{mutants:?}"
    );
    assert_eq!(mutants.len(), 11, "{mutants:?}");

    // A head the cut made can always reach completion: nothing wedges at `Buggy = 0`,
    // and the resume that refused its own head strands the lease (`--abandon` refuses a
    // published release). The liveness half runs on the same machine WITHOUT its
    // invariants, so the `Buggy = 1` verdict on both tiers is the wedge itself, never an
    // earlier safety counterexample.
    let mut liveness = model.clone();
    liveness.invariants.clear();
    let is_final = |state: &interp::State| state["phase"] == 5 || state["phase"] == 6;
    verify::deadlock_free_and_catches_tiered(&liveness, is_final, "release publish once");
    // …and the re-sent PATCH is what that takes: a resume that only READS (proves the
    // head, sends nothing) wedges once another release has taken `latest` since.
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

    // The shipped order reaches the head, and only through the proof and the PATCH.
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

    // THE SECOND FLOOR READ: a join during the uploads makes the first read stale, the
    // PATCH waits on a read after it, and that read refuses — from a release carrying
    // this cut's uploads, which an abandon then withdraws, leaving the engine's release.
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

    // THE RESUME AFTER THE PATCH: the pointer gate ran out of time, another release took
    // `latest` and a join re-dressed the channel since, and the cut still finishes — the
    // PATCH re-sent, the read side proved, no floor read to refuse it.
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
    // The engine before 2026-09-23: a full source release takes `latest` empty.
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
    // THE DEFECT THE DOUBLE PATH GUARDED: bytes made the head that nothing re-read.
    // The origin's live verify stood between them and the fleet before the mirror
    // copied a release; with one publication the channel's own proof is that line.
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
    // The adopt path before 2026-09-23: everything uploaded, no PATCH.
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
    // The resume before 2026-09-26: after the PATCH and a join, the write half
    // re-entered and the floors refused a head the fleet already runs.
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

/// THE GUARD-CONJUNCT AUDIT (`common/conjunct_audit.rs`), pinned: for every conjunct of
/// every guard the committed machine takes, which invariants dropping it breaks (each
/// checked alone) and whether it changes a real transition. The ORDER the model exists
/// to prove carries weight — the signature before the appcast (`UploadToml`'s `sig_up`),
/// the proof after the last upload (`ProveAssets`' `toml_up`), the bind after the floors
/// (`Bind`'s `checked`), the PATCH on a floor read after the uploads (`MakeHead`'s
/// `checked`, which only a join makes stale), a join refused only before the head
/// (`RefuseFloor`'s `phase`) — and every conjunct that carries nothing is named below
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
        // THE FLOORS: the head half and the roster half of the real ratchet.
        ("Ratchet", 0, &["NeverDisplacesNewer"], true),
        ("Ratchet", 1, &["NeverDisplacesNewer"], true),
        ("Ratchet", 2, &["NeverBehindTheFleet"], true),
        ("RefuseNewer", 0, none, true),
        ("RefuseNewer", 1, none, true),
        // A join is refused only BEFORE the head: from phase 3 it is the resume that
        // refused its own head (`channel::ChannelRelease::head_made` is the real guard).
        ("RefuseFloor", 0, &["MadeHeadFinishes"], true),
        ("RefuseFloor", 1, none, true),
        // THE BIND AFTER THE FLOORS.
        ("Bind", 0, none, true),
        ("Bind", 1, &["BoundOnAFloorRead"], true),
        ("Bind", 2, none, false),
        // Uploads go to a bound release, in phase; the signature is one-shot.
        ("UploadSig", 0, none, true),
        ("UploadSig", 1, none, true),
        ("UploadSig", 2, none, true),
        ("UploadToml", 0, none, true),
        // THE SIGNATURE BEFORE THE APPCAST.
        (
            "UploadToml",
            1,
            &["NoEmptyHead", "SignatureBeforeAppcast"],
            true,
        ),
        ("UploadToml", 2, none, true),
        ("ProveAssets", 0, none, true),
        // THE PROOF AFTER THE LAST UPLOAD.
        (
            "ProveAssets",
            1,
            &["NoEmptyHead", "ProofAfterTheLastUpload", "NoUnprovedHead"],
            true,
        ),
        ("ProveAssets", 2, none, false),
        ("MakeHead", 0, none, true),
        // THE PATCH ON A FLOOR READ AFTER THE UPLOADS.
        ("MakeHead", 1, &["NeverBehindTheFleet"], true),
        // THE PATCH ON A PROOF.
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
        // THE POINTER NAMES THIS RELEASE before the cut is done.
        ("ProveHead", 1, &["PublishedMeansHead"], true),
        ("Abandon", 0, &["AbandonLeavesTheSourceRelease"], true),
        // `--abandon` refuses a published release. At `Buggy = 0` no refused cut holds
        // a head, so this changes nothing; its weight is the liveness catch above — with
        // it, the resume that refused its own head is a WEDGE, the stranded lease.
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
