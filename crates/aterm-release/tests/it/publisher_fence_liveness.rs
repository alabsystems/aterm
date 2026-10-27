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
    let format = publish::FENCE_IDENTITY_FORMAT;
    format!(
        "aterm publisher fence for claim {owner}; pid {pid}; nonce 1; sequence 0\n\n\
         fence-format: {format}\n\
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

fn local_boot_id() -> [u8; 16] {
    LocalProbe
        .boot()
        .expect("this machine publishes a boot session")
}

fn local_boot() -> String {
    publish::boot_text(local_boot_id())
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

/// The boot [`FakeProbe::here`] is in, which the fences below spell
/// `5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57`; `...4B56` is another boot.
const BOOT_7: [u8; 16] = [
    0x5C, 0x4A, 0x2E, 0x1B, 0x7D, 0x3F, 0x4A, 0x6E, 0x9B, 0x8C, 0x0D, 0x1E, 0x2F, 0x3A, 0x4B, 0x57,
];

struct FakeProbe {
    host: Option<String>,
    boot: Option<[u8; 16]>,
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
            boot: Some(BOOT_7),
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

    fn boot(&self) -> Option<[u8; 16]> {
        self.boot
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

const FULL: &str = "fence-format: 3\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
                    fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n\
                    fence-exe: /opt/aterm/aterm-release\n";

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
         fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n\
         fence-exe: /opt/aterm/aterm-release-cutter\n",
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
            "fence-format: 3\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: some-other-mac\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n",
        ),
        (
            "another boot session",
            "fence-format: 3\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B56\n",
        ),
        (
            "no pid",
            "fence-format: 3\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n",
        ),
        (
            "no boot session",
            "fence-format: 3\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n",
        ),
        (
            "a newer format this binary cannot read",
            "fence-format: 99\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n",
        ),
        (
            "an unprobeable pid",
            "fence-format: 3\nfence-pid: 5151\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n",
        ),
        (
            "no process group recorded (a format-1 fence)",
            "fence-format: 1\nfence-pid: 4242\nfence-host: publisher-host\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n\
             fence-exe: /opt/aterm/aterm-release\n",
        ),
        (
            "a process group the table could not be read for",
            "fence-format: 3\nfence-pid: 4242\nfence-pgid: 4300\nfence-host: publisher-host\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n",
        ),
        (
            "the process group this very process is in",
            "fence-format: 3\nfence-pid: 4242\nfence-pgid: 9000\nfence-host: publisher-host\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n",
        ),
        (
            "a boot session that is not a boot id",
            "fence-format: 3\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: boot-session-7\n",
        ),
        (
            "a boot id one hex digit short",
            "fence-format: 3\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B5\n",
        ),
        (
            "the all-zero boot id, which names no boot",
            "fence-format: 3\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: 00000000-0000-0000-0000-000000000000\n",
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
            "fence-format: 3\nfence-pid: {pid}\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: 5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57\n"
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

/// A boot is its 32 hex digits, which `astream_sys::parse_boot_id` reads in either letter
/// case with every hyphen dropped wherever it falls: the kernel's 8-4-4-4-12 form, none, or
/// any others. Each spelling of this boot is this boot.
#[test]
fn a_boot_is_its_sixteen_bytes_however_its_digits_are_spelled() {
    let probe = FakeProbe::here()
        .with_pid(4242, PidState::NotRunning)
        .with_group(FENCE_GROUP, &[]);
    for spelled in [
        "5C4A2E1B-7D3F-4A6E-9B8C-0D1E2F3A4B57",
        "5c4a2e1b-7d3f-4a6e-9b8c-0d1e2f3a4b57",
        "5c4A2e1B-7d3F-4a6E-9b8C-0d1E2f3A4b57",
        "5C4A2E1B7D3F4A6E9B8C0D1E2F3A4B57",
        "5-C-4-A-2E1B7D3F4A6E9B8C0D1E2F3A4B57",
        "-5C4A2E1B7D3F4A6E9B8C0D1E2F3A4B57-",
    ] {
        let identity = identity(&format!(
            "fence-format: 3\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: {spelled}\nfence-exe: /opt/aterm/aterm-release\n"
        ));
        let liveness = publish::fence_liveness(&identity, &probe);
        assert!(
            matches!(liveness, FenceLiveness::Dead(_)),
            "{spelled} is this boot, so the absent pid and empty group are proof of death: \
             {liveness:?}"
        );
    }
}

#[test]
fn a_boot_stamped_in_a_pre_astream_form_refuses_and_says_so() {
    let probe = FakeProbe::here()
        .with_pid(4242, PidState::NotRunning)
        .with_group(FENCE_GROUP, &[]);
    // The Linux stamp carries this very boot's id behind its prefix, and still refuses:
    // only a boot id is compared.
    for stamp in [
        "kern.boottime:1",
        "kern.boottime:1792190170",
        "linux-boot-id:5c4a2e1b-7d3f-4a6e-9b8c-0d1e2f3a4b57",
    ] {
        let identity = identity(&format!(
            "fence-format: 2\nfence-pid: 4242\nfence-pgid: 4200\nfence-host: publisher-host\n\
             fence-boot: {stamp}\nfence-exe: /opt/aterm/aterm-release\n"
        ));
        match publish::fence_liveness(&identity, &probe) {
            FenceLiveness::Unprovable(detail) => assert!(
                detail.contains("pre-astream form") && detail.contains(stamp),
                "the refusal names the stamp and why it cannot be compared: {detail}"
            ),
            other => panic!("{stamp} must be UNPROVABLE, got {other:?}"),
        }
    }
}

#[test]
fn a_boot_the_fence_writes_reads_back_as_the_same_bytes() {
    let mut other = BOOT_7;
    other[15] ^= 1;
    for id in [BOOT_7, other, [0xFF; 16]] {
        let text = publish::boot_text(id);
        assert_eq!(text.len(), 36, "{text}");
        assert_eq!(
            text.match_indices('-')
                .map(|(at, _)| at)
                .collect::<Vec<_>>(),
            [8, 13, 18, 23],
            "the kernel's 8-4-4-4-12 form: {text}"
        );
        assert_eq!(astream_sys::parse_boot_id(text.as_bytes()), Some(id));
    }
}

/// The last fence format whose `fence-boot` was a stamp (`kern.boottime:<secs>`,
/// `linux-boot-id:<id>`) that its readers compare as text.
const LAST_STAMPED_BOOT_FORMAT: u32 = 2;

/// A cutter built before the boot became its id reads `fence-boot` as a stamp and
/// compares it as text, so a boot id would read to it as another boot: it would tell the
/// operator a live publisher "cannot be that publisher any more". The format this binary
/// writes must be one that cutter refuses as newer than its own, by its own rule.
#[test]
fn a_fence_naming_its_boot_by_id_is_a_format_a_stamp_reader_refuses_as_newer() {
    let fixture = bare_fixture("format-names-boot-id");
    let git_a = Git::new(&fixture.repo_a);
    let git_b = Git::new(&fixture.repo_b);
    this_invocation_publishes();
    publish::acquire_release_lease(&git_a, &fixture.owner).unwrap();
    let guard = publish::acquire_publisher_fence(&git_a, &fixture.owner).unwrap();
    let written = publish::publisher_fence_identity(&git_b, guard.token());
    publish::release_publisher_fence(&git_a, &guard).unwrap();
    let boot = written.boot.as_deref().expect("the fence records its boot");
    assert_eq!(
        astream_sys::parse_boot_id(boot.as_bytes()),
        Some(local_boot_id()),
        "the fence names its boot by the kernel's boot id: {written:?}"
    );
    let format = written.format.expect("the fence records its format");
    assert!(
        format > LAST_STAMPED_BOOT_FORMAT,
        "fence-boot holds a boot id, which a format-{LAST_STAMPED_BOOT_FORMAT} reader would \
         compare as a stamp; the fence declares fence-format {format}, which that reader \
         does not refuse as newer than its own"
    );
    // Every format this binary reads is still read: a stamped fence keeps its precise
    // pre-astream refusal, never "newer than this binary's".
    let probe = FakeProbe::here()
        .with_pid(4242, PidState::NotRunning)
        .with_group(FENCE_GROUP, &[]);
    for format in 1..=publish::FENCE_IDENTITY_FORMAT {
        let identity = identity(&format!(
            "fence-format: {format}\nfence-pid: 4242\nfence-pgid: 4200\n\
             fence-host: publisher-host\nfence-boot: kern.boottime:1792190170\n"
        ));
        match publish::fence_liveness(&identity, &probe) {
            FenceLiveness::Unprovable(detail) => assert!(
                !detail.contains("newer than") && detail.contains("kern.boottime:1792190170"),
                "fence-format {format}: {detail}"
            ),
            other => panic!("fence-format {format} with a stamp must be UNPROVABLE, got {other:?}"),
        }
    }
}

/// A refusal's version note never says a fence predates the format it carries.
#[test]
fn the_version_note_names_the_format_that_first_recorded_a_version() {
    let fence = publish::PublisherFence {
        token: "1".repeat(40),
        owner: "2".repeat(40),
    };
    let liveness = FenceLiveness::Unprovable("test".to_string());
    for format in 1..=publish::FENCE_IDENTITY_FORMAT {
        let identity = identity(&format!(
            "fence-format: {format}\nfence-pid: 4242\nfence-host: publisher-host\n"
        ));
        let refusal =
            publish::publisher_fence_refusal(&fence, &identity, &liveness, Some("0.87.0"));
        for older_or_same in 1..=format {
            assert!(
                !refusal.contains(&format!("predates fence-format {older_or_same}")),
                "a fence-format {format} fence does not predate fence-format {older_or_same}: \
                 {refusal}"
            );
        }
        assert!(
            refusal.contains("v0.87.0 is THIS invocation's version"),
            "{refusal}"
        );
    }
    let unrecorded = identity("");
    let refusal = publish::publisher_fence_refusal(&fence, &unrecorded, &liveness, Some("0.87.0"));
    assert!(
        refusal.contains("predates fence-format 1"),
        "a fence with no liveness block predates the first format, which recorded the version: \
         {refusal}"
    );
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
    let mut another = local_boot_id();
    another[15] ^= 1;
    let refusal = dead_publishers_fence_naming_boot_is_kept(
        "other-boot",
        publish::FENCE_IDENTITY_FORMAT,
        &publish::boot_text(another),
    );
    assert!(refusal.contains("UNPROVABLE"), "{refusal}");
}

#[test]
fn a_fence_naming_its_boot_in_a_pre_astream_form_refuses_and_is_kept() {
    let refusal = dead_publishers_fence_naming_boot_is_kept(
        "pre-astream-boot",
        LAST_STAMPED_BOOT_FORMAT,
        "kern.boottime:1",
    );
    assert!(
        refusal.contains("UNPROVABLE") && refusal.contains("pre-astream form (kern.boottime:1)"),
        "{refusal}"
    );
}

/// A fence on this host in fence-format `format`, whose publisher is gone, its boot
/// recorded as `boot`, is refused by the preflight and kept by a `--resume`; the
/// preflight's refusal is returned.
fn dead_publishers_fence_naming_boot_is_kept(name: &str, format: u32, boot: &str) -> String {
    let fixture = bare_fixture(name);
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let (dead, dead_group) = reaped_pid();
    let body = body_for(
        &fixture.owner,
        dead,
        dead_group,
        &local_host(),
        boot,
        &local_exe(),
        "0.87.0",
    )
    .replace(
        &format!("fence-format: {}\n", publish::FENCE_IDENTITY_FORMAT),
        &format!("fence-format: {format}\n"),
    );
    let token = forge_fence(&fixture.repo_a, &fixture.owner, &body);
    let refusal = publish::preflight_release_lease(&git)
        .unwrap_err()
        .to_string();
    assert!(matches!(
        publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap(),
        FenceReclaim::Kept { .. }
    ));
    assert_eq!(
        publish::publisher_fence(&git).unwrap().unwrap().token,
        token
    );
    refusal
}

#[test]
fn a_fence_naming_this_boot_in_the_other_letter_case_is_reclaimed() {
    let fixture = bare_fixture("boot-case");
    let git = Git::new(&fixture.repo_a);
    this_invocation_publishes();
    publish::acquire_release_lease(&git, &fixture.owner).unwrap();
    let ours = local_boot();
    let swapped: String = ours
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    assert_ne!(
        swapped, ours,
        "this boot's id has a hex letter whose case can be swapped"
    );
    let (dead, dead_group) = reaped_pid();
    let stale = forge_fence(
        &fixture.repo_a,
        &fixture.owner,
        &body_for(
            &fixture.owner,
            dead,
            dead_group,
            &local_host(),
            &swapped,
            &local_exe(),
            "0.87.0",
        ),
    );
    match publish::reclaim_dead_publisher_fence(&git, &fixture.owner, &LocalProbe).unwrap() {
        FenceReclaim::Reclaimed { token, .. } => assert_eq!(token, stale),
        other => panic!("{swapped} is this boot ({ours}), so the fence is reclaimed: {other:?}"),
    }
    assert_eq!(publish::publisher_fence(&git).unwrap(), None);
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
