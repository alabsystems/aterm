// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-machine release-lease proofs. The race fixture drives real Git
//! protocol/ref semantics against a local bare remote; the injected fixture
//! covers the one transport ambiguity real local Git cannot reproduce.

#[path = "../src/apple.rs"]
#[allow(dead_code)]
mod apple;
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
#[path = "../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/mirror.rs"]
#[allow(dead_code)]
mod mirror;
// Mounted for publish.rs, whose roster reconstruction publishes the pair through the
// provisioning module's writer lock and redo transaction.
#[path = "../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../src/verify.rs"]
#[allow(dead_code)]
mod verify;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use ledger::{Error, GitCli, GitRunner, Result, RunOut};
use publish::{FenceRelease, LeaseRelease, PUBLISHER_FENCE_REF, RELEASE_LEASE_REF};

fn command(dir: &Path, args: &[&str]) -> String {
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

static BARE_FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct BareFixture {
    root: PathBuf,
    repo_a: PathBuf,
    repo_b: PathBuf,
    owner_a: String,
    owner_b: String,
}

impl Drop for BareFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn bare_fixture(name: &str) -> BareFixture {
    let sequence = BARE_FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("release-lease")
        .join(format!("{name}-{}-{sequence}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create lease fixture");
    let remote = root.join("origin.git");
    let seed = root.join("seed");
    assert!(
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote)
            .status()
            .expect("git init bare")
            .success()
    );
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .arg(&seed)
            .status()
            .expect("git init seed")
            .success()
    );
    command(&seed, &["config", "user.name", "Release Lease Test"]);
    command(
        &seed,
        &["config", "user.email", "release-lease@example.invalid"],
    );
    std::fs::write(seed.join("claim"), "first\n").expect("write first claim");
    command(&seed, &["add", "claim"]);
    command(&seed, &["commit", "-m", "first claim"]);
    let first = command(&seed, &["rev-parse", "HEAD"]);
    std::fs::write(seed.join("claim"), "second\n").expect("write second claim");
    command(&seed, &["commit", "-am", "second claim"]);
    let second = command(&seed, &["rev-parse", "HEAD"]);
    command(
        &seed,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    command(&seed, &["push", "origin", "main"]);

    let clone = |name: &str| {
        let path = root.join(name);
        let status = Command::new("git")
            .args(["clone", "--branch", "main"])
            .arg(&remote)
            .arg(&path)
            .status()
            .expect("git clone");
        assert!(status.success());
        command(&path, &["config", "user.name", "Release Fence Test"]);
        command(
            &path,
            &["config", "user.email", "release-fence@example.invalid"],
        );
        path
    };
    BareFixture {
        repo_a: clone("publisher-a"),
        repo_b: clone("publisher-b"),
        owner_a: first,
        owner_b: second,
        root,
    }
}

/// Pause same-claim sessions after both observed the same fence token (or
/// absence), forcing the create/rotation CAS itself to decide the winner.
struct FenceProbeBarrier {
    inner: GitCli,
    barrier: Arc<Barrier>,
    first_probe: AtomicBool,
}

impl FenceProbeBarrier {
    fn new(repo: PathBuf, barrier: Arc<Barrier>) -> Self {
        Self {
            inner: GitCli::new(repo),
            barrier,
            first_probe: AtomicBool::new(true),
        }
    }
}

impl GitRunner for FenceProbeBarrier {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        let output = self.inner.git(args)?;
        if args.len() == 4
            && args[0] == "ls-remote"
            && args[1] == "origin"
            && args[2] == PUBLISHER_FENCE_REF
            && self.first_probe.swap(false, Ordering::SeqCst)
        {
            self.barrier.wait();
        }
        Ok(output)
    }
}

/// Pause both publishers only after each captured the same absent-ref answer.
/// Their subsequent pushes therefore exercise the remote's real atomic
/// create behavior rather than a conveniently serialized test schedule.
struct FirstProbeBarrier {
    inner: GitCli,
    barrier: Arc<Barrier>,
    first_probe: AtomicBool,
}

