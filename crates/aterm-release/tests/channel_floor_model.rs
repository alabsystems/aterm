// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the release-channel floor lifecycle. The production
//! resolver and late guard remain in `publish.rs`; this test projects their real
//! decisions onto the single-source derived model and keeps explicit mutants.

// The release crate is intentionally binary-only. As in `resume.rs`, mount the
// pipeline modules so the test drives the exact production functions.
#[path = "../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/mirror.rs"]
#[allow(dead_code)]
mod mirror;
#[path = "../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../src/verify.rs"]
#[allow(dead_code)]
mod verify;

use aterm_spec::derive::{
    Model, release_channel_floor_model, release_channel_single_head_model,
    release_published_identity_model, release_yank_successor_first_model,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use ledger::{GitRunner, RunOut};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use std::collections::{BTreeSet, VecDeque};
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

#[derive(Debug)]
struct IdentityArchiveRemote {
    releases: Vec<publish::AppcastRelease>,
    renames: Vec<publish::AppcastRename>,
    replace_object_mutant: bool,
    next_replacement_id: u64,
}

impl IdentityArchiveRemote {
    fn new(releases: Vec<publish::AppcastRelease>) -> Self {
        Self {
            releases,
            renames: Vec::new(),
            replace_object_mutant: false,
            next_replacement_id: 10_000,
        }
    }

    fn ids(&self) -> BTreeSet<u64> {
        self.releases
            .iter()
            .flat_map(|release| release.assets.iter().map(|asset| asset.id))
            .collect()
    }
}

impl publish::AppcastArchiveRemote for IdentityArchiveRemote {
    fn list_releases(&mut self) -> ledger::Result<Vec<publish::AppcastRelease>> {
        Ok(self.releases.clone())
    }

    fn rename_asset(&mut self, rename: &publish::AppcastRename) -> ledger::Result<()> {
        self.renames.push(rename.clone());
        let release = self
            .releases
            .iter_mut()
            .find(|release| !release.draft && release.tag == rename.tag)
            .ok_or_else(|| ledger::Error::new("archive fixture release missing"))?;
        let index = release
            .assets
            .iter()
            .position(|asset| asset.id == rename.id)
            .ok_or_else(|| ledger::Error::new("archive fixture asset missing"))?;
        if release.assets[index].name != rename.from {
            return Err(ledger::Error::new("archive fixture source name drifted"));
        }
        if self.replace_object_mutant {
            release.assets.remove(index);
            release.assets.push(publish::AppcastAsset {
                id: self.next_replacement_id,
                name: rename.to.clone(),
            });
            self.next_replacement_id += 1;
        } else {
            release.assets[index].name.clone_from(&rename.to);
        }
        Ok(())
    }
}

fn archive_release(tag: &str, manifest_id: u64, signature_id: u64) -> publish::AppcastRelease {
    publish::AppcastRelease {
        release_id: manifest_id,
        tag: tag.to_string(),
        draft: false,
        target_commitish: "a".repeat(40),
        assets: vec![
            publish::AppcastAsset {
                id: manifest_id,
                name: manifest_out::MANIFEST_ASSET.to_string(),
            },
            publish::AppcastAsset {
                id: signature_id,
                name: manifest_out::MANIFEST_SIG_ASSET.to_string(),
            },
        ],
    }
}

fn release_manifest(version: &str, build: u64, commit: &str, dmg: &str) -> Vec<u8> {
    format!(
        "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\ncommit = \"{commit}\"\n\
         dmg = \"{dmg}\"\nsha256 = \"{}\"\n",
        "0".repeat(64)
    )
    .into_bytes()
}

fn assert_live_identity_refusal(model: &Model, label: &str, rejected: bool) {
    assert!(rejected, "real identity validator accepted {label}");
    let mut observed = model.init_state();
    assert!(model.fire("Flip", &mut observed));
    assert!(model.fire("ObserveLiveIdentityMismatch", &mut observed));
    let refused = model.successors("AbortLiveIdentityMismatch", &observed)[0].clone();
    assert_eq!(refused["owner"], 1);
    assert_eq!(refused["guard_attached"], 1);
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        model,
        &[],
        &observed,
        &refused,
        Some("AbortLiveIdentityMismatch"),
        label,
    );
    assert!(admitted, "model rejected real identity refusal: {why}");
    let exited = model.successors("ExitAfterRefusal", &refused)[0].clone();
    assert_eq!(exited["owner"], 1);
    assert_eq!(exited["guard_attached"], 0);
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
                }
            }
        }
    }
    assert!(
        dropped_observed_control,
        "bounded domain must distinguish carry-forward from operator-only policy"
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
/// persisted floor is what reconstructs it, and the journal resumes at `archive`.
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
        format: publish::JOURNAL_FORMAT,
        version: "0.55.0".into(),
        build_number: 3,
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        min_build: Some(2),
        arm64_only: false,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        release_id: Some(55),
        draft_create_issued: true,
        upload_intents: Vec::new(),
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        done: publish::STEPS
            .iter()
            .take_while(|step| **step != "archive")
            .map(|step| (*step).to_string())
            .collect(),
    };
    journal.save(&path).expect("persist frozen release journal");
    let loaded = publish::Journal::load(&path)
        .expect("load release journal")
        .expect("journal exists");
    assert_eq!(loaded, journal);
    assert_eq!(loaded.first_incomplete(), Some("archive"));

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
        "flip must retain the remote owner"
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

    let archived = model.successors("ArchiveAfterPublish", &published)[0].clone();
    let pinned = model.successors("PinCask", &archived)[0].clone();
    let verified = model.successors("VerifyRelease", &pinned)[0].clone();
    assert_eq!(verified["lease_owned"], 1);

    // Exact-CAS deletion observes our owner, succeeds, then confirms absence.
    let unlock_git = LeaseScript::new(vec![lease_owner_row(&owner), command_ok(), command_ok()]);
    assert_eq!(
        publish::release_release_lease(&unlock_git, &owner).unwrap(),
        publish::LeaseRelease::Released
    );
    let unlocked = model.successors("Unlock", &verified)[0].clone();
    assert_eq!(unlocked["phase"], 9);
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
    assert!(model.check_invariant("CompletionRequiresPostPublishSteps", &unlocked));

    // NEGATIVE CONTROL: the model's early-unlock mutant is unreachable in the
    // healthy lifecycle and violates both completion and bypass invariants.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let early = buggy.successors("UnlockBeforeVerification", &published)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &published,
        &early,
        Some("UnlockBeforeVerification"),
        "release early-unlock negative control",
    );
    assert!(!admitted, "healthy model admitted early unlock: {why}");
    assert!(!buggy.check_invariant("CompletionRequiresPostPublishSteps", &early));
    assert!(!buggy.check_invariant("UnlockCannotBeBypassed", &early));
}

