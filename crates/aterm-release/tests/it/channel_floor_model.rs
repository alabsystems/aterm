// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the release-channel floor lifecycle. The production
//! resolver and late guard remain in `publish.rs`; this test projects their real
//! decisions onto the single-source derived model and keeps explicit mutants.

use crate::{ledger, manifest_out, publish, verify};

use aterm_spec::derive::{
    Model, release_channel_floor_model, release_published_identity_model,
    release_yank_successor_first_model,
};
use ledger::{GitRunner, RunOut};
use std::collections::VecDeque;
use std::sync::Mutex;

fn numeric(floor: Option<u64>) -> i64 {
    i64::try_from(floor.unwrap_or(0)).expect("bounded floor fits i64")
}

fn tier1_step(
    model: &Model,
    before: &aterm_spec::interp::State,
    action: &str,
    label: &str,
) -> aterm_spec::interp::State {
    assert!(
        model.action_enabled(action, before),
        "model disabled {action}"
    );
    let after = model.successors(action, before)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        model,
        &[],
        before,
        &after,
        Some(action),
        label,
    );
    assert!(admitted, "model rejected {label}: {why}");
    after
}

fn resolver_state(
    model: &Model,
    operator: Option<u64>,
    observed: Option<u64>,
    claimed: u64,
) -> aterm_spec::interp::State {
    let mut state = model.init_state();
    state.insert("operator_floor", numeric(operator));
    state.insert("observed_floor", numeric(observed));
    state.insert("latest_floor", numeric(observed));
    state.insert(
        "claimed_build",
        i64::try_from(claimed).expect("bounded claim fits i64"),
    );
    state
}

fn frozen_state(
    model: &Model,
    carried: Option<u64>,
    newest: Option<u64>,
) -> aterm_spec::interp::State {
    let mut state = model.init_state();
    state.insert("phase", 1);
    state.insert("claimed_build", 4);
    state.insert("frozen_floor", numeric(carried));
    state.insert("journal_floor", numeric(carried));
    state.insert("latest_floor", numeric(newest));
    // The pure guard is modeled at its required call site: inside the release
    // lease that must remain held through PublishChecked.
    state.insert("lease_owned", 1);
    state
}

struct LeaseScript {
    replies: Mutex<VecDeque<RunOut>>,
}

impl LeaseScript {
    fn new(replies: Vec<RunOut>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
        }
    }
}

impl GitRunner for LeaseScript {
    fn git(&self, _args: &[&str]) -> ledger::Result<RunOut> {
        self.replies
            .lock()
            .expect("lease replies lock")
            .pop_front()
            .ok_or_else(|| ledger::Error::new("unexpected lease command"))
    }
}

fn lease_owner_row(owner: &str) -> RunOut {
    RunOut {
        status: 0,
        stdout: format!("{owner}\t{}\n", publish::RELEASE_LEASE_REF).into_bytes(),
        stderr: Vec::new(),
    }
}

fn command_ok() -> RunOut {
    RunOut {
        status: 0,
        stdout: Vec::new(),
        stderr: Vec::new(),
    }
}

