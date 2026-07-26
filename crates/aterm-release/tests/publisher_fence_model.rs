// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding from real bare-Git publisher fencing to
//! `ReleasePublisherFence`. Recovery assumes the old publisher has been proved
//! stopped; production must reject any residual stale guard's later mutation and
//! exact-token cleanup authority.

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
    Model, release_publisher_fence_model, release_yank_successor_first_model,
};
use ledger::{GitCli, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct BareFixture {
    repo: PathBuf,
    owner: String,
    root: PathBuf,
}

impl Drop for BareFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn bare_fixture(name: &str) -> BareFixture {
    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("publisher-fence-model")
        .join(format!("{name}-{}-{sequence}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let remote = root.join("origin.git");
    let seed = root.join("seed");
    assert!(
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .arg(&seed)
            .status()
            .unwrap()
            .success()
    );
    git(&seed, &["config", "user.name", "Publisher Fence Model"]);
    git(
        &seed,
        &[
            "config",
            "user.email",
            "publisher-fence-model@example.invalid",
        ],
    );
    std::fs::write(seed.join("claim"), "release claim\n").unwrap();
    git(&seed, &["add", "claim"]);
    git(&seed, &["commit", "-m", "release claim"]);
    let owner = git(&seed, &["rev-parse", "HEAD"]);
    git(
        &seed,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("utf8 fixture path"),
        ],
    );
    git(&seed, &["push", "origin", "main"]);
    let repo = root.join("publisher");
    assert!(
        Command::new("git")
            .args(["clone", "--branch", "main"])
            .arg(&remote)
            .arg(&repo)
            .status()
            .unwrap()
            .success()
    );
    git(&repo, &["config", "user.name", "Publisher Fence Model"]);
    git(
        &repo,
        &[
            "config",
            "user.email",
            "publisher-fence-model@example.invalid",
        ],
    );
    BareFixture { repo, owner, root }
}

fn transition(
    model: &Model,
    before: &aterm_spec::interp::State,
    action: &str,
    label: &str,
) -> aterm_spec::interp::State {
    let after = model.successors(action, before)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        model,
        &[],
        before,
        &after,
        Some(action),
        label,
    );
    assert!(admitted, "model rejected production transition: {why}");
    after
}

fn assert_session(
    git: &GitCli,
    lease: &publish::ReleaseLeaseGuard,
    guard: &publish::PublisherFenceGuard,
) -> Result<()> {
    publish::assert_publisher_session(git, lease, guard)
}

#[test]
fn stopped_process_recovery_rejects_residual_guard_and_exact_cleanup() {
    let fixture = bare_fixture("rotation");
    let git = GitCli::new(&fixture.repo);
    let lease = publish::acquire_release_lease(&git, &fixture.owner).unwrap();

    let model = release_publisher_fence_model();
    let mut state = model.init_state();
    let stale = publish::acquire_publisher_fence(&git, &fixture.owner).unwrap();
    let observed = publish::publisher_fence(&git).unwrap().unwrap();
    assert_eq!(observed.owner, fixture.owner);
    assert_eq!(observed.token, stale.token());
    state = transition(
        &model,
        &state,
        "AcquireA",
        "publisher fence: create-only unique token",
    );

    assert_session(&git, &lease, &stale).unwrap();
    state = transition(
        &model,
        &state,
        "MutateA",
        "publisher fence: exact owner+token admits mutation",
    );

    // The operator has established the documented stopped-publisher precondition.
    // Retaining a copy of A's guard lets this test prove that residual token data
    // cannot mutate or delete B after B's exact-CAS rotation. It does not claim
    // that rotation cancels an external request which was already in flight.
    state = transition(
        &model,
        &state,
        "StopA",
        "publisher fence: old publisher stopped before recovery",
    );
    let fresh = publish::rotate_publisher_fence_for_recovery(&git, &fixture.owner).unwrap();
    assert_ne!(fresh.token(), stale.token());
    state = transition(
        &model,
        &state,
        "RotateAtoB",
        "publisher fence: recovery exact-CAS rotation",
    );
    assert!(assert_session(&git, &lease, &stale).is_err());
    assert!(!model.action_enabled("MutateA", &state));
    assert_eq!(
        publish::release_publisher_fence(&git, &stale).unwrap(),
        publish::FenceRelease::AlreadySuperseded
    );
    state = transition(
        &model,
        &state,
        "ObserveStaleARelease",
        "publisher fence: stale cleanup preserves winner",
    );
    assert_eq!(
        publish::publisher_fence(&git).unwrap().unwrap().token,
        fresh.token()
    );

    assert_session(&git, &lease, &fresh).unwrap();
    state = transition(
        &model,
        &state,
        "MutateB",
        "publisher fence: rotated winner admits mutation",
    );
    assert_eq!(
        publish::release_publisher_fence(&git, &fresh).unwrap(),
        publish::FenceRelease::Released
    );
    state = transition(
        &model,
        &state,
        "ReleaseB",
        "publisher fence: ordinary exact-token cleanup retains lease",
    );
    assert_eq!(state["remote_token"], 0);
    assert_eq!(state["lease_owner"], 1);
    assert_eq!(
        publish::release_lease_owner(&git).unwrap(),
        Some(fixture.owner.clone())
    );
    publish::release_release_lease(&git, &fixture.owner).unwrap();
}

#[test]
fn real_final_unlock_atomically_deletes_owner_and_fence() {
    let fixture = bare_fixture("atomic-final");
    let git = GitCli::new(&fixture.repo);
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let fence = publish::acquire_publisher_fence(&git, &fixture.owner).unwrap();

    let model = release_publisher_fence_model();
    let before = model.init_state();
    let acquired = transition(
        &model,
        &before,
        "AcquireA",
        "publisher fence: final-session acquire",
    );
    assert_eq!(
        publish::release_completed_publisher_session(&git, &fixture.owner, &fence).unwrap(),
        publish::LeaseRelease::Released
    );
    let deleted = transition(
        &model,
        &acquired,
        "AtomicFinalDeleteA",
        "publisher fence: atomic owner+token final delete",
    );
    assert_eq!(deleted["remote_token"], 0);
    assert_eq!(deleted["remote_fence_owner"], 0);
    assert_eq!(deleted["lease_owner"], 0);
    assert_eq!(publish::publisher_fence(&git).unwrap(), None);
    assert_eq!(publish::release_lease_owner(&git).unwrap(), None);
}

#[test]
fn ordinary_reentry_cannot_reuse_an_old_stopped_process_proof() {
    let fixture = bare_fixture("fresh-stop-proof");
    let git = GitCli::new(&fixture.repo);
    let lease = publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let first = publish::acquire_publisher_fence(&git, &fixture.owner).unwrap();

    let model = release_publisher_fence_model();
    let mut state = transition(
        &model,
        &model.init_state(),
        "AcquireA",
        "publisher reentry: first concrete process acquires token",
    );
    state = transition(
        &model,
        &state,
        "StopA",
        "publisher reentry: first process is externally proved stopped",
    );
    assert_eq!(
        publish::release_publisher_fence(&git, &first).unwrap(),
        publish::FenceRelease::Released
    );
    state = transition(
        &model,
        &state,
        "ReleaseA",
        "publisher reentry: ordinary token release retains claim",
    );

    let second = publish::acquire_publisher_fence(&git, &fixture.owner).unwrap();
    assert_ne!(first.token(), second.token());
    state = transition(
        &model,
        &state,
        "AcquireA",
        "publisher reentry: same claim gets a fresh process token",
    );
    assert_eq!(state["old_process_stopped"], 0);
    assert!(!model.action_enabled("RotateAtoB", &state));
    assert_session(&git, &lease, &second).unwrap();

    state = transition(
        &model,
        &state,
        "StopA",
        "publisher reentry: new process obtains its own stop proof",
    );
    let recovered = publish::rotate_publisher_fence_for_recovery(&git, &fixture.owner).unwrap();
    state = transition(
        &model,
        &state,
        "RotateAtoB",
        "publisher reentry: recovery follows the fresh proof",
    );
    assert!(assert_session(&git, &lease, &second).is_err());
    assert_session(&git, &lease, &recovered).unwrap();
    assert_eq!(
        publish::release_completed_publisher_session(&git, &fixture.owner, &recovered).unwrap(),
        publish::LeaseRelease::Released
    );
    let state = transition(
        &model,
        &state,
        "AtomicFinalDeleteB",
        "publisher reentry: recovered session atomically unlocks",
    );
    assert_eq!(state["lease_owner"], 0);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut stale = buggy.init_state();
    for action in ["AcquireA", "StopA", "ReleaseA"] {
        assert!(buggy.fire(action, &mut stale), "{action}");
    }
    assert!(!model.action_enabled("AcquireAReusingStoppedProof", &stale));
    assert!(buggy.fire("AcquireAReusingStoppedProof", &mut stale));
    assert!(buggy.action_enabled("RotateAtoB", &stale));
    assert!(!buggy.check_invariant("StoppedProofIsPerProcess", &stale));
}

#[test]
fn yank_cleanup_refines_real_session_guards_and_atomic_unlock() {
    let fixture = bare_fixture("yank-cleanup");
    let git = GitCli::new(&fixture.repo);
    let model = release_yank_successor_first_model();
    let mut state = transition(
        &model,
        &model.init_state(),
        "PublishVerifiedSuccessor",
        "yank cleanup: verified successor precedes coordination",
    );

    let lease = publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    state = transition(
        &model,
        &state,
        "AcquireCleanupLease",
        "yank cleanup: real persistent owner ref acquired",
    );
    let fence = publish::acquire_publisher_fence(&git, &fixture.owner).unwrap();
    state = transition(
        &model,
        &state,
        "AcquireCleanupFence",
        "yank cleanup: real unique publisher token acquired",
    );
    state = transition(
        &model,
        &state,
        "ReproveVerifiedSuccessor",
        "yank cleanup: successor replayed after session acquisition",
    );
    assert_session(&git, &lease, &fence).unwrap();
    state = transition(
        &model,
        &state,
        "DeleteExactTagAfterSuccessor",
        "yank cleanup: real session guard admits exact-tag edge",
    );
    state = transition(
        &model,
        &state,
        "ReproveVerifiedSuccessor",
        "yank cleanup: successor replayed before release edge",
    );
    assert_session(&git, &lease, &fence).unwrap();
    state = transition(
        &model,
        &state,
        "DeleteReleaseAfterTag",
        "yank cleanup: real session guard admits release edge",
    );
    assert_eq!(state["cleanup_complete"], 1);
    assert_session(&git, &lease, &fence).unwrap();
    assert_eq!(
        publish::release_completed_publisher_session(&git, &fixture.owner, &fence).unwrap(),
        publish::LeaseRelease::Released
    );
    state = transition(
        &model,
        &state,
        "ReleaseCleanupSession",
        "yank cleanup: real atomic owner+token unlock after convergence",
    );
    assert_eq!(state["cleanup_session_released"], 1);
    assert_eq!(publish::release_lease_owner(&git).unwrap(), None);
    assert_eq!(publish::publisher_fence(&git).unwrap(), None);
}

#[test]
fn yank_cleanup_lease_loss_refuses_and_mutant_is_non_vacuous() {
    let fixture = bare_fixture("yank-lease-loss");
    let repo_git = GitCli::new(&fixture.repo);
    let lease = publish::acquire_release_lease(&repo_git, &fixture.owner).unwrap();
    let fence = publish::acquire_publisher_fence(&repo_git, &fixture.owner).unwrap();

    let model = release_yank_successor_first_model();
    let mut state = model.init_state();
    for action in [
        "PublishVerifiedSuccessor",
        "AcquireCleanupLease",
        "AcquireCleanupFence",
        "ReproveVerifiedSuccessor",
    ] {
        state = transition(&model, &state, action, "yank lease-loss setup");
    }
    assert_session(&repo_git, &lease, &fence).unwrap();

    let owner_lease = format!(
        "--force-with-lease={}:{}",
        publish::RELEASE_LEASE_REF,
        fixture.owner
    );
    let delete = format!(":{}", publish::RELEASE_LEASE_REF);
    git(&fixture.repo, &["push", &owner_lease, "origin", &delete]);
    assert!(assert_session(&repo_git, &lease, &fence).is_err());
    state = transition(
        &model,
        &state,
        "LoseCleanupLease",
        "yank cleanup: real owner loss invalidates process guard",
    );
    assert!(!model.action_enabled("DeleteExactTagAfterSuccessor", &state));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let escaped = buggy.successors("DeleteTagAfterCleanupLeaseLoss", &state)[0].clone();
    let (healthy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &state,
        &escaped,
        Some("DeleteTagAfterCleanupLeaseLoss"),
        "yank cleanup: healthy lease-loss refusal",
    );
    assert!(
        !healthy_admitted,
        "healthy model admitted stale cleanup: {why}"
    );
    let (buggy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &state,
        &escaped,
        Some("DeleteTagAfterCleanupLeaseLoss"),
        "yank cleanup: lease-loss negative control",
    );
    assert!(
        buggy_admitted,
        "mutant failed to expose stale cleanup: {why}"
    );
    assert!(!buggy.check_invariant("CleanupSessionCannotBeBypassed", &escaped));

    let mut early = buggy.init_state();
    for action in [
        "PublishVerifiedSuccessor",
        "AcquireCleanupLease",
        "AcquireCleanupFence",
    ] {
        assert!(buggy.fire(action, &mut early), "{action}");
    }
    assert!(!model.action_enabled("ReleaseCleanupSession", &early));
    assert!(buggy.fire("ReleaseCleanupSessionEarly", &mut early));
    assert!(!buggy.check_invariant("CleanupSessionReleasesOnlyAfterConvergence", &early));
}