/// Tier-1 binds the complete live authority seam: version/build/commit/DMG,
/// byte equality, signature policy/key, signature bytes, and cryptographic
/// validity. Only exact identity enters archive; every concrete mismatch maps to
/// the same refusal abstraction while retaining the persistent lease.
#[test]
fn live_release_identity_conforms_to_single_head_archive_guard() {
    let model = release_channel_single_head_model();
    let commit = "a".repeat(40);
    let expected = publish::ExpectedReleaseIdentity {
        version: "0.2.0",
        build: 2,
        commit: &commit,
    };
    let manifest = release_manifest("0.2.0", 2, &commit, "aterm-0.2.0.dmg");

    let real = publish::validate_live_release_identity(
        expected,
        &manifest,
        None,
        Some(&manifest),
        None,
        false,
        None,
    );
    assert!(real.is_ok());
    let mut before = model.init_state();
    assert!(model.fire("Flip", &mut before));
    let after = model.successors("BeginArchive", &before)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &after,
        Some("BeginArchive"),
        "archive exact unsigned live identity",
    );
    assert!(admitted, "model rejected exact live identity: {why}");

    for (label, bad) in [
        (
            "archive rejects wrong version",
            release_manifest("0.3.0", 2, &commit, "aterm-0.2.0.dmg"),
        ),
        (
            "archive rejects wrong build",
            release_manifest("0.2.0", 3, &commit, "aterm-0.2.0.dmg"),
        ),
        (
            "archive rejects wrong commit",
            release_manifest("0.2.0", 2, &"b".repeat(40), "aterm-0.2.0.dmg"),
        ),
        (
            "archive rejects wrong DMG",
            release_manifest("0.2.0", 2, &commit, "other.dmg"),
        ),
    ] {
        assert_live_identity_refusal(
            &model,
            label,
            publish::validate_live_release_identity(expected, &bad, None, None, None, false, None)
                .is_err(),
        );
    }
    assert_live_identity_refusal(
        &model,
        "archive rejects local/live manifest byte drift",
        publish::validate_live_release_identity(
            expected,
            &manifest,
            None,
            Some(b"different manifest bytes"),
            None,
            false,
            None,
        )
        .is_err(),
    );
    assert_live_identity_refusal(
        &model,
        "archive rejects malformed manifest bytes",
        publish::validate_live_release_identity(
            expected,
            b"not a manifest",
            None,
            None,
            None,
            false,
            None,
        )
        .is_err(),
    );
    assert_live_identity_refusal(
        &model,
        "unsigned journal rejects unexpected live signature",
        publish::validate_live_release_identity(
            expected,
            &manifest,
            Some(&[0_u8; 64]),
            Some(&manifest),
            None,
            false,
            None,
        )
        .is_err(),
    );

    let keypair = Ed25519KeyPair::from_seed_unchecked(&[42_u8; 32]).unwrap();
    let pubkey = B64.encode(keypair.public_key().as_ref());
    let signature = keypair.sign(&manifest).as_ref().to_vec();
    let bad_signature = [0_u8; 64];
    assert!(
        publish::validate_live_release_identity(
            expected,
            &manifest,
            Some(&signature),
            Some(&manifest),
            Some(&signature),
            true,
            Some(&pubkey),
        )
        .is_ok()
    );
    let mut signed_before = model.init_state();
    assert!(model.fire("ConfigureSignatures", &mut signed_before));
    assert!(model.fire("Flip", &mut signed_before));
    let signed_after = model.successors("BeginArchive", &signed_before)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &signed_before,
        &signed_after,
        Some("BeginArchive"),
        "archive exact signed live identity",
    );
    assert!(admitted, "model rejected exact signed identity: {why}");

    for (label, live_signature, local_signature, key) in [
        (
            "signed archive rejects missing live signature",
            None,
            Some(signature.as_slice()),
            Some(pubkey.as_str()),
        ),
        (
            "signed archive rejects missing key identity",
            Some(signature.as_slice()),
            Some(signature.as_slice()),
            None,
        ),
        (
            "signed archive rejects local/live signature drift",
            Some(signature.as_slice()),
            Some(bad_signature.as_slice()),
            Some(pubkey.as_str()),
        ),
        (
            "signed archive rejects invalid signature",
            Some(bad_signature.as_slice()),
            Some(bad_signature.as_slice()),
            Some(pubkey.as_str()),
        ),
    ] {
        assert_live_identity_refusal(
            &model,
            label,
            publish::validate_live_release_identity(
                expected,
                &manifest,
                live_signature,
                Some(&manifest),
                local_signature,
                true,
                key,
            )
            .is_err(),
        );
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut invalid = model.init_state();
    assert!(model.fire("Flip", &mut invalid));
    assert!(model.fire("ObserveLiveIdentityMismatch", &mut invalid));
    let bypassed = buggy.successors("BeginArchiveInvalidLiveIdentity", &invalid)[0].clone();
    assert!(!model.action_enabled("BeginArchiveInvalidLiveIdentity", &invalid));
    assert!(!buggy.check_invariant("ArchiveUsesValidatedLiveIdentity", &bypassed));
    assert!(!buggy.check_invariant("LiveIdentityGuardCannotBeBypassed", &bypassed));
}