/// Exhaustively bind the real carry-forward resolver to the model over the whole
/// bounded domain. `None` and `Some(0)` deliberately project to the same canonical
/// zero state, matching the manifest wire policy.
#[test]
fn effective_min_build_conforms_to_release_channel_floor_model() {
    let model = release_channel_floor_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let floors: Vec<Option<u64>> = std::iter::once(None).chain((0..=4).map(Some)).collect();
    let mut dropped_observed_control = false;
    let mut over_claim_control = false;

    for operator in &floors {
        for observed in &floors {
            for claimed in 0..=4 {
                let before = resolver_state(&model, *operator, *observed, claimed);
                let real = publish::effective_min_build(*operator, *observed, claimed);
                let maximum = numeric(*operator).max(numeric(*observed));
                let action = if maximum <= i64::try_from(claimed).unwrap() {
                    assert!(real.is_ok(), "({operator:?}, {observed:?}, {claimed})");
                    "Resolve"
                } else {
                    assert!(real.is_err(), "({operator:?}, {observed:?}, {claimed})");
                    if numeric(*operator) > i64::try_from(claimed).unwrap() {
                        "RejectOperatorAboveClaim"
                    } else {
                        "RejectObservedAboveClaim"
                    }
                };
                assert!(
                    model.action_enabled(action, &before),
                    "model disabled {action} for ({operator:?}, {observed:?}, {claimed})"
                );
                let after = model.successors(action, &before)[0].clone();

                if let Ok(real_floor) = real {
                    let real_floor = numeric(real_floor);
                    assert_eq!(after["phase"], 1);
                    assert_eq!(after["frozen_floor"], real_floor);
                    assert_eq!(after["journal_floor"], real_floor);
                    assert_eq!(real_floor, maximum);

                    // NEGATIVE CONTROL 1: the operator-only mutant must disagree
                    // whenever the live channel carries the larger floor.
                    if numeric(*operator) < numeric(*observed) {
                        let dropped = buggy.successors("ResolveOperatorOnly", &before)[0].clone();
                        assert_eq!(dropped["frozen_floor"], numeric(*operator));
                        assert_ne!(dropped["frozen_floor"], real_floor);
                        assert!(!model.successors("Resolve", &before).contains(&dropped));
                        dropped_observed_control = true;
                    }
                } else {
                    assert_eq!(after["phase"], 4);
                    // NEGATIVE CONTROL 1b: the claim check applied to the
                    // operator's request alone carries a channel floor above
                    // the build, which the real resolver just refused.
                    if action == "RejectObservedAboveClaim" {
                        let carried =
                            buggy.successors("ResolveUncheckedCarryForward", &before)[0].clone();
                        assert!(!buggy.check_invariant("FrozenFloorFitsClaim", &carried));
                        over_claim_control = true;
                    }
                }
            }
        }
    }
    assert!(
        dropped_observed_control,
        "bounded domain must distinguish carry-forward from operator-only policy"
    );
    assert!(
        over_claim_control,
        "bounded domain must reach a channel floor above the claimed build"
    );

    // Escalate one positive and one mutant transition through external `ty` too.
    let before = resolver_state(&model, Some(1), Some(2), 3);
    let accepted = model.successors("Resolve", &before)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &accepted,
        Some("Resolve"),
        "release floor: production max carry-forward",
    );
    assert!(admitted, "model rejected real resolver transition: {why}");
    let dropped = buggy.successors("ResolveOperatorOnly", &before)[0].clone();
    let (healthy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &dropped,
        Some("ResolveOperatorOnly"),
        "release floor: reject operator-only resolver mutant",
    );
    assert!(
        !healthy_admitted,
        "healthy model admitted floor drop: {why}"
    );
    let (buggy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &before,
        &dropped,
        Some("ResolveOperatorOnly"),
        "release floor: operator-only resolver negative control",
    );
    assert!(buggy_admitted, "Buggy=1 did not admit floor drop: {why}");
}

