// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publisher fence liveness and reclaim integration tests.

use crate::{ledger, publish};

use aterm_uds::spawnfd::Command;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ledger::testing::Git;
use publish::{
    FenceIdentity, FenceLiveness, FenceReclaim, GroupState, LocalProbe, PUBLISHER_FENCE_REF,
    PidState, PublisherProbe,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct BareFixture {
    _root: aterm_tempfile::TempDir,
    repo_a: PathBuf,
    repo_b: PathBuf,
    owner: String,
    other_owner: String,
}

fn command(dir: &Path, args: &[&str]) -> String {
    let output = ledger::testing::git()
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

fn config_identity(repo: &Path) {
    ledger::testing::config(
        repo,
        &[
            ("user.name", "Fence Liveness Test"),
            ("user.email", "fence-liveness@example.invalid"),
        ],
    );
}

fn bare_fixture(name: &str) -> BareFixture {
    static BUILT: crate::template::Template<(String, String)> = crate::template::Template::new();
    let parent = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("publisher-fence-liveness");
    std::fs::create_dir_all(&parent).expect("create fixture root");
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("{name}-"))
        .tempdir_in(parent)
        .expect("create fixture root");
    let root = dir.path();
    let (owner, other_owner) = BUILT.lay(root, build_bare);
    BareFixture {
        repo_a: root.join("publisher-a"),
        repo_b: root.join("publisher-b"),
        owner,
        other_owner,
        _root: dir,
    }
}

fn build_bare(root: &Path) -> (String, String) {
    let remote = root.join("origin.git");
    let seed = root.join("seed");
    assert!(
        ledger::testing::git()
            .args(["init", "--bare"])
            .arg(&remote)
            .status()
            .expect("git init bare")
            .success()
    );
    ledger::testing::config(&remote, &[]);
    assert!(
        ledger::testing::git()
            .args(["init", "-b", "main"])
            .arg(&seed)
            .status()
            .expect("git init seed")
            .success()
    );
    config_identity(&seed);
    std::fs::write(seed.join("claim"), "first\n").expect("write claim");
    command(&seed, &["add", "claim"]);
    command(&seed, &["commit", "-m", "first claim"]);
    let owner = command(&seed, &["rev-parse", "HEAD"]);
    std::fs::write(seed.join("claim"), "second\n").expect("write claim");
    command(&seed, &["commit", "-am", "second claim"]);
    let other_owner = command(&seed, &["rev-parse", "HEAD"]);
    command(
        &seed,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("utf8 remote path"),
        ],
    );
    command(&seed, &["push", "origin", "main"]);

    let clone = |name: &str| {
        let path = root.join(name);
        assert!(
            ledger::testing::git()
                .args(["clone", "--branch", "main"])
                .arg(&remote)
                .arg(&path)
                .status()
                .expect("git clone")
                .success()
        );
        config_identity(&path);
        path
    };
    clone("publisher-a");
    clone("publisher-b");
    (owner, other_owner)
}

fn forge_fence(repo: &Path, owner: &str, body: &str) -> String {
    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let local = format!("forged-fence-{}-{sequence}", std::process::id());
    command(repo, &["tag", "-a", &local, "-m", body, owner]);
    let token = command(repo, &["rev-parse", &format!("refs/tags/{local}")]);
    command(
        repo,
        &[
            "push",
            "--force",
            "origin",
            &format!("{token}:{PUBLISHER_FENCE_REF}"),
        ],
    );
    command(repo, &["tag", "-d", &local]);
    token
}

fn body_for(
    owner: &str,
    pid: u32,
    pgid: u32,
    host: &str,
    boot: &str,
    exe: &str,
    version: &str,
) -> String {
    format!(
        "aterm publisher fence for claim {owner}; pid {pid}; nonce 1; sequence 0\n\n\
         fence-format: 2\n\
         fence-pid: {pid}\n\
         fence-pgid: {pgid}\n\
         fence-host: {host}\n\
         fence-boot: {boot}\n\
         fence-exe: {exe}\n\
         fence-version: {version}\n\
         fence-started: 1788396800\n"
    )
}

fn this_invocation_publishes() {
    publish::set_publisher_fence_version("0.87.0");
}

fn local_host() -> String {
    LocalProbe.host().expect("this machine has a hostname")
}

fn local_boot() -> String {
    LocalProbe
        .boot()
        .expect("this machine publishes a boot session")
}

