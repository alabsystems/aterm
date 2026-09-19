// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The publisher fence must be able to say WHO holds it and whether that
//! publisher is alive, dead, or unprovable — and a `--resume` may take the
//! fence back only under proof of death.
//!
//! Before this, every refusal was the same unactionable sentence and an
//! interrupted resume wedged the pipeline behind a hand-assembled
//! `ship recover vX.Y.Z <40-char-sha> --old-publisher-stopped`. These tests pin
//! both halves of the fix: the recorded identity survives a real push/fetch
//! between two clones, and the verdict is biased toward "alive" everywhere it
//! cannot prove otherwise.

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

use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use ledger::GitCli;
use publish::{
    FenceIdentity, FenceLiveness, FenceReclaim, GroupState, LocalProbe, PUBLISHER_FENCE_REF,
    PidState, PublisherProbe,
};

// ---------------------------------------------------------------------------
// fixture: a real bare remote and two clones, so a fence written by one machine
// is read by the other through a real push/fetch.
// ---------------------------------------------------------------------------

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct BareFixture {
    root: PathBuf,
    repo_a: PathBuf,
    repo_b: PathBuf,
    owner: String,
    other_owner: String,
}

impl Drop for BareFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

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

fn bare_fixture(name: &str) -> BareFixture {
    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("publisher-fence-liveness")
        .join(format!("{name}-{}-{sequence}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create fixture root");
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
    command(&seed, &["config", "user.name", "Fence Liveness Test"]);
    command(
        &seed,
        &["config", "user.email", "fence-liveness@example.invalid"],
    );
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
            Command::new("git")
                .args(["clone", "--branch", "main"])
                .arg(&remote)
                .arg(&path)
                .status()
                .expect("git clone")
                .success()
        );
        command(&path, &["config", "user.name", "Fence Liveness Test"]);
        command(
            &path,
            &["config", "user.email", "fence-liveness@example.invalid"],
        );
        path
    };
    BareFixture {
        repo_a: clone("publisher-a"),
        repo_b: clone("publisher-b"),
        owner,
        other_owner,
        root,
    }
}

/// Write a fence whose body we choose, exactly as a publisher would: an
/// annotated tag peeled to the claim, force-pushed to the fence ref.
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

/// The body a publisher on THIS machine, in THIS boot session, would write.
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

/// What this process is cutting. The recorded version is process-global, and
/// these tests run in parallel in one binary, so EVERY test sets the same value
/// — a test that needs a different version records it in the FENCE, where the
/// value is per-fence and cannot race.
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

/// This test process's own process group — what a fence written by a LIVE
/// publisher standing in for this process would record.
fn own_pgid() -> u32 {
    LocalProbe
        .pgid()
        .expect("this machine reports process groups")
}

/// A publisher that is provably gone: spawned IN A PROCESS GROUP OF ITS OWN and
/// reaped, so the pid is out of the table AND the group is empty — the group
/// being what proof of death is about, since a publisher's spawned `gh`/`curl`
/// children share it and outlive a killed parent. Returns `(pid, pgid)`; for a
/// group leader the two are equal. The assert is the guard against the one way
/// this could lie (immediate pid reuse); it is a precondition, not the proof.
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

// ---------------------------------------------------------------------------
// the decision itself, at every verdict, without arranging a real reboot
// ---------------------------------------------------------------------------

struct FakeProbe {
    host: Option<String>,
    boot: Option<String>,
    pids: HashMap<u32, PidState>,
    /// Process groups this fake can answer for. Anything else is `Unknown`,
    /// exactly as a real probe that could not read the table — and `Unknown`
    /// must never license a reclaim.
    groups: HashMap<u32, GroupState>,
    /// The group THIS (probing) process is in.
    own_pgid: Option<u32>,
}

/// The group the fake publisher's fence records, and the one this fake process
/// is in — distinct, as a real resume's are.
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

/// A format-2 block for the fake publisher: pid 4242 in group [`FENCE_GROUP`],
/// on this host, in this boot session.
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
    // The group is deliberately NOT stubbed: a running publisher is ALIVE on
    // its pid alone, and the group is never consulted.
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

/// THE REFUTED CASE. No GitHub mutation happens in the publisher process:
/// every one runs in a spawned `gh` or `curl`, and a SIGKILL of the parent
/// stops none of them. The parent's pid is gone; its upload is not.
///
/// KILLS: judging liveness on the recorded pid alone — which is exactly what
/// the first version of this decision did, and would have let a `--resume`
/// run beside a `curl --upload-file` still streaming the previous session's
/// asset.
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
    // A reused pid with a survivor in the group is the same answer.
    let reused = FakeProbe::here()
        .with_pid(
            4242,
            PidState::Running {
                exe: Some("/bin/zsh".to_string()),
            },
        )
        .with_group(FENCE_GROUP, &[(4243, "gh")]);
    assert!(matches!(
        publish::fence_liveness(&identity, &reused),
        FenceLiveness::Alive(_)
    ));
}