/// Tier-1 binds the model's symbolic-target distinction to the real immutable
/// release snapshot and exact historical tag resolver. The old
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
    publish::assert_remote_historical_tag_commits(&git, &[("v0.25", manifest_commit.as_str())])
        .unwrap();
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
        publish::assert_remote_historical_tag_commits(
            &wrong_git,
            &[("v0.25", manifest_commit.as_str())]
        )
        .is_err()
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

/// Tier-1 binds the real archive executor—not only its scalar plan—to the
/// model's identity-preserving rename transitions. The negative remote keeps
/// the same names/counts but swaps object IDs, which production must reject.
#[test]
fn archive_executor_refines_identity_preserving_single_head_transitions() {
    let model = release_channel_single_head_model();
    let releases = vec![
        archive_release("v0.2.0", 1, 2),
        archive_release("v0.1.0", 11, 12),
        archive_release("v0.0.0", 21, 22),
    ];
    let mut remote = IdentityArchiveRemote::new(releases.clone());
    let ids_before = remote.ids();
    assert_eq!(
        publish::converge_appcast_archive(&mut remote, "v0.2.0").unwrap(),
        4
    );
    assert_eq!(remote.ids(), ids_before, "metadata PATCH must preserve IDs");

    let mut state = model.init_state();
    assert!(model.fire("ConfigureSignatures", &mut state));
    assert!(model.fire("Flip", &mut state));
    assert!(model.fire("BeginArchive", &mut state));
    for (index, rename) in remote.renames.iter().enumerate() {
        let action = if rename.from == manifest_out::MANIFEST_ASSET {
            "RenameHistoricalManifest"
        } else {
            assert_eq!(rename.from, manifest_out::MANIFEST_SIG_ASSET);
            "RenameHistoricalSignature"
        };
        let before = state.clone();
        let after = model.successors(action, &before)[0].clone();
        let label = format!("archive same-ID PATCH {} for {}", rename.id, rename.tag);
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &after,
            Some(action),
            &label,
        );
        assert!(admitted, "real archive PATCH {index} rejected: {why}");
        state = after;
    }
    assert!(model.fire("FinalizeArchived", &mut state));
    assert!(model.check_invariant("HistoricalManifestIdentityPreserved", &state));
    assert!(model.check_invariant("HistoricalSignatureIdentityPreserved", &state));
    assert!(model.check_invariant("StableHasSingleExactHead", &state));

    let mut replacement = IdentityArchiveRemote::new(releases);
    replacement.replace_object_mutant = true;
    let error = publish::converge_appcast_archive(&mut replacement, "v0.2.0")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("vanished instead of being metadata-renamed"),
        "delete+recreate mutant escaped identity proof: {error}"
    );
}