fn local_exe() -> String {
    LocalProbe.exe().expect("this test has an executable path")
}

fn own_pgid() -> u32 {
    LocalProbe
        .pgid()
        .expect("this machine reports process groups")
}

fn reaped_pid() -> (u32, u32) {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .process_group(0)
        .spawn()
        .expect("spawn a short-lived child in its own process group");
    let pid = child.id();
    child.wait().expect("reap the child");
    assert_eq!(
        LocalProbe.pid_state(pid),
        PidState::NotRunning,
        "pid {pid} was reused between reaping it and probing it; rerun"
    );
    (pid, pid)
}

struct FakeProbe {
    host: Option<String>,
    boot: Option<String>,
    pids: HashMap<u32, PidState>,
    /// Process groups this fake can answer for; anything else is `Unknown`, as from a real
    /// probe that could not read the table, and `Unknown` never licenses a reclaim.
    groups: HashMap<u32, GroupState>,
    /// The group this (probing) process is in.
    own_pgid: Option<u32>,
}

const FENCE_GROUP: u32 = 4200;
const PROBE_GROUP: u32 = 9000;

impl FakeProbe {
    fn here() -> Self {
        FakeProbe {
            host: Some("publisher-host".to_string()),
            boot: Some("boot-session-7".to_string()),
            pids: HashMap::new(),
            groups: HashMap::new(),
            own_pgid: Some(PROBE_GROUP),
        }
    }

    fn with_pid(mut self, pid: u32, state: PidState) -> Self {
        self.pids.insert(pid, state);
        self
    }

    fn with_group(mut self, pgid: u32, members: &[(u32, &str)]) -> Self {
        let state = if members.is_empty() {
            GroupState::Empty
        } else {
            GroupState::Members(
                members
                    .iter()
                    .map(|(pid, command)| (*pid, (*command).to_string()))
                    .collect(),
            )
        };
        self.groups.insert(pgid, state);
        self
    }
}

impl PublisherProbe for FakeProbe {
    fn host(&self) -> Option<String> {
        self.host.clone()
    }

    fn boot(&self) -> Option<String> {
        self.boot.clone()
    }

    fn exe(&self) -> Option<String> {
        Some("/opt/aterm/aterm-release".to_string())
    }

    fn pid_state(&self, pid: u32) -> PidState {
        self.pids
            .get(&pid)
            .cloned()
            .unwrap_or(PidState::Unknown("not stubbed".to_string()))
    }

    fn pgid(&self) -> Option<u32> {
        self.own_pgid
    }

    fn group_members(&self, pgid: u32) -> GroupState {
        self.groups
            .get(&pgid)
            .cloned()
            .unwrap_or(GroupState::Unknown("not stubbed".to_string()))
    }
}

const FULL: &str = "fence-format: 2\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
                    fence-boot: boot-session-7\nfence-exe: /opt/aterm/aterm-release\n";

fn identity(extra: &str) -> FenceIdentity {
    FenceIdentity::parse(&format!(
        "object deadbeef\ntype commit\ntag fence\n\n\
         aterm publisher fence for claim deadbeef; pid 4242\n\n{extra}"
    ))
}

#[test]
fn a_running_pid_on_this_host_and_boot_is_alive() {
    let identity = identity(FULL);
    let probe = FakeProbe::here().with_pid(
        4242,
        PidState::Running {
            exe: Some("/opt/aterm/aterm-release".to_string()),
        },
    );
    let liveness = publish::fence_liveness(&identity, &probe);
    assert!(
        matches!(liveness, FenceLiveness::Alive(_)),
        "a running publisher must be ALIVE, got {liveness:?}"
    );
}

#[test]
fn an_absent_pid_with_an_empty_group_on_this_host_and_boot_is_dead() {
    let identity = identity(FULL);
    let probe = FakeProbe::here()
        .with_pid(4242, PidState::NotRunning)
        .with_group(FENCE_GROUP, &[]);
    let liveness = publish::fence_liveness(&identity, &probe);
    match &liveness {
        FenceLiveness::Dead(detail) => assert!(
            detail.contains("4242") && detail.contains("4200"),
            "the proof names both the pid and the empty group: {detail}"
        ),
        other => panic!("an absent pid AND an empty group is proof of death, got {other:?}"),
    }
}