#[test]
fn a_truncated_executable_name_is_never_read_as_a_mismatch() {
    // `/proc/<pid>/comm` truncates at 15 bytes. A truncation must read as the
    // SAME program (alive, refuse), never as proof of a reused pid.
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
    // Sanity: the same shape IS provable when nothing is off.
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
        // The refuted shape: the pid is gone, and a format-1 fence cannot say
        // whether the `gh`/`curl` it spawned is. Every fence written before
        // 2026-09-18 looks like this once its publisher dies.
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
        // This process cannot judge a group it is itself inside: it would be
        // counting itself as the survivor, or worse, not counting itself.
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

    // pid 0 and pid 1 are what a corrupt field parses to; neither is an answer.
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

    // A machine that cannot name its own host or boot proves nothing either.
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

// ---------------------------------------------------------------------------
// end to end, against a real remote
// ---------------------------------------------------------------------------

#[test]
fn a_fence_this_binary_writes_is_readable_from_another_clone() {
    let fixture = bare_fixture("round-trip");
    let git_a = GitCli::new(&fixture.repo_a);
    let git_b = GitCli::new(&fixture.repo_b);
    this_invocation_publishes();
    publish::acquire_release_lease(&git_a, &fixture.owner).unwrap();
    let guard = publish::acquire_publisher_fence(&git_a, &fixture.owner).unwrap();

    // The SECOND clone has never seen the tag object: reading the identity has
    // to fetch it. That is the cross-machine path the refusal depends on.
    let identity = publish::publisher_fence_identity(&git_b, guard.token());
    assert_eq!(identity.unreadable, None, "{identity:?}");
    assert_eq!(identity.format, Some(publish::FENCE_IDENTITY_FORMAT));
    assert_eq!(identity.pid, Some(std::process::id()));
    assert_eq!(
        identity.pgid,
        Some(own_pgid()),
        "the fence records the group its gh/curl children will share"
    );
    assert_eq!(identity.host.as_deref(), Some(local_host().as_str()));
    assert_eq!(identity.boot.as_deref(), Some(local_boot().as_str()));
    assert_eq!(identity.version.as_deref(), Some("0.87.0"));
    assert!(identity.started_unix.is_some(), "{identity:?}");

    // And this process is alive, so the other clone refuses.
    assert!(matches!(
        publish::fence_liveness(&identity, &LocalProbe),
        FenceLiveness::Alive(_)
    ));
    publish::release_publisher_fence(&git_a, &guard).unwrap();
}

#[test]
fn a_live_publisher_still_refuses_and_is_never_reclaimed() {
    let fixture = bare_fixture("live-refuses");
    let git = GitCli::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    // THIS test process is the recorded publisher, and it is demonstrably alive.
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

    let refusal = publish::preflight_release_lease(&git)
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("liveness: ALIVE"), "{refusal}");
    assert!(
        refusal.contains(&format!("pid {}", std::process::id())),
        "{refusal}"
    );
    assert!(refusal.contains("Stop pid"), "{refusal}");

    match publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap() {
        FenceReclaim::Kept { refusal } => assert!(refusal.contains("liveness: ALIVE"), "{refusal}"),
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
    let git = GitCli::new(&fixture.repo_a);
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

    // The resume then acquires its OWN fence, and a second session — the loser
    // of the same reclaim race — refuses against that live winner.
    let mine = publish::acquire_publisher_fence(&git, &fixture.owner).unwrap();
    assert_ne!(mine.token(), stale);
    match publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap() {
        FenceReclaim::Kept { refusal } => assert!(refusal.contains("liveness: ALIVE"), "{refusal}"),
        other => panic!("the live winner's fence must be kept, got {other:?}"),
    }
    publish::release_publisher_fence(&git, &mine).unwrap();
}

#[test]
fn a_reused_pid_is_proof_of_death_end_to_end() {
    let fixture = bare_fixture("reused-pid");
    let git = GitCli::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    // A live pid — this very process — recorded against an executable it is
    // provably not running. That is a reused pid, and the publisher is gone.
    // Its GROUP is one that is empty: the reuser lives elsewhere, as a real
    // reuser would (a stranger handed a dead publisher's number), and the
    // publisher's own tree — where its `gh`/`curl` would be — has nobody in it.
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

/// THE REFUTED CASE, END TO END ON THE REAL PROCESS TABLE. A long-lived child
/// stands in for the `curl --upload-file` the publisher spawned; the publisher
/// itself is spawned into that same group and reaped. Its pid is gone. Its
/// upload is not — and the fence must say so, by name, until it is.
#[test]
fn a_spawned_request_that_outlives_the_publisher_keeps_the_fence_end_to_end() {
    let fixture = bare_fixture("orphaned-child");
    let git = GitCli::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    // A process group of its own, led by the survivor …
    let mut survivor = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .expect("spawn the survivor in its own group");
    let group = survivor.id();
    // … and the "publisher", spawned INTO that group and reaped: gone.
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
        FenceReclaim::Kept { refusal } => {
            assert!(refusal.contains("liveness: ALIVE"), "{refusal}");
            assert!(
                refusal.contains(&format!("pid {group}")),
                "the refusal names the survivor so the operator can stop it: {refusal}"
            );
        }
        other => panic!("a fence whose group still has a live member must be kept, got {other:?}"),
    }

    // Stop the survivor — the remedy the refusal names — and the very next
    // resume reclaims on its own.
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
    let git = GitCli::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let token = forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        // A pid that IS alive here, so only the host field can decide.
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
    let refusal = publish::preflight_release_lease(&git)
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("liveness: UNPROVABLE"), "{refusal}");
    assert!(
        refusal.contains("written on host some-other-publisher.local"),
        "{refusal}"
    );
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
    let git = GitCli::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let (dead, dead_group) = reaped_pid();
    let token = forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        // The pid is gone AND the host matches; only the boot session differs,
        // and that alone keeps the fence.
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
    let refusal = publish::preflight_release_lease(&git)
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("liveness: UNPROVABLE"), "{refusal}");
    assert!(
        refusal.contains("boot session kern.boottime:1"),
        "{refusal}"
    );
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
    let git = GitCli::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    // Byte-for-byte the message every aterm-release before this one wrote.
    let token = forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &format!(
            "aterm publisher fence for claim {}; pid 4242; nonce 17; sequence 0",
            fixture.owner
        ),
    );
    let refusal = publish::preflight_release_lease(&git)
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("liveness: UNPROVABLE"), "{refusal}");
    assert!(refusal.contains("no liveness block"), "{refusal}");
    // Its version is unknown, so the command carries THIS invocation's version
    // and says so rather than silently pretending the fence named it.
    assert!(
        refusal.contains(&format!(
            "targo --unverified ship recover v0.87.0 {} --old-publisher-stopped",
            fixture.owner
        )),
        "{refusal}"
    );
    assert!(
        refusal.contains("the fence records no version"),
        "{refusal}"
    );
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
fn the_refusal_carries_a_runnable_recover_command_for_the_real_cut() {
    let fixture = bare_fixture("recover-command");
    let git = GitCli::new(&fixture.repo_a);
    // The refusing process is cutting something ELSE (0.87.0): the command must
    // name the FENCE's version, which is the WEDGED cut's, not this one's.
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &body_for(
            &fixture.owner,
            std::process::id(),
            own_pgid(),
            &local_host(),
            &local_boot(),
            &local_exe(),
            "0.42.0",
        ),
    );
    let refusal = publish::preflight_release_lease(&git)
        .unwrap_err()
        .to_string();
    let expected = format!(
        "targo --unverified ship recover v0.42.0 {} --old-publisher-stopped",
        fixture.owner
    );
    assert!(refusal.contains(&expected), "{refusal}");
    assert!(
        !refusal.contains("recover v0.87.0"),
        "the fence's own version must win over this invocation's: {refusal}"
    );
    assert!(
        !refusal.contains("vX.Y.Z") && !refusal.contains("<full-claim-sha>"),
        "the refusal must never hand back a template: {refusal}"
    );
    assert_eq!(fixture.owner.len(), 40, "the printed sha is the full claim");

    // The command the refusal printed is the one the CLI accepts.
    let parsed = cli::parse(&[
        "recover".to_string(),
        "v0.87.0".to_string(),
        fixture.owner.clone(),
        "--old-publisher-stopped".to_string(),
    ])
    .expect("the printed recover command must parse");
    assert!(format!("{parsed:?}").contains(&fixture.owner), "{parsed:?}");
}