/// Bind the non-vacuous crash/resume transition to the real atomic Journal
/// save/load path. Runtime state is deliberately cleared before resume; the loaded
/// persisted floor is what reconstructs it, and the journal resumes at `publish`.
#[test]
fn journal_round_trip_restores_frozen_floor_for_resume() {
    let model = release_channel_floor_model();
    let before = resolver_state(&model, Some(1), Some(2), 3);
    let frozen = model.successors("Resolve", &before)[0].clone();
    let crashed = model.successors("CrashBeforeResume", &frozen)[0].clone();
    assert_eq!(crashed["phase"], 5);
    assert_eq!(crashed["frozen_floor"], 0);
    assert_eq!(crashed["journal_floor"], 2);

    let path = std::env::temp_dir().join(format!(
        "aterm-channel-floor-journal-{}.toml",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let journal = publish::Journal {
        verify_pubkey: None,
        format: publish::JOURNAL_FORMAT,
        version: "0.55.0".into(),
        build_number: 3,
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        min_build: Some(2),
        arm64_only: false,
        linux: None,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        signature_machine_id: None,
        release_id: Some(55),
        release_intent: true,
        upload_intents: Vec::new(),
        done: publish::STEPS
            .iter()
            .take_while(|step| **step != "publish")
            .map(|step| (*step).to_string())
            .collect(),
    };
    journal.save(&path).expect("persist frozen release journal");
    let loaded = publish::Journal::load(&path)
        .expect("load release journal")
        .expect("journal exists");
    assert_eq!(loaded, journal);
    assert_eq!(loaded.first_incomplete(), Some("publish"));

    let resumed = model.successors("ResumeFrozen", &crashed)[0].clone();
    assert_eq!(resumed["phase"], 1);
    assert_eq!(resumed["frozen_floor"], numeric(loaded.min_build));
    assert_eq!(resumed["frozen_floor"], resumed["journal_floor"]);
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &crashed,
        &resumed,
        Some("ResumeFrozen"),
        "release floor: atomic journal restores resume policy",
    );
    assert!(admitted, "model rejected real journal resume: {why}");
    // NEGATIVE CONTROL: the resumed floor rebuilt from the resume command's
    // request (1) instead of the journal's frozen floor (2).
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let from_request = buggy.successors("ResumeFromOperatorRequest", &crashed)[0].clone();
    assert_ne!(from_request["frozen_floor"], numeric(loaded.min_build));
    assert!(!buggy.check_invariant("RuntimeMatchesFrozenJournal", &from_request));

    let impossible = publish::Journal {
        min_build: Some(4),
        ..journal
    };
    assert!(
        impossible.save(&path).is_err(),
        "journal must reject a frozen floor above its claimed build"
    );
    let _ = std::fs::remove_file(path);
}

/// Bind the real late race guard, at its required lease-held call site, to
/// ConfirmCovered/RejectAdvanced and prove that a publish-from-frozen mutant is
/// rejected by the healthy lifecycle.
#[test]
fn channel_floor_covered_conforms_and_skipped_guard_is_rejected() {
    let model = release_channel_floor_model();
    let floors: Vec<Option<u64>> = std::iter::once(None).chain((0..=4).map(Some)).collect();

    for carried in &floors {
        for newest in &floors {
            let before = frozen_state(&model, *carried, *newest);
            let real = publish::channel_floor_covered(*carried, *newest);
            let action = if numeric(*newest) <= numeric(*carried) {
                assert!(real.is_ok(), "({carried:?}, {newest:?})");
                "ConfirmCovered"
            } else {
                assert!(real.is_err(), "({carried:?}, {newest:?})");
                "RejectAdvanced"
            };
            assert!(model.action_enabled(action, &before));
            let after = model.successors(action, &before)[0].clone();
            assert_eq!(after["late_checked"], 1);
            assert_eq!(after["phase"], if real.is_ok() { 2 } else { 4 });
            assert_eq!(
                after["lease_owned"], 1,
                "a pure late-guard decision must not release the remote lease"
            );
        }
    }

    // The concrete race: our journal carries 1, another publisher raises the
    // channel to 2, and the real guard refuses visibility.
    let raced = frozen_state(&model, Some(1), Some(2));
    assert!(publish::channel_floor_covered(Some(1), Some(2)).is_err());
    let rejected = model.successors("RejectAdvanced", &raced)[0].clone();
    assert_eq!(rejected["lease_owned"], 1);
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &raced,
        &rejected,
        Some("RejectAdvanced"),
        "release floor: production late-ratchet rejection",
    );
    assert!(admitted, "model rejected real late guard: {why}");

    // NEGATIVE CONTROL 2: a skipped guard jumps directly from Frozen to
    // Published. Healthy rejects the step; Buggy=1 admits it, and both publish
    // invariants expose the lowered live floor and absent revalidation.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let published = buggy.successors("PublishUnchecked", &raced)[0].clone();
    assert_eq!(published["phase"], 3);
    assert!(!buggy.check_invariant("PublishedNeverLowersLatest", &published));
    assert!(!buggy.check_invariant("PublishedRequiresLateGuard", &published));
    let (healthy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &raced,
        &published,
        Some("PublishUnchecked"),
        "release floor: reject skipped late guard",
    );
    assert!(
        !healthy_admitted,
        "healthy model admitted skipped guard: {why}"
    );
    let (buggy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &raced,
        &published,
        Some("PublishUnchecked"),
        "release floor: skipped late guard negative control",
    );
    assert!(buggy_admitted, "Buggy=1 did not admit skipped guard: {why}");
}