#[test]
fn a_reused_pid_running_another_program_with_an_empty_group_is_dead() {
    let identity = identity(FULL);
    let probe = FakeProbe::here()
        .with_pid(
            4242,
            PidState::Running {
                exe: Some("/bin/zsh".to_string()),
            },
        )
        .with_group(FENCE_GROUP, &[]);
    assert!(
        matches!(
            publish::fence_liveness(&identity, &probe),
            FenceLiveness::Dead(_)
        ),
        "a pid now running a different program cannot be the publisher"
    );
}

#[test]
fn a_child_that_outlived_its_publisher_keeps_the_fence_alive() {
    let identity = identity(FULL);
    let orphaned = FakeProbe::here()
        .with_pid(4242, PidState::NotRunning)
        .with_group(FENCE_GROUP, &[(4243, "curl")]);
    match publish::fence_liveness(&identity, &orphaned) {
        FenceLiveness::Alive(detail) => assert!(
            detail.contains("4243") && detail.contains("curl"),
            "the verdict names the survivor, so the operator can stop it: {detail}"
        ),
        other => panic!("a live child in the publisher's group must read ALIVE, got {other:?}"),
    }
    let reused = FakeProbe::here()
        .with_pid(
            4242,
            PidState::Running {
                exe: Some("/bin/zsh".to_string()),
            },
        )
        .with_group(FENCE_GROUP, &[(4243, "git")]);
    assert!(matches!(
        publish::fence_liveness(&identity, &reused),
        FenceLiveness::Alive(_)
    ));
}

#[test]
fn a_truncated_executable_name_is_never_read_as_a_mismatch() {
    let identity = identity(
        "fence-format: 1\nfence-pid: 4242\nfence-host: publisher-host\n\
         fence-boot: boot-session-7\nfence-exe: /opt/aterm/aterm-release-cutter\n",
    );
    let probe = FakeProbe::here().with_pid(
        4242,
        PidState::Running {
            exe: Some("aterm-release-c".to_string()),
        },
    );
    assert!(
        matches!(
            publish::fence_liveness(&identity, &probe),
            FenceLiveness::Alive(_)
        ),
        "a truncated comm is not evidence of anything"
    );
}

#[test]
fn every_unprovable_shape_refuses_rather_than_guessing() {
    let probe = FakeProbe::here()
        .with_pid(4242, PidState::NotRunning)
        .with_group(FENCE_GROUP, &[]);
    let full = FULL;
    assert!(matches!(
        publish::fence_liveness(&identity(full), &probe),
        FenceLiveness::Dead(_)
    ));

    for (label, body) in [
        ("no liveness block at all (older binary)", ""),
        (
            "another host",
            "fence-format: 2\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: some-other-mac\n\
             fence-boot: boot-session-7\n",
        ),
        (
            "another boot session",
            "fence-format: 2\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: boot-session-6\n",
        ),
        (
            "no pid",
            "fence-format: 2\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: boot-session-7\n",
        ),
        (
            "no boot session",
            "fence-format: 2\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n",
        ),
        (
            "a newer format this binary cannot read",
            "fence-format: 99\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: boot-session-7\n",
        ),
        (
            "an unprobeable pid",
            "fence-format: 2\nfence-pid: 5151\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: boot-session-7\n",
        ),
        (
            "no process group recorded (a format-1 fence)",
            "fence-format: 1\nfence-pid: 4242\nfence-host: publisher-host\n\
             fence-boot: boot-session-7\nfence-exe: /opt/aterm/aterm-release\n",
        ),
        (
            "a process group the table could not be read for",
            "fence-format: 2\nfence-pid: 4242\nfence-pgid: 4300\nfence-host: publisher-host\n\
             fence-boot: boot-session-7\n",
        ),
        (
            "the process group this very process is in",
            "fence-format: 2\nfence-pid: 4242\nfence-pgid: 9000\nfence-host: publisher-host\n\
             fence-boot: boot-session-7\n",
        ),
    ] {
        let liveness = publish::fence_liveness(&identity(body), &probe);
        assert!(
            matches!(liveness, FenceLiveness::Unprovable(_)),
            "{label} must be UNPROVABLE, got {liveness:?}"
        );
    }

    for pid in [0u32, 1] {
        let body = format!(
            "fence-format: 2\nfence-pid: {pid}\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: boot-session-7\n"
        );
        assert!(
            matches!(
                publish::fence_liveness(&identity(&body), &LocalProbe),
                FenceLiveness::Unprovable(_)
            ),
            "pid {pid} must never be a liveness answer"
        );
    }

    for blinded in [
        FakeProbe {
            host: None,
            ..FakeProbe::here()
        },
        FakeProbe {
            boot: None,
            ..FakeProbe::here()
        },
    ] {
        let blinded = blinded.with_pid(4242, PidState::NotRunning);
        assert!(
            matches!(
                publish::fence_liveness(&identity(full), &blinded),
                FenceLiveness::Unprovable(_)
            ),
            "a machine that cannot identify itself cannot prove a pid is dead"
        );
    }
}