#[test]
fn a_fence_for_another_claim_is_never_reclaimed() {
    let fixture = bare_fixture("foreign-claim");
    let git = GitCli::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let (dead, dead_group) = reaped_pid();
    // Provably dead, but it fences a DIFFERENT claim: not this resume's to take.
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
        FenceReclaim::Kept { refusal } => {
            assert!(refusal.contains("different claim"), "{refusal}");
            assert!(
                refusal.contains(&format!(
                    "targo --unverified ship recover v0.86.0 {} --old-publisher-stopped",
                    fixture.other_owner
                )),
                "{refusal}"
            );
        }
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
    let git = GitCli::new(&fixture.repo_a);
    // No such object anywhere, and the remote cannot supply it.
    let identity = publish::publisher_fence_identity(&git, &"0".repeat(40));
    assert!(identity.unreadable.is_some(), "{identity:?}");
    assert!(matches!(
        publish::fence_liveness(&identity, &LocalProbe),
        FenceLiveness::Unprovable(_)
    ));
    // A token that is not an object id at all is rejected without shelling out.
    let malformed = publish::publisher_fence_identity(&git, "not-a-sha");
    assert!(
        malformed
            .unreadable
            .as_deref()
            .is_some_and(|reason| reason.contains("malformed")),
        "{malformed:?}"
    );
}