impl FirstProbeBarrier {
    fn new(repo: PathBuf, barrier: Arc<Barrier>) -> Self {
        Self {
            inner: GitCli::new(repo),
            barrier,
            first_probe: AtomicBool::new(true),
        }
    }
}

impl GitRunner for FirstProbeBarrier {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        let output = self.inner.git(args)?;
        if args == ["ls-remote", "origin", RELEASE_LEASE_REF]
            && self.first_probe.swap(false, Ordering::SeqCst)
        {
            self.barrier.wait();
        }
        Ok(output)
    }
}

#[test]
fn real_bare_remote_allows_one_claim_and_only_owner_recovery() {
    let fixture = bare_fixture("atomic-race");
    let repo_a = fixture.repo_a.clone();
    let repo_b = fixture.repo_b.clone();
    let owner_a = fixture.owner_a.clone();
    let owner_b = fixture.owner_b.clone();
    let barrier = Arc::new(Barrier::new(2));
    let runner_a = FirstProbeBarrier::new(repo_a.clone(), Arc::clone(&barrier));
    let runner_b = FirstProbeBarrier::new(repo_b.clone(), Arc::clone(&barrier));

    let (result_a, result_b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| publish::acquire_release_lease(&runner_a, &owner_a));
        let b = scope.spawn(|| publish::acquire_release_lease(&runner_b, &owner_b));
        (
            a.join().expect("publisher A thread"),
            b.join().expect("publisher B thread"),
        )
    });
    assert_ne!(
        result_a.is_ok(),
        result_b.is_ok(),
        "exactly one create from the same absent observation must win"
    );
    let (winner_repo, loser_repo, winner, loser, guard) = if let Ok(guard) = result_a {
        (repo_a, repo_b, owner_a, owner_b, guard)
    } else {
        (repo_b, repo_a, owner_b, owner_a, result_b.unwrap())
    };
    let winner_git = GitCli::new(&winner_repo);
    let loser_git = GitCli::new(&loser_repo);
    assert_eq!(
        publish::release_lease_owner(&winner_git).unwrap(),
        Some(winner.clone())
    );

    let preflight = publish::preflight_release_lease(&loser_git)
        .unwrap_err()
        .to_string();
    assert!(preflight.contains(&winner), "{preflight}");
    assert!(publish::publish_checked(&guard, Some(&winner), Some(7), Some(7)).is_ok());
    assert!(publish::publish_checked(&guard, Some(&winner), Some(7), Some(8)).is_err());
    assert!(publish::publish_checked(&guard, Some(&loser), Some(7), Some(7)).is_err());

    // Crash-before-unlock: the same journal owner resumes; a different claim
    // cannot acquire or release it and cannot mutate the owner ref.
    assert_eq!(
        publish::acquire_release_lease(&winner_git, &winner)
            .unwrap()
            .owner(),
        winner
    );
    let refusal = publish::acquire_release_lease(&loser_git, &loser)
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("refusing to steal"), "{refusal}");
    assert!(publish::release_release_lease(&loser_git, &loser).is_err());
    assert_eq!(
        publish::release_lease_owner(&winner_git).unwrap(),
        Some(winner.clone())
    );

    assert_eq!(
        publish::release_release_lease(&winner_git, &winner).unwrap(),
        LeaseRelease::Released
    );
    assert_eq!(
        publish::release_release_lease(&winner_git, &winner).unwrap(),
        LeaseRelease::AlreadyAbsent,
        "crash after delete and before journal mark converges without reacquiring"
    );

    // A successor may acquire after that delete but before the old unlock is
    // marked. Unlock-only replay recognizes create-only succession and leaves
    // the new owner byte-for-byte untouched.
    publish::acquire_release_lease(&loser_git, &loser).unwrap();
    assert_eq!(
        publish::release_completed_release_lease(&winner_git, &winner).unwrap(),
        LeaseRelease::AlreadySuperseded
    );
    assert_eq!(
        publish::release_lease_owner(&winner_git).unwrap(),
        Some(loser.clone())
    );
    publish::release_release_lease(&loser_git, &loser).unwrap();
}