#[test]
fn a_fence_this_binary_writes_is_readable_from_another_clone() {
    let fixture = bare_fixture("round-trip");
    let git_a = Git::new(&fixture.repo_a);
    let git_b = Git::new(&fixture.repo_b);
    this_invocation_publishes();
    publish::acquire_release_lease(&git_a, &fixture.owner).unwrap();
    let guard = publish::acquire_publisher_fence(&git_a, &fixture.owner).unwrap();

    let identity = publish::publisher_fence_identity(&git_b, guard.token());
    assert_eq!(identity.unreadable, None, "{identity:?}");
    assert_eq!(identity.format, Some(publish::FENCE_IDENTITY_FORMAT));
    assert_eq!(identity.pid, Some(std::process::id()));
    assert_eq!(
        identity.pgid,
        Some(own_pgid()),
        "the fence records the group its curl children will share"
    );
    assert_eq!(identity.host.as_deref(), Some(local_host().as_str()));
    assert_eq!(identity.boot.as_deref(), Some(local_boot().as_str()));
    assert_eq!(identity.version.as_deref(), Some("0.87.0"));
    assert!(identity.started_unix.is_some(), "{identity:?}");

    assert!(matches!(
        publish::fence_liveness(&identity, &LocalProbe),
        FenceLiveness::Alive(_)
    ));
    publish::release_publisher_fence(&git_a, &guard).unwrap();
}

#[test]
fn a_live_publisher_still_refuses_and_is_never_reclaimed() {
    let fixture = bare_fixture("live-refuses");
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let token = forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &body_for(
            &fixture.owner,
            std::process::id(),
            own_pgid(),
            &local_host(),
            &local_boot(),
            &local_exe(),
            "0.87.0",
        ),
    );

    publish::preflight_release_lease(&git).unwrap_err();

    match publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap() {
        FenceReclaim::Kept { .. } => {}
        other => panic!("a live publisher's fence must be kept, got {other:?}"),
    }
    assert_eq!(
        publish::publisher_fence(&git).unwrap().unwrap().token,
        token,
        "the live fence must still be exactly where it was"
    );
}

#[test]
fn a_resume_reclaims_a_fence_whose_publisher_is_provably_dead() {
    let fixture = bare_fixture("dead-reclaimed");
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let (dead, dead_group) = reaped_pid();
    let stale = forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &body_for(
            &fixture.owner,
            dead,
            dead_group,
            &local_host(),
            &local_boot(),
            &local_exe(),
            "0.87.0",
        ),
    );

    match publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap() {
        FenceReclaim::Reclaimed { token, detail } => {
            assert_eq!(token, stale);
            assert!(detail.contains(&format!("pid {dead}")), "{detail}");
        }
        other => panic!("a dead publisher's fence must be reclaimed, got {other:?}"),
    }
    assert_eq!(
        publish::publisher_fence(&git).unwrap(),
        None,
        "the reclaimed token must be gone"
    );

    let mine = publish::acquire_publisher_fence(&git, &fixture.owner).unwrap();
    assert_ne!(mine.token(), stale);
    match publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap() {
        FenceReclaim::Kept { .. } => {}
        other => panic!("the live winner's fence must be kept, got {other:?}"),
    }
    publish::release_publisher_fence(&git, &mine).unwrap();
}

#[test]
fn a_reused_pid_is_proof_of_death_end_to_end() {
    let fixture = bare_fixture("reused-pid");
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let (_, empty_group) = reaped_pid();
    forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &body_for(
            &fixture.owner,
            std::process::id(),
            empty_group,
            &local_host(),
            &local_boot(),
            "/opt/aterm/definitely-not-this-test-binary",
            "0.87.0",
        ),
    );
    assert!(matches!(
        publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap(),
        FenceReclaim::Reclaimed { .. }
    ));
    assert_eq!(publish::publisher_fence(&git).unwrap(), None);
}