/// Tier-1 binds exact-owner resume, owner+floor checked visibility, lease retention
/// through the post-publish suffix, and the final exact-CAS unlock to the corrected
/// `ReleaseChannelFloor` lifecycle.
#[test]
fn release_lease_seams_conform_through_final_unlock() {
    let model = release_channel_floor_model();
    let owner = "a".repeat(40);
    let foreign = "b".repeat(40);

    assert_eq!(
        publish::acquire_lease_action(None, &owner).unwrap(),
        publish::LeaseAcquireAction::Create
    );
    assert_eq!(
        publish::acquire_lease_action(Some(&owner), &owner).unwrap(),
        publish::LeaseAcquireAction::AlreadyOwned
    );
    assert!(publish::acquire_lease_action(Some(&foreign), &owner).is_err());

    let mut before_acquire = frozen_state(&model, Some(2), Some(2));
    before_acquire.insert("lease_owned", 0);
    let acquired = model.successors("AcquireLease", &before_acquire)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before_acquire,
        &acquired,
        Some("AcquireLease"),
        "release lease exact-owner acquire/resume",
    );
    assert!(admitted, "model rejected exact-owner acquire: {why}");

    // Existing exact ownership is the crash/resume path: acquire reads the same
    // owner twice and produces a guard without attempting to replace the ref.
    let resume_git = LeaseScript::new(vec![
        command_ok(),
        lease_owner_row(&owner),
        lease_owner_row(&owner),
    ]);
    let guard = publish::acquire_release_lease(&resume_git, &owner)
        .expect("same journal commit resumes exact owner");
    assert_eq!(guard.owner(), owner);

    let frozen = frozen_state(&model, Some(2), Some(2));
    assert!(
        publish::publish_checked(&guard, Some(&owner), Some(2), Some(2)).is_ok(),
        "the real owner+floor guard must admit the covered cut"
    );
    assert!(
        publish::publish_checked(&guard, Some(&foreign), Some(2), Some(2)).is_err(),
        "a foreign owner must fail before visibility"
    );
    // NEGATIVE CONTROL: the floor check alone, without the owner check the real
    // guard pairs it with, confirms a covered cut nobody holds the lease for.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert!(
        model
            .successors("ConfirmCovered", &before_acquire)
            .is_empty()
    );
    let unowned = buggy.successors("ConfirmCoveredWithoutLease", &before_acquire)[0].clone();
    assert!(!buggy.check_invariant("RevalidatedOwnsLease", &unowned));
    assert!(
        publish::publish_checked(&guard, Some(&owner), Some(2), Some(3)).is_err(),
        "an advanced floor must fail under the exact owner"
    );

    let confirmed = model.successors("ConfirmCovered", &frozen)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &frozen,
        &confirmed,
        Some("ConfirmCovered"),
        "release PublishChecked owner+floor guard",
    );
    assert!(admitted, "model rejected real publish guard: {why}");
    let published = model.successors("PublishChecked", &confirmed)[0].clone();
    assert_eq!(published["phase"], 3);
    assert_eq!(
        published["lease_owned"], 1,
        "the head PATCH must retain the remote owner"
    );
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &confirmed,
        &published,
        Some("PublishChecked"),
        "release visibility retains lease",
    );
    assert!(admitted, "model rejected lease-retaining visibility: {why}");

    let verified = model.successors("ProveHead", &published)[0].clone();
    assert_eq!(verified["lease_owned"], 1);

    // Exact-CAS deletion observes our owner, succeeds, then confirms absence.
    let unlock_git = LeaseScript::new(vec![lease_owner_row(&owner), command_ok(), command_ok()]);
    assert_eq!(
        publish::release_completed_release_lease(&unlock_git, &owner).unwrap(),
        publish::LeaseRelease::Released
    );
    let unlocked = model.successors("Unlock", &verified)[0].clone();
    assert_eq!(unlocked["phase"], 7);
    assert_eq!(unlocked["lease_owned"], 0);
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &verified,
        &unlocked,
        Some("Unlock"),
        "release final exact-CAS unlock",
    );
    assert!(admitted, "model rejected real final unlock: {why}");
    assert!(model.check_invariant("CompletionRequiresProvedHead", &unlocked));

    // NEGATIVE CONTROL: the model's early-unlock mutant is unreachable in the
    // healthy lifecycle and violates both completion and bypass invariants.
    let early = buggy.successors("UnlockBeforeHeadProof", &published)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &published,
        &early,
        Some("UnlockBeforeHeadProof"),
        "release early-unlock negative control",
    );
    assert!(!admitted, "healthy model admitted early unlock: {why}");
    assert!(!buggy.check_invariant("CompletionRequiresProvedHead", &early));
    assert!(!buggy.check_invariant("UnlockCannotBeBypassed", &early));
}