#[test]
fn same_claim_publishers_get_one_unique_fence_and_exact_token_cleanup() {
    let fixture = bare_fixture("same-owner-fence");
    let repo_a = fixture.repo_a.clone();
    let repo_b = fixture.repo_b.clone();
    let owner = fixture.owner_a.clone();
    let git_a = GitCli::new(&repo_a);
    publish::acquire_release_lease(&git_a, &owner).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let runner_a = FenceProbeBarrier::new(repo_a.clone(), Arc::clone(&barrier));
    let runner_b = FenceProbeBarrier::new(repo_b.clone(), Arc::clone(&barrier));
    let (result_a, result_b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| publish::acquire_publisher_fence(&runner_a, &owner));
        let b = scope.spawn(|| publish::acquire_publisher_fence(&runner_b, &owner));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(result_a.is_ok(), result_b.is_ok(), "one create-only winner");
    let guard = result_a.or(result_b).unwrap();
    let observed = publish::publisher_fence(&git_a).unwrap().unwrap();
    assert_eq!(observed.owner, owner);
    assert_eq!(observed.token, guard.token());
    assert_ne!(
        observed.token, observed.owner,
        "fence must be unique, not owner=claim"
    );
    let lease = publish::acquire_lease_action(Some(&owner), &owner).unwrap();
    assert_eq!(lease, publish::LeaseAcquireAction::AlreadyOwned);

    assert_eq!(
        publish::release_publisher_fence(&git_a, &guard).unwrap(),
        FenceRelease::Released
    );
    assert_eq!(
        publish::release_publisher_fence(&git_a, &guard).unwrap(),
        FenceRelease::AlreadyAbsent
    );
    publish::release_release_lease(&git_a, &owner).unwrap();
}

#[test]
fn killed_session_recovery_rotates_exact_old_token_with_one_winner() {
    let fixture = bare_fixture("recovery-rotation");
    let repo_a = fixture.repo_a.clone();
    let repo_b = fixture.repo_b.clone();
    let owner = fixture.owner_a.clone();
    let git_a = GitCli::new(&repo_a);
    publish::acquire_release_lease(&git_a, &owner).unwrap();
    let stale = publish::acquire_publisher_fence(&git_a, &owner).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let runner_a = FenceProbeBarrier::new(repo_a.clone(), Arc::clone(&barrier));
    let runner_b = FenceProbeBarrier::new(repo_b.clone(), Arc::clone(&barrier));
    let (result_a, result_b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| publish::rotate_publisher_fence_for_recovery(&runner_a, &owner));
        let b = scope.spawn(|| publish::rotate_publisher_fence_for_recovery(&runner_b, &owner));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(
        result_a.is_ok(),
        result_b.is_ok(),
        "one exact-CAS rotation winner"
    );
    let fresh = result_a.or(result_b).unwrap();
    assert_ne!(
        fresh.token(),
        stale.token(),
        "rotation must change the token"
    );
    let observed = publish::publisher_fence(&git_a).unwrap().unwrap();
    assert_eq!(observed.token, fresh.token());
    assert_eq!(
        publish::release_publisher_fence(&git_a, &stale).unwrap(),
        FenceRelease::AlreadySuperseded,
        "stale cleanup must never delete the rotated winner"
    );
    assert_eq!(
        publish::publisher_fence(&git_a).unwrap().unwrap().token,
        fresh.token()
    );
    publish::release_publisher_fence(&git_a, &fresh).unwrap();
    publish::release_release_lease(&git_a, &owner).unwrap();
}