#[test]
fn a_spawned_request_that_outlives_the_publisher_keeps_the_fence_end_to_end() {
    let fixture = bare_fixture("orphaned-child");
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let mut survivor = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .expect("spawn the survivor in its own group");
    let group = survivor.id();
    let mut publisher = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .process_group(group as i32)
        .spawn()
        .expect("spawn the publisher into the survivor's group");
    let publisher_pid = publisher.id();
    publisher.wait().expect("reap the publisher");
    forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &body_for(
            &fixture.owner,
            publisher_pid,
            group,
            &local_host(),
            &local_boot(),
            &local_exe(),
            "0.87.0",
        ),
    );

    match publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap() {
        FenceReclaim::Kept { .. } => {}
        other => panic!("a fence whose group still has a live member must be kept, got {other:?}"),
    }

    survivor.kill().expect("stop the survivor");
    survivor.wait().expect("reap the survivor");
    assert!(matches!(
        publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap(),
        FenceReclaim::Reclaimed { .. }
    ));
    assert_eq!(publish::publisher_fence(&git).unwrap(), None);
}

#[test]
fn a_fence_from_another_host_refuses_and_is_kept() {
    let fixture = bare_fixture("other-host");
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let token = forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &body_for(
            &fixture.owner,
            std::process::id(),
            own_pgid(),
            "some-other-publisher.local",
            &local_boot(),
            &local_exe(),
            "0.87.0",
        ),
    );
    publish::preflight_release_lease(&git).unwrap_err();
    assert!(matches!(
        publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap(),
        FenceReclaim::Kept { .. }
    ));
    assert_eq!(
        publish::publisher_fence(&git).unwrap().unwrap().token,
        token
    );
}

#[test]
fn a_fence_from_another_boot_session_refuses_and_is_kept() {
    let fixture = bare_fixture("other-boot");
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let (dead, dead_group) = reaped_pid();
    let token = forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &body_for(
            &fixture.owner,
            dead,
            dead_group,
            &local_host(),
            "kern.boottime:1",
            &local_exe(),
            "0.87.0",
        ),
    );
    publish::preflight_release_lease(&git).unwrap_err();
    assert!(matches!(
        publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap(),
        FenceReclaim::Kept { .. }
    ));
    assert_eq!(
        publish::publisher_fence(&git).unwrap().unwrap().token,
        token
    );
}

#[test]
fn a_fence_written_by_an_older_binary_refuses_instead_of_crashing() {
    let fixture = bare_fixture("legacy-fence");
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let token = forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &format!(
            "aterm publisher fence for claim {}; pid 4242; nonce 17; sequence 0",
            fixture.owner
        ),
    );
    publish::preflight_release_lease(&git).unwrap_err();
    assert!(matches!(
        publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap(),
        FenceReclaim::Kept { .. }
    ));
    assert_eq!(
        publish::publisher_fence(&git).unwrap().unwrap().token,
        token,
        "an old fence is never auto-stolen"
    );
}

#[test]
fn a_fence_for_another_claim_is_never_reclaimed() {
    let fixture = bare_fixture("foreign-claim");
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let (dead, dead_group) = reaped_pid();
    let token = forge_fence(
        &fixture.repo_a,
        &fixture.other_owner,
        &body_for(
            &fixture.other_owner,
            dead,
            dead_group,
            &local_host(),
            &local_boot(),
            &local_exe(),
            "0.86.0",
        ),
    );
    match publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap() {
        FenceReclaim::Kept { .. } => {}
        other => panic!("another claim's fence must be kept, got {other:?}"),
    }
    assert_eq!(
        publish::publisher_fence(&git).unwrap().unwrap().token,
        token
    );
}

#[test]
fn an_unreadable_fence_body_refuses_rather_than_crashing() {
    let fixture = bare_fixture("unreadable-body");
    let git = Git::new(&fixture.repo_a);
    let identity = publish::publisher_fence_identity(&git, &"0".repeat(40));
    assert!(identity.unreadable.is_some(), "{identity:?}");
    assert!(matches!(
        publish::fence_liveness(&identity, &LocalProbe),
        FenceLiveness::Unprovable(_)
    ));
    let malformed = publish::publisher_fence_identity(&git, "not-a-sha");
    assert!(
        malformed
            .unreadable
            .as_deref()
            .is_some_and(|reason| reason.contains("malformed")),
        "{malformed:?}"
    );
}