/// Tier-1 binds the model's symbolic-target distinction to the real immutable
/// release snapshot and the exact origin-tag resolver `ship verify` and a yank's
/// successor proof bind the published head through. The old
/// target-equals-manifest mutant is retained as an explicit negative control.
#[test]
fn published_identity_snapshot_and_tag_resolution_refine_model() {
    let model = release_published_identity_model();
    let manifest_commit = "a".repeat(40);
    let historical = publish::ReleaseObjectIdentity {
        id: 349_821_802,
        tag: "v0.25".into(),
        draft: false,
        target_commitish: "main".into(),
    };
    publish::validate_release_object_snapshot(Some(&historical), &historical).unwrap();
    assert!(
        publish::validate_release_object_capability(
            Some(&historical),
            historical.id,
            &historical.tag,
            &manifest_commit,
            false,
        )
        .is_err(),
        "negative control: symbolic history is not a current claim-SHA capability"
    );
    let tag_object = "1".repeat(40);
    let git = LeaseScript::new(vec![RunOut {
        status: 0,
        stdout: format!("{tag_object}\trefs/tags/v0.25\n{manifest_commit}\trefs/tags/v0.25^{{}}\n")
            .into_bytes(),
        stderr: Vec::new(),
    }]);
    publish::assert_remote_annotated_tag_commit(&git, "v0.25", &manifest_commit).unwrap();
    let accepted = tier1_step(
        &model,
        &model.init_state(),
        "AcceptSymbolicHistory",
        "published identity: exact snapshot + tag peel accepts symbolic target",
    );

    let mut target_drift = historical.clone();
    target_drift.target_commitish = "Main".into();
    assert!(publish::validate_release_object_snapshot(Some(&target_drift), &historical).is_err());
    let drifted = tier1_step(
        &model,
        &accepted,
        "DriftCapturedTarget",
        "published identity: symbolic target drift is observable",
    );
    assert!(!model.action_enabled("DeleteWithExactPublishedIdentity", &drifted));

    let wrong = "b".repeat(40);
    let wrong_git = LeaseScript::new(vec![RunOut {
        status: 0,
        stdout: format!("{wrong}\trefs/tags/v0.25\n").into_bytes(),
        stderr: Vec::new(),
    }]);
    assert!(
        publish::assert_remote_annotated_tag_commit(&wrong_git, "v0.25", &manifest_commit).is_err()
    );
    let tag_drifted = tier1_step(
        &model,
        &accepted,
        "DriftResolvedTag",
        "published identity: wrong tag resolution is observable",
    );
    assert!(!model.action_enabled("DeleteWithExactPublishedIdentity", &tag_drifted));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let rejected =
        buggy.successors("RejectValidSymbolicHistoryAsNonSha", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("ValidSymbolicHistoryIsNotRejected", &rejected));
    let unbound =
        buggy.successors("AcceptUnboundSymbolicWithoutTag", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("UnboundSymbolicHistoryFailsClosed", &unbound));
}