#[test]
fn final_unlock_atomically_removes_owner_and_fence_without_orphan_state() {
    let fixture = bare_fixture("atomic-final-unlock");
    let repo = fixture.repo_a.clone();
    let owner = fixture.owner_a.clone();
    let git = GitCli::new(&repo);
    publish::acquire_release_lease(&git, &owner).unwrap();
    let fence = publish::acquire_publisher_fence(&git, &owner).unwrap();
    assert_eq!(
        publish::release_completed_publisher_session(&git, &owner, &fence).unwrap(),
        LeaseRelease::Released
    );
    assert_eq!(publish::release_lease_owner(&git).unwrap(), None);
    assert_eq!(publish::publisher_fence(&git).unwrap(), None);
    assert_eq!(
        publish::release_completed_release_lease(&git, &owner).unwrap(),
        LeaseRelease::AlreadyAbsent,
        "crash after atomic push and before journal mark converges"
    );
}

#[test]
fn unlock_only_replay_leaves_a_coherent_foreign_successor_pair_untouched() {
    let fixture = bare_fixture("unlock-successor-pair");
    let repo = fixture.repo_a.clone();
    let old_owner = fixture.owner_a.clone();
    let successor_owner = fixture.owner_b.clone();
    let git = GitCli::new(&repo);
    publish::acquire_release_lease(&git, &old_owner).unwrap();
    let old_fence = publish::acquire_publisher_fence(&git, &old_owner).unwrap();
    publish::release_completed_publisher_session(&git, &old_owner, &old_fence).unwrap();

    publish::acquire_release_lease(&git, &successor_owner).unwrap();
    let successor_fence = publish::acquire_publisher_fence(&git, &successor_owner).unwrap();
    assert_eq!(
        publish::release_completed_session_without_guard(&git, &old_owner).unwrap(),
        LeaseRelease::AlreadySuperseded
    );
    assert_eq!(
        publish::release_lease_owner(&git).unwrap(),
        Some(successor_owner.clone())
    );
    assert_eq!(
        publish::publisher_fence(&git).unwrap().unwrap().token,
        successor_fence.token()
    );

    assert!(
        publish::release_completed_session_without_guard(&git, &successor_owner).is_err(),
        "the same-owner live token is not proof of a completed atomic delete"
    );
    publish::release_completed_publisher_session(&git, &successor_owner, &successor_fence).unwrap();
}

#[test]
fn final_unlock_refuses_incoherent_foreign_owner_and_old_fence() {
    let fixture = bare_fixture("incoherent-final-unlock");
    let repo = fixture.repo_a.clone();
    let owner = fixture.owner_a.clone();
    let foreign = fixture.owner_b.clone();
    let git = GitCli::new(&repo);
    publish::acquire_release_lease(&git, &owner).unwrap();
    let fence = publish::acquire_publisher_fence(&git, &owner).unwrap();
    command(
        &repo,
        &[
            "push",
            "--force",
            "origin",
            &format!("{foreign}:{RELEASE_LEASE_REF}"),
        ],
    );
    let error = publish::release_completed_publisher_session(&git, &owner, &fence)
        .unwrap_err()
        .to_string();
    assert!(error.contains("inconsistent final unlock"), "{error}");
    assert_eq!(publish::release_lease_owner(&git).unwrap(), Some(foreign));
    assert_eq!(
        publish::publisher_fence(&git).unwrap().unwrap().token,
        fence.token()
    );

    // Fixture cleanup deliberately bypasses production APIs after proving the
    // incoherent state was left untouched.
    command(&repo, &["push", "origin", &format!(":{RELEASE_LEASE_REF}")]);
    command(
        &repo,
        &["push", "origin", &format!(":{PUBLISHER_FENCE_REF}")],
    );
}

#[test]
fn recovery_requires_a_clean_tree_with_no_cask_era_exception() {
    // Format 6 admitted one dirty state: the exact cask pin written/staged by a
    // crash mid-`step_cask`. Format 7 removed that step, so recovery now admits
    // NOTHING — a clean tree passes and any dirty path at all fails closed.
    let fixture = bare_fixture("clean-tree-recovery");
    let repo = fixture.repo_a.clone();
    std::fs::write(repo.join(".gitignore"), "dist/\n").unwrap();
    command(&repo, &["add", ".gitignore"]);
    command(&repo, &["commit", "-m", "add ignore fixture"]);

    let owner = command(&repo, &["rev-parse", "HEAD"]);
    let journal = publish::Journal {
        verify_pubkey: None,
        format: publish::JOURNAL_FORMAT,
        version: "0.55.0".into(),
        build_number: 55,
        commit: owner,
        min_build: None,
        arm64_only: false,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        signature_machine_id: None,
        release_id: Some(55),
        draft_create_issued: true,
        upload_intents: Vec::new(),
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        done: publish::STEPS
            .iter()
            .take_while(|step| **step != "verify")
            .map(|step| (*step).to_string())
            .collect(),
    };

    // A clean tree is the only accepted state.
    publish::recovery_resume_worktree_preflight(&repo, &GitCli::new(&repo), &journal).unwrap();

    // An untracked path fails closed.
    std::fs::write(repo.join("claim"), "unrelated mutation\n").unwrap();
    assert!(
        publish::recovery_resume_worktree_preflight(&repo, &GitCli::new(&repo), &journal).is_err()
    );
    std::fs::remove_file(repo.join("claim")).unwrap();

    // So does a modification to a tracked path — there is no longer any
    // journal-derived write that recovery will explain away.
    std::fs::write(repo.join(".gitignore"), "dist/\nextra/\n").unwrap();
    assert!(
        publish::recovery_resume_worktree_preflight(&repo, &GitCli::new(&repo), &journal).is_err()
    );
}

struct ScriptedGit {
    replies: Mutex<VecDeque<RunOut>>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl ScriptedGit {
    fn new(replies: Vec<RunOut>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl GitRunner for ScriptedGit {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(args.iter().map(|arg| (*arg).to_string()).collect());
        self.replies
            .lock()
            .expect("replies lock")
            .pop_front()
            .ok_or_else(|| Error::new("unexpected scripted git call"))
    }
}

fn ls_remote(owner: &str) -> RunOut {
    RunOut {
        status: 0,
        stdout: format!("{owner}\t{RELEASE_LEASE_REF}\n").into_bytes(),
        stderr: vec![],
    }
}

#[test]
fn failed_unlock_transport_with_foreign_reread_is_safe_successor_convergence() {
    let owner = "a".repeat(40);
    let successor = "b".repeat(40);
    let git = ScriptedGit::new(vec![
        ls_remote(&owner),
        RunOut {
            status: 1,
            stdout: vec![],
            stderr: b"injected timeout after server accepted delete".to_vec(),
        },
        ls_remote(&successor),
    ]);

    assert_eq!(
        publish::release_completed_release_lease(&git, &owner).unwrap(),
        LeaseRelease::AlreadySuperseded,
        "create-only foreign ownership proves the old ref was absent even when push status lies"
    );
    let calls = git.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[1],
        [
            "push".to_string(),
            format!("--force-with-lease={RELEASE_LEASE_REF}:{owner}"),
            "origin".to_string(),
            format!(":{RELEASE_LEASE_REF}"),
        ],
        "unlock must be exact-owner CAS deletion, never an unconditional force/delete"
    );
}

#[test]
fn foreign_owner_is_refused_outside_unlock_only_convergence() {
    let expected = "a".repeat(40);
    let foreign = "b".repeat(40);
    let git = ScriptedGit::new(vec![ls_remote(&foreign)]);
    let error = publish::release_release_lease(&git, &expected)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("refusing to delete another cut's lease"),
        "{error}"
    );
    assert_eq!(
        git.calls.lock().unwrap().len(),
        1,
        "no delete was attempted"
    );
}