fn yank_published(tag: &str, build: u64, min_build: Option<u64>) -> verify::Published {
    let version = tag.trim_start_matches('v');
    let commit = "a".repeat(40);
    verify::Published {
        release_id: Some(build),
        release: None,
        tag: tag.into(),
        build,
        version: version.into(),
        asset: manifest_out::MANIFEST_ASSET.into(),
        min_build,
        text: format!(
            "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\ncommit = \"{commit}\"\n\
             dmg = \"aterm-{version}.dmg\"\nsha256 = \"{}\"\n{}",
            "0".repeat(64),
            min_build.map_or_else(String::new, |floor| format!("min_build = {floor}\n"))
        ),
    }
}

/// Tier-1 binds the real successor ordering/build/floor decision to the yank
/// model. Signature, remote DMG digest, and tag-peel checks are replayed by the
/// caller before this eligibility result can authorize either cleanup mutation.
#[test]
fn yank_successor_decision_refines_successor_first_model() {
    let model = release_yank_successor_first_model();
    let bad = yank_published("v0.54.0", 54, None);
    let successor = yank_published("v0.55.0", 55, Some(55));
    assert!(verify::yank_successor_covers(&bad, &successor).unwrap());
    let before = model.init_state();
    let published = tier1_step(
        &model,
        &before,
        "PublishVerifiedSuccessor",
        "yank: real numeric successor/build/floor decision",
    );
    assert!(!model.action_enabled("DeleteExactTagAfterSuccessor", &published));
    let leased = tier1_step(
        &model,
        &published,
        "AcquireCleanupLease",
        "yank: acquire persistent cleanup lease",
    );
    let fenced = tier1_step(
        &model,
        &leased,
        "AcquireCleanupFence",
        "yank: acquire unique cleanup publisher fence",
    );
    assert!(!model.action_enabled("DeleteExactTagAfterSuccessor", &fenced));
    let reproved = tier1_step(
        &model,
        &fenced,
        "ReproveVerifiedSuccessor",
        "yank: reprove successor after acquiring cleanup session",
    );
    assert!(model.action_enabled("DeleteExactTagAfterSuccessor", &reproved));

    let weak_floor = yank_published("v0.55.0", 55, Some(54));
    assert!(!verify::yank_successor_covers(&bad, &weak_floor).unwrap());
    let stale_order = yank_published("v0.53.0", 56, Some(55));
    assert!(!verify::yank_successor_covers(&bad, &stale_order).unwrap());
    let stale_build = yank_published("v0.55.0", 54, Some(55));
    assert!(verify::yank_successor_covers(&bad, &stale_build).is_err());

    // A retired two-component release is inert archive history no client
    // selects: it is refused as target AND as successor, so a non-orderable
    // identity can never license a deletion.
    let retired = yank_published("v0.61", 61, Some(61));
    for (bad_end, successor_end) in [(&bad, &retired), (&retired, &successor)] {
        let error = verify::yank_successor_covers(bad_end, successor_end)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("retired two-component release"),
            "retired release was ordered instead of refused: {error}"
        );
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let weak = buggy.successors("DeleteTagWithWeakFloor", &before)[0].clone();
    let (healthy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &weak,
        Some("DeleteTagWithWeakFloor"),
        "yank: reject weak-floor cleanup",
    );
    assert!(!healthy_admitted, "healthy yank admitted weak floor: {why}");
    let (buggy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &before,
        &weak,
        Some("DeleteTagWithWeakFloor"),
        "yank: weak-floor negative control",
    );
    assert!(buggy_admitted, "mutant did not admit weak floor: {why}");

    let overflow = yank_published("v0.54.0", u64::MAX, None);
    assert!(verify::yank_successor_covers(&overflow, &successor).is_err());
    let mut mismatched = successor;
    mismatched.version = "0.56.0".into();
    assert!(verify::yank_successor_covers(&bad, &mismatched).is_err());
}
