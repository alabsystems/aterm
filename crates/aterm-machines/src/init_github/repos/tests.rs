// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use crate::git_credential::script;
use crate::items::defaults::tests::user;

use crate::port::{Fake, GithubResponse, Live, Ran, Stat};
use std::path::PathBuf;

mod advertised_clone_tests;
mod clone_route_tests;
mod default_branch_tests;
mod hook_installer_tests;
mod staging_tests;

const TOKEN: &str = "fixture_synced_token_only";
const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";

fn directory(uid: u32) -> Stat {
    Stat {
        uid,
        gid: 20,
        mode: 0o700,
        dir: true,
        symlink: false,
        len: 0,
    }
}

fn fixture() -> (Fake, User) {
    let mut sys = Fake::root();
    let who = user("alice", 501);
    sys.euid = who.uid;
    sys.stat_of(&who.home, directory(who.uid));
    sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
    // Her vault token, which aterm's route hands back.
    sys.vault_set(&who.home, crate::init_github::TOKEN_ENTRY, TOKEN.as_bytes());
    script::launcher(&sys, &who);
    let helper = crate::init_github::helper(&sys, &who).expect("the launcher is there");
    script::hands_back(&sys, &who, &helper, TOKEN);
    sys.on_github_api(&who, "/user", json(r#"{"login":"alice"}"#));
    (sys, who)
}

fn value(name: &str, id: u64, private: bool) -> Value {
    aterm_json::json!({
        "id": id, "name": name, "full_name": format!("alice/{name}"),
        "owner": {"login": "alice"}, "private": private,
        "clone_url": format!("https://github.com/alice/{name}.git"),
        "default_branch": "main", "size": 0,
    })
}

fn json(body: &str) -> Result<GithubResponse, String> {
    Ok(GithubResponse {
        status: 200,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: body.as_bytes().to_vec(),
    })
}

/// `path` a finished checkout of alice's `name`: [`script_worktree`], with a HEAD commit and
/// its index.
pub(in crate::init_github) fn script_existing(sys: &Fake, who: &User, path: &Path, name: &str) {
    script_existing_head(sys, who, path, name, Some(A));
    script_index(sys, path);
}

/// `path` a top-level worktree of alice's `name` at its GitHub origin ([`script_worktree`])
/// whose HEAD names `head` ([`script_head`]), its index unscripted.
fn script_existing_head(sys: &Fake, who: &User, path: &Path, name: &str, head: Option<&str>) {
    script_worktree(sys, who, path, name);
    script_head(sys, who, path, head);
}

/// What [`verify_existing`] and [`crate::checkout::unborn`] hear of whether `path`'s HEAD
/// names a commit it has: the refs its record of facts reads
/// ([`crate::checkout::facts::tests::script_refs`]), `main` and `origin/main` at `head`, or
/// neither (a branch with no commit). Each scripted answer is heard in turn, the last again.
pub(in crate::init_github) fn script_head(sys: &Fake, who: &User, path: &Path, head: Option<&str>) {
    crate::checkout::facts::tests::script_refs(sys, who, path, head);
}

/// Where a scripted checkout at `path` keeps its index (its record's `--git-path index`): in a
/// git directory outside the worktree, as a linked worktree's is, so that no staging the fake
/// allocates and no publish rename it checks for a collision meets it.
pub(in crate::init_github) fn fake_index(path: &Path) -> PathBuf {
    Path::new("/fake-git-dirs")
        .join(path.strip_prefix("/").unwrap_or(path))
        .join("index")
}

/// `path`'s index ([`fake_index`]), as a finished checkout leaves it.
pub(in crate::init_github) fn script_index(sys: &Fake, path: &Path) {
    sys.file(fake_index(path).to_str().unwrap(), b"DIRC");
}

/// `path` a top-level worktree of alice's `name` at its GitHub origin, `main` tracking origin's
/// ([`crate::checkout::facts::tests::script`]), its refs unscripted ([`script_head`]).
pub(in crate::init_github) fn script_worktree(sys: &Fake, who: &User, path: &Path, name: &str) {
    let origin = format!("https://github.com/alice/{name}.git");
    crate::checkout::facts::tests::script(
        sys,
        who,
        path,
        &fake_index(path),
        &[
            ("remote.origin.url", origin.as_str()),
            ("branch.main.remote", "origin"),
            ("branch.main.merge", "refs/heads/main"),
        ],
    );
}

/// What a read of the checkout at `path` asks beyond its record of facts: where git reaches
/// origin, and the `status` that says whether its files hold the person's own work.
fn script_read(sys: &Fake, who: &User, path: &Path, name: &str, status: &str) {
    sys.on(
        &git(sys, who, Some(path))
            .args(["ls-remote", "--get-url", "origin"])
            .display(),
        Ran::ok(&format!("https://github.com/alice/{name}.git\n")),
    );
    sys.on(
        &crate::checkout::Programs::default()
            .off(git(sys, who, Some(path)))
            .args(crate::checkout::NO_SIDE_RUNS)
            .args([
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=normal",
                "--ignore-submodules=all",
            ])
            .display(),
        Ran::ok(status),
    );
}

const GIB: u64 = aterm_capacity::floor::GIB;

/// A data volume of 100 GiB with `free` bytes free and a floor of 8 GiB.
fn volume(free: u64) -> aterm_capacity::reading::Volume {
    aterm_capacity::reading::Volume {
        mount: PathBuf::from("/System/Volumes/Data"),
        total: 100 * GIB,
        free,
        floor: 8 * GIB,
        room: 8 * GIB,
    }
}

/// A repository named `big` that GitHub, or the vault's catalog, sizes at `size_kib`.
fn sized(size_kib: u64) -> Repo {
    Repo {
        owner: "alice".into(),
        name: "big".into(),
        default_branch: Some("main".into()),
        size_kib,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    }
}

/// A clone runs while five times the class of its size ([`size_class`]) leaves the disk
/// watch's floor free, to the byte; one that would not is named and left; an unread volume runs
/// every clone.
#[test]
fn a_clone_runs_only_while_it_leaves_the_disk_watchs_floor_free() {
    let repo = sized;
    assert_eq!(no_room(None, &repo(u64::MAX)), None);
    assert_eq!(short_of_room(None, &[], "run aterm init again"), None);

    // GitHub's 0 (empty, or not sized yet) still needs room above the floor.
    assert_eq!(no_room(Some(&volume(8 * GIB + 5 * 1024)), &repo(0)), None);
    assert_eq!(
        no_room(Some(&volume(8 * GIB + 5 * 1024 - 1)), &repo(0)).as_deref(),
        Some("~/big")
    );
    assert_eq!(
        no_room(Some(&volume(3 * GIB)), &repo(0)).as_deref(),
        Some("~/big")
    );

    // Every size of at most 1 MiB is in the least class.
    let volume = volume(8 * GIB + 5 * 1024 * 1024);
    assert_eq!(no_room(Some(&volume), &repo(1)), None);
    assert_eq!(no_room(Some(&volume), &repo(1024)), None);
    let left = [
        no_room(Some(&volume), &repo(1025)).unwrap(),
        no_room(Some(&volume), &repo(2 * 1024 * 1024)).unwrap(),
    ];
    assert_eq!(left, ["~/big", "~/big (about 10 GiB)"]);
    assert_eq!(
        short_of_room(Some(&volume), &[], "run aterm init again"),
        None
    );
    assert_eq!(
        short_of_room(Some(&volume), &left, "try again").unwrap(),
        "2 repositories not cloned, for want of disk space: ~/big, ~/big (about 10 GiB); \
         /System/Volumes/Data has 8 GiB free and keeps 8 GiB free; try again"
    );
}

/// Setup reads a size as its class ([`size_class`]): every size in a class needs five times the
/// class. A size an earlier build wrote to the vault's catalog, which a refresh keeps while
/// GitHub's size is in its class, needs room for five times every GitHub size it is kept for:
/// a stored 2 GiB and 1 KiB, in the class (2, 2.5] GiB, needs 12.5 GiB, not 10 GiB. A class
/// the catalog writes needs five times itself, under a quarter more than five times GitHub's
/// size: the class GitHub's 2 GiB and 1 KiB is written as needs 12.5 GiB, not 20 GiB.
#[test]
fn a_size_needs_five_times_its_class() {
    for (size_kib, need_bytes) in [
        (0, 5 * 1024),
        (1, 5 * 1024 * 1024),
        (1_024, 5 * 1024 * 1024),
        (1_025, 5 * 1_280 * 1024),
        (GIB / 1024, 5 * GIB),
        (2 * GIB / 1024, 10 * GIB),
        (2 * GIB / 1024 + 1, 25 * GIB / 2),
        (5 * GIB / 2 / 1024, 25 * GIB / 2),
        (5 * GIB / 2 / 1024 + 1, 15 * GIB),
        (u64::MAX, u64::MAX),
    ] {
        assert_eq!(
            need(&sized(size_kib)),
            need_bytes,
            "a size of {size_kib} KiB"
        );
    }
    // 15 GiB above the floor clones the class GitHub's 2 GiB and 1 KiB is written as.
    let roomy = volume(23 * GIB);
    assert_eq!(no_room(Some(&roomy), &sized(2 * GIB / 1024 + 1)), None);
    assert_eq!(no_room(Some(&roomy), &sized(5 * GIB / 2 / 1024)), None);
    assert_eq!(
        no_room(Some(&roomy), &sized(5 * GIB / 2 / 1024 + 1)),
        None,
        "15 GiB fits in 15 GiB, to the byte"
    );
    assert!(no_room(Some(&roomy), &sized(3 * GIB / 1024 + 1)).is_some());
}

/// A pass reads the disk once, and each clone it admits holds its room for the ones after
/// it: of two repositories that each fit alone but not together, the first clones and the
/// second is left with the reason, never started.
#[test]
fn each_clone_a_pass_admits_holds_its_room_for_the_ones_after_it() {
    let (mut sys, who) = fixture();
    // 8 GiB above the floor: room for one clone of about 5 GiB, not for two.
    sys.volume = Some(volume(16 * GIB));
    let sized = |name: &str, sequence| {
        let (mut repo, _, _) = script_clone_named(&sys, &who, name, sequence, false);
        repo.size_kib = GIB / 1024;
        repo
    };
    let missing = [sized("one", 0), sized("two", 1)];
    let cloned = clone_all(&sys, &who, &missing, &[], &BTreeSet::new(), None, None);
    let names: Vec<&str> = cloned
        .cloned
        .iter()
        .map(|repo| repo.name.as_str())
        .collect();
    assert_eq!(names, ["one"]);
    assert!(cloned.failed.is_empty(), "{:?}", cloned.failed);
    assert_eq!(cloned.left, ["~/two (about 5 GiB)"]);
    let observed = sys.repository_observations();
    let one = observed
        .iter()
        .filter(|p| p.name == "one")
        .collect::<Vec<_>>();
    assert_eq!(one.first().unwrap().phase, Phase::Queued);
    assert_eq!(one.last().unwrap().phase, Phase::Ready);
    assert!(one.last().unwrap().usable);
    let placing = one.iter().position(|p| p.phase == Phase::Placing).unwrap();
    let dependencies = one
        .iter()
        .position(|p| p.phase == Phase::Dependencies)
        .unwrap();
    assert!(placing < dependencies);
    assert!(one[..=placing].iter().all(|p| !p.usable));
    assert!(
        observed
            .iter()
            .any(|p| p.name == "two" && p.phase == Phase::WaitingForSpace)
    );
    let calls = sys.calls();
    assert!(
        calls.iter().all(|call| !call.contains("alice/two")),
        "{calls:?}"
    );
}

/// Repositories read on several lanes still stand in the order GitHub listed them.
#[test]
fn a_partition_read_on_lanes_keeps_the_list_order() {
    let (sys, who) = fixture();
    let mut names: Vec<String> = (0..3 * READ_LANES)
        .map(|index| format!("r{index:02}"))
        .collect();
    names.insert(READ_LANES, "Music".to_owned());
    let mut missing = Vec::new();
    let mut notes = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let path = Path::new(&who.home).join(name);
        if name == "Music" {
            notes.push("~/Music is a standard home folder; left untouched".to_owned());
            continue;
        }
        match index % 3 {
            0 => {
                missing.push(name.clone());
                continue;
            }
            1 => sys.file(path.to_str().unwrap(), b"owner file"),
            _ => sys.link(path.to_str().unwrap(), "/elsewhere"),
        };
        notes.push(format!("~/{name} is occupied or linked; left untouched"));
    }
    let all: Vec<Repo> = names
        .iter()
        .zip(1..)
        .map(|(name, id)| repo(&value(name, id, true), Some("alice")).unwrap())
        .collect();
    let standing = partition(&sys, &who, &all).unwrap();
    let read: Vec<_> = standing.missing.iter().map(|repo| &repo.name).collect();
    assert_eq!(read, missing.iter().collect::<Vec<_>>());
    assert_eq!(standing.notes, notes);
    assert!(standing.present.is_empty() && standing.unborn.is_empty());
}

fn script_clone(sys: &Fake, who: &User, empty: bool) -> (Repo, PathBuf, String) {
    script_clone_named(sys, who, "new", 0, empty)
}

#[test]
fn deferred_alignment_records_the_local_condition_and_keeps_the_checkout_usable() {
    use crate::repository_progress::{OutcomeReason, PreservationReason, RetryDisposition};
    let (sys, who) = fixture();
    let path = Path::new(&who.home).join("dirty");
    script_existing(&sys, &who, &path, "dirty");
    script_read(&sys, &who, &path, "dirty", "?? owner-work\0");
    let origin = crate::checkout::Origin(vec![crate::checkout::Remote::Github {
        owner: "alice".into(),
        name: "dirty".into(),
    }]);
    assert!(matches!(
        crate::checkout::align(&sys, &who, &path, &origin),
        Ok(Stands::Kept(_))
    ));
    let rows = sys.repository_observations();
    let row = rows.last().unwrap();
    assert_eq!(row.phase, Phase::Deferred);
    assert!(row.usable && row.placed);
    let outcome = row.outcome.as_ref().unwrap();
    assert_eq!(outcome.stage, Phase::Checking);
    assert_eq!(
        outcome.reason,
        OutcomeReason::Preserved(PreservationReason::LocalChanges)
    );
    assert_eq!(outcome.retry, RetryDisposition::AfterChanges);
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains(" fetch ") && !call.contains(" merge "))
    );
}

#[test]
fn clone_failures_publish_typed_reasons_without_transport_or_callback_diagnostics() {
    use crate::repository_progress::{FailureKind, OutcomeReason, RetryDisposition};
    let diagnostic = [
        "https://",
        "fixture-user:",
        "fixture-value",
        "@example.invalid/repo",
    ]
    .concat();
    for network in [false, true] {
        let (sys, who) = fixture();
        let (repo, _, _) = script_clone(&sys, &who, false);
        let stderr = diagnostic.clone();
        sys.simulate(move |_, command| {
            (network && fetches_advertised(&command.display())).then(|| Ran::exit(128, &stderr))
        });
        let current = || {
            if network {
                Ok(())
            } else {
                Err(diagnostic.clone())
            }
        };
        let guard = CloneGuard::new(&current);
        let result = clone_all(
            &sys,
            &who,
            &[repo],
            &[],
            &BTreeSet::new(),
            Some(&guard),
            None,
        );
        assert_eq!(result.failed.len(), 1);
        let observed = sys.repository_observations();
        let row = observed.last().unwrap();
        assert_eq!(row.phase, Phase::Failed);
        assert!(!row.usable && !row.placed);
        let outcome = row.outcome.as_ref().unwrap();
        assert_eq!(
            outcome.stage,
            if network {
                Phase::Connecting
            } else {
                Phase::Queued
            }
        );
        assert_eq!(
            outcome.reason,
            OutcomeReason::Failed(if network {
                FailureKind::Network
            } else {
                FailureKind::Local
            })
        );
        assert_eq!(outcome.retry, RetryDisposition::Unrecorded);
        let snapshot = aterm_json::to_string(row).unwrap();
        assert!(!snapshot.contains("fixture-value") && !snapshot.contains("://"));
        assert!(!outcome.reason.label().contains("authentication"));
    }
}

/// A STALLED CLONE IS A NETWORK FAILURE: a clone whose transfer made no progress for its window
/// ([`crate::port::RunFailed::Stalled`]) fails its line and its row as the network's, counts
/// toward the pass's network failures, and says it made no progress; it is never a local fault.
/// NEGATIVE CONTROL: the same clone ended at a wall-clock deadline ([`Fake::time_out`]) fails as
/// local, outside the network count.
#[test]
fn a_stalled_clone_is_a_network_failure() {
    use crate::repository_progress::{FailureKind, OutcomeReason};
    for stalled in [true, false] {
        let (sys, who) = fixture();
        let (repo, _, key) = script_clone(&sys, &who, false);
        if stalled {
            sys.stall(&key);
        } else {
            sys.time_out(&key);
        }
        let cloned = clone_all(&sys, &who, &[repo], &[], &BTreeSet::new(), None, None);
        assert!(cloned.cloned.is_empty(), "stalled={stalled}");
        assert_eq!(
            cloned.failed.len(),
            1,
            "stalled={stalled}: {:?}",
            cloned.failed
        );
        assert_eq!(cloned.network_failed, stalled, "{:?}", cloned.failed);
        assert_eq!(
            cloned.failed[0].contains("GitHub clone did not finish (no progress for"),
            stalled,
            "{:?}",
            cloned.failed
        );
        let observed = sys.repository_observations();
        let row = observed.last().unwrap();
        assert_eq!(row.phase, Phase::Failed);
        assert!(!row.usable && !row.placed);
        assert_eq!(
            row.outcome.as_ref().unwrap().reason,
            OutcomeReason::Failed(if stalled {
                FailureKind::Network
            } else {
                FailureKind::Local
            })
        );
    }
}

/// A clone of github.com/alice/`name` into the `sequence`th staging directory the run
/// allocates, scripted from its first read to its last.
pub(in crate::init_github) fn script_clone_named(
    sys: &Fake,
    who: &User,
    name: &str,
    sequence: u64,
    empty: bool,
) -> (Repo, PathBuf, String) {
    script_clone_named_route(sys, who, name, sequence, empty, false)
}

/// Where the `sequence`th clone of github.com/alice/`name` stages its repository, and the
/// display of that clone's one fetch of what GitHub advertised ([`fetch_command`]), under which
/// a script adds what the clone writes.
pub(in crate::init_github) fn clone_key(
    sys: &Fake,
    who: &User,
    name: &str,
    sequence: u64,
) -> (PathBuf, String) {
    let staging = Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", sequence);
    let staged = staged(&staging);
    let command = fetch_command(
        &crate::checkout::network(sys, who, None),
        &staged,
        &format!("https://github.com/alice/{name}.git"),
        &staging.join(ADVERTISED),
    );
    (staged, command.display())
}

/// [`script_clone_named`], its effective URL a same-repository SSH rewrite when `rewritten`:
/// the person's own URL, which the clone keeps, under aterm's credential route. The display it
/// hands back is the one the clone's content arrives under: its fetch ([`clone_key`]), or, for
/// an `empty` remote, which fetches nothing, its `git init`.
pub(in crate::init_github) fn script_clone_named_route(
    sys: &Fake,
    who: &User,
    name: &str,
    sequence: u64,
    empty: bool,
    rewritten: bool,
) -> (Repo, PathBuf, String) {
    let repo = Repo {
        owner: "alice".into(),
        name: name.into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    };
    let url = format!("https://github.com/alice/{name}.git");
    let url = url.as_str();
    let endpoint = if rewritten {
        format!("git@github.com:alice/{name}.git")
    } else {
        url.to_owned()
    };
    sys.on(
        &git(sys, who, None)
            .args(["ls-remote", "--get-url", "--", url])
            .display(),
        Ran::ok(&endpoint),
    );
    let route = || crate::checkout::network(sys, who, None);
    let remote = if empty {
        String::new()
    } else {
        format!(
            "ref: refs/heads/main\tHEAD\n{A}\tHEAD\n{A}\trefs/heads/main\n{B}\trefs/heads/topic\n{B}\trefs/tags/v1\n"
        )
    };
    let command = route().args([
        "ls-remote",
        "--symref",
        "--",
        url,
        "HEAD",
        "refs/heads/*",
        "refs/tags/*",
    ]);
    sys.on(&command.display(), Ran::ok(&remote));
    // What the clone runs in its staging ([`clone_advertised`]): its `git init`, which makes the
    // repository, origin's configuration, and, for a remote with a commit, its fetch, origin's
    // HEAD and the checkout that writes its index.
    let (staged, fetch) = clone_key(sys, who, name, sequence);
    let init = init_command(&route(), "sha1", Some("main"), &staged).display();
    sys.on(&init, Ran::ok(""));
    sys.on_stat(&init, &staged, directory(who.uid));
    sys.on_stat(&init, &staged.join(".git"), directory(who.uid));
    sys.on_stat(&init, &staged.join(".git/objects"), directory(who.uid));
    sys.on_write(&init, &staged.join(".git/fixture"), b"complete clone");
    let mut configured = vec![
        ("remote.origin.url", url.to_owned()),
        ("remote.origin.fetch", ORIGIN_FETCH.to_owned()),
    ];
    if empty {
        configured.push(("branch.main.remote", "origin".to_owned()));
        configured.push(("branch.main.merge", "refs/heads/main".to_owned()));
    } else {
        sys.on(&fetch, Ran::ok(""));
        sys.on(
            &origin_head_command(&route(), &staged, "main").display(),
            Ran::ok(""),
        );
        let checkout = checkout_command(&route(), &staged, "main").display();
        sys.on(&checkout, Ran::ok(""));
        // A finished clone of a commit: git wrote its index as it checked it out.
        sys.on_write(&checkout, &fake_index(&staged), b"DIRC");
    }
    for (key, value) in &configured {
        sys.on(
            &config_command(&route(), &staged, key, value).display(),
            Ran::ok(""),
        );
    }
    script_existing_head(sys, who, &staged, name, (!empty).then_some(A));
    if empty {
        let destination = Path::new(&who.home).join(name);
        script_existing_head(sys, who, &destination, name, None);
        for (index, path) in [
            destination.clone(),
            destination.join(".git"),
            destination.join(".git/objects"),
        ]
        .into_iter()
        .enumerate()
        {
            sys.file_id_of(
                &path,
                FileId {
                    dev: 1,
                    ino: 100 + sequence * 3 + index as u64,
                    born_ns: Some(1),
                },
            );
        }
        for (args, ran) in [
            (
                vec!["for-each-ref", "--format=%(objectname) %(refname)"],
                Ran::ok(""),
            ),
            (
                vec!["ls-remote", "--get-url", "--", "origin"],
                Ran::ok(&endpoint),
            ),
        ] {
            sys.on(&git(sys, who, Some(&destination)).args(args).display(), ran);
        }
    }
    let head = git(sys, who, Some(&staged)).args(["rev-parse", "--verify", "HEAD^{commit}"]);
    sys.on(
        &head.display(),
        if empty {
            Ran::exit(128, "unborn")
        } else {
            Ran::ok(A)
        },
    );
    let refs = if empty {
        String::new()
    } else {
        format!(
            "{A} refs/heads/main\n{A} refs/remotes/origin/HEAD\n{A} refs/remotes/origin/main\n{B} refs/remotes/origin/topic\n{B} refs/tags/v1\n"
        )
    };
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["for-each-ref", "--format=%(objectname) %(refname)"])
            .display(),
        Ran::ok(&refs),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .display(),
        Ran::ok("refs/heads/main\n"),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
            .display(),
        Ran::ok("refs/remotes/origin/main\n"),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["config", "--get", "branch.main.remote"])
            .display(),
        Ran::ok("origin\n"),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["config", "--get", "branch.main.merge"])
            .display(),
        Ran::ok("refs/heads/main\n"),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
            .display(),
        Ran::ok(""),
    );
    (repo, staged, if empty { init } else { fetch })
}

/// A clone's publish rename is not repeated safely, so it holds the update boundary while
/// it runs (init-on-by-default design §6.7): an update's handoff waits for it, boundedly.
/// Before, nothing held the guard, and a handoff could cut the publish in half.
#[test]
fn a_clones_publish_rename_holds_the_update_boundary() {
    let (sys, who) = fixture();
    let (repo, _, _) = script_clone(&sys, &who, false);
    clone_one(&sys, &who, "alice", &repo).unwrap();
    assert_eq!(
        sys.guarded_renames(),
        [Path::new(&who.home).join("new")],
        "the publish rename ran under the guard"
    );
}

/// A guarded clone checks `current` before every step, the publish included. The publish's
/// guard waits for a pending update handoff (design §6.7), up to minutes, after the last
/// check; once the handoff is refused it checks `current` again, and publishes nothing when
/// the vault changed meanwhile. Before, the clone was published on the check made before the
/// wait.
#[test]
fn a_clone_publish_that_waited_for_a_handoff_checks_current_again() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for changes in [true, false] {
        let (sys, who) = fixture();
        let changed = AtomicBool::new(false);
        let current = || {
            if changed.load(Ordering::SeqCst) {
                Err("injected: the vault changed while the publish waited".to_owned())
            } else {
                Ok(())
            }
        };
        let (repo, _staged, _) = script_clone(&sys, &who, false);
        let latch = sys.boundary.latch_handoff(Duration::from_secs(60));
        let result = std::thread::scope(|scope| {
            let worker = scope.spawn(|| clone_one_guarded(&sys, &who, &repo, &current));
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while sys.boundary.waiting() == 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the publish never waited"
                );
                aterm_uds::deadline::poll_yield();
            }
            changed.store(changes, Ordering::SeqCst);
            drop(latch);
            worker.join().unwrap()
        });
        let published = sys
            .calls()
            .iter()
            .any(|call| call.starts_with("rename-noreplace "));
        if changes {
            assert!(
                !published,
                "the clone was published after the vault changed during its wait: {result:?}"
            );
            assert!(
                matches!(&result, Err(why) if why.contains("the vault changed")),
                "{result:?}"
            );
        } else {
            // CONTROL: nothing changed while it waited, and it publishes once.
            assert!(published && result.is_ok(), "{result:?}");
        }
        assert_eq!(sys.boundary.any_held(), None);
    }
}

/// A guarded clone carries aterm's credential route and no keychain helper, and refuses to
/// go on once the vault snapshot changed.
#[test]
fn guarded_clones_carry_the_route_and_refuse_newer_versions() {
    for changed in [false, true] {
        let (sys, who) = fixture();
        let current = || {
            if changed && sys.calls().iter().any(|call| fetches_advertised(call)) {
                Err("injected newer vault version".into())
            } else {
                Ok(())
            }
        };
        let (repo, _staged, _) = script_clone(&sys, &who, false);
        let result = clone_one_guarded(&sys, &who, &repo, &current);
        assert_eq!(result.is_ok(), !changed, "{result:?}");
        let final_path = Path::new(&who.home).join("new");
        assert_eq!(sys.stat(&final_path).is_ok(), !changed);
        let calls = sys.calls();
        let network: Vec<_> = calls
            .iter()
            .filter(|call| call.contains("ls-remote --symref") || fetches_advertised(call))
            .collect();
        assert_eq!(network.len(), 2, "{calls:?}");
        assert!(
            network.iter().all(
                |call| call.contains("-c credential.helper= ") && !call.contains("osxkeychain")
            ),
            "{network:?}"
        );
        assert!(calls.iter().all(|call| !call.contains("--global")));
        assert_eq!(
            calls
                .iter()
                .any(|call| call.starts_with("rename-noreplace ")),
            !changed
        );
    }
}

/// Two clones of one repository race (`sync.clone` and a second `aterm init`): the one that
/// finds it in place at its origin, before it clones or as it publishes, is done, and its
/// staging goes; the racing worktree is never touched.
#[test]
fn a_clone_that_finds_the_repository_in_place_at_its_origin_is_done() {
    for at_publish in [false, true] {
        let (sys, who) = fixture();
        let staged = Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
        let (repo, _, clone_key) = script_clone(&sys, &who, false);
        let destination = Path::new(&who.home).join("new");
        script_existing(&sys, &who, &destination, "new");
        if at_publish {
            sys.on_stat(&clone_key, &destination, directory(who.uid));
            sys.on_write(&clone_key, &destination.join(".git/racer"), b"racing clone");
        } else {
            sys.stat_of(destination.to_str().unwrap(), directory(who.uid));
            sys.file(
                destination.join(".git/racer").to_str().unwrap(),
                b"racing clone",
            );
        }
        assert_eq!(
            clone_one(&sys, &who, "alice", &repo),
            Ok(Placed::InPlace),
            "{at_publish}"
        );
        assert_eq!(
            sys.read_file(&destination.join(".git/racer")).unwrap(),
            b"racing clone"
        );
        assert!(sys.read_file(&destination.join(".git/fixture")).is_err());
        assert!(sys.stat(&staged).is_err(), "{at_publish}");
        assert_eq!(
            sys.calls().iter().any(|call| fetches_advertised(call)),
            at_publish,
            "{:?}",
            sys.calls()
        );
    }
}

/// A racing worktree at this repository's origin whose HEAD names no commit (another
/// `git clone` that has not fetched yet, or one cut off before it did) is no repository in
/// place, before this clone or as it publishes: the pass says it left it untouched, and the
/// next pass's standing read takes it up as the unfinished checkout it is
/// ([`crate::checkout::unborn`]). Before, the race arms accepted the origin and index check
/// alone, which says nothing of a HEAD with no commit, and read it in place.
#[test]
fn a_racing_checkout_with_no_commit_is_never_in_place() {
    for at_publish in [false, true] {
        let (sys, who) = fixture();
        let (repo, _, clone_key) = script_clone(&sys, &who, false);
        let destination = Path::new(&who.home).join("new");
        script_existing_head(&sys, &who, &destination, "new", None);
        if at_publish {
            sys.on_stat(&clone_key, &destination, directory(who.uid));
            sys.on_write(&clone_key, &destination.join(".git/racer"), b"racing clone");
        } else {
            sys.stat_of(destination.to_str().unwrap(), directory(who.uid));
            sys.file(
                destination.join(".git/racer").to_str().unwrap(),
                b"racing clone",
            );
        }
        let placed = clone_one(&sys, &who, "alice", &repo);
        assert!(
            placed
                .as_ref()
                .is_err_and(|why| why.contains("left untouched")),
            "{at_publish}: {placed:?}"
        );
        assert_eq!(
            sys.read_file(&destination.join(".git/racer")).unwrap(),
            b"racing clone",
            "{at_publish}: the racing worktree is never touched"
        );
    }
}

#[test]
fn failure_or_a_destination_created_during_clone_never_overwrites_owner_data() {
    for race in [false, true] {
        let (sys, who) = fixture();
        let staged = Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
        if !race {
            // The clone's record of facts cannot be read.
            sys.on(
                &crate::checkout::facts::rev_parse(&sys, &who, &staged).display(),
                Ran::exit(1, "failure includes secret-like bytes that must not escape"),
            );
        }
        let (repo, _, clone_key) = script_clone(&sys, &who, false);
        let destination = Path::new(&who.home).join("new");
        if race {
            sys.on_stat(&clone_key, &destination, directory(who.uid));
            sys.on_write(&clone_key, &destination.join("owner.txt"), b"racing owner");
        }
        let error = clone_one(&sys, &who, "alice", &repo).unwrap_err();
        assert!(!error.contains("secret-like"));
        assert!(sys.stat(&staged).is_err());
        if race {
            assert_eq!(
                sys.read_file(&destination.join("owner.txt")).unwrap(),
                b"racing owner"
            );
            assert!(sys.read_file(&destination.join(".git/fixture")).is_err());
        } else {
            assert!(sys.stat(&destination).is_err());
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.starts_with("rename-noreplace "))
            );
        }
    }
    for endpoint in [
        "https://mirror.invalid/alice/new.git",
        "https://github.com/other/new.git",
        "https://private-token@github.com/alice/new.git",
        "file:///owner/repository",
        "https://github.com/alice/new.git\nhttps://elsewhere.invalid/new.git",
        "",
    ] {
        let (sys, who) = fixture();
        let key = git(&sys, &who, None)
            .args([
                "ls-remote",
                "--get-url",
                "--",
                "https://github.com/alice/new.git",
            ])
            .display();
        sys.on(&key, Ran::ok(endpoint));
        let (repo, _, _) = script_clone(&sys, &who, false);
        let error = clone_one(&sys, &who, "alice", &repo).unwrap_err();
        assert!(!error.contains(endpoint) || endpoint.is_empty());
        assert!(sys.calls().iter().all(|call| !call.contains("--symref")
            && !fetches_advertised(call)
            && !call.contains("scratch allocated")));
    }
    let (sys, who) = fixture();
    let key = git(&sys, &who, None)
        .args([
            "ls-remote",
            "--get-url",
            "--",
            "https://github.com/alice/new.git",
        ])
        .display();
    sys.on(&key, Ran::ok("https://github.com/alice/new.git"))
        .on(&key, Ran::ok("https://mirror.invalid/alice/new.git"));
    let (repo, _, _) = script_clone(&sys, &who, false);
    assert!(clone_one(&sys, &who, "alice", &repo).is_err());
    assert!(sys.calls().iter().any(|call| call.contains("--symref")));
    assert!(
        sys.calls()
            .iter()
            .all(|call| !fetches_advertised(call) && !call.contains("scratch allocated"))
    );
}

/// A promisor remote whose name is not UTF-8 makes a partial clone as any other does, and
/// [`verify_integrity`] refuses it: the checkout's record lists its config in the C locale
/// ([`crate::checkout::facts::config_listing`]). Under the account's UTF-8 locale git's `.`
/// matches no such byte, the remote went unlisted and the check passed. The controls: a repository with no promisor passes; this one's
/// remote is live (a read of an object the repository lacks, lazy fetches allowed, fetches it
/// from there); and the same listing under the account's own locale misses it (a COULD NOT
/// RUN where it does not, a host whose git matches those bytes there or lacks the locale).
#[test]
fn a_promisor_remote_named_in_bytes_that_are_not_utf8_is_refused() {
    use std::io::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    crate::items::github::need_native_git();
    let sys = Live::new();
    let home = Scratch::new(&sys, "github-promisor-bytes").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = home.path().to_string_lossy().into_owned();
    let run = |at: Option<&Path>, args: &[&str]| {
        text(
            &sys,
            &git(&sys, &who, at)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(crate::checkout::NO_SIDE_RUNS)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                ])
                .args(args.iter().copied()),
            "isolated local Git fixture failed",
        )
        .unwrap()
    };
    let source = home.path().join("source");
    let repo = home.path().join("repo");
    run(
        None,
        &["init", "--quiet", "--template=", source.to_str().unwrap()],
    );
    sys.write_file(&source.join("code"), b"only the source has this\n", 0o600)
        .unwrap();
    run(Some(&source), &["add", "--", "code"]);
    run(Some(&source), &["commit", "--quiet", "-m", "fixture"]);
    let blob = run(Some(&source), &["rev-parse", "HEAD:code"]);
    let blob = blob.trim();
    run(
        None,
        &["init", "--quiet", "--template=", repo.to_str().unwrap()],
    );
    assert_eq!(
        verify_integrity(&sys, &who, &repo),
        Ok(()),
        "control: a repository with no promisor passes"
    );
    let mut config = std::fs::File::options()
        .append(true)
        .open(repo.join(".git/config"))
        .unwrap();
    config.write_all(b"[remote \"\xff\"]\n\turl = ").unwrap();
    config.write_all(source.as_os_str().as_bytes()).unwrap();
    config.write_all(b"\n\tpromisor = true\n").unwrap();
    drop(config);
    let unlisted = sys
        .run_complete_stdout(&git(&sys, &who, Some(&repo)).args([
            "config",
            "--get-regexp",
            r"^remote\..*\.promisor$",
        ]))
        .unwrap();
    if unlisted.status != Some(1) {
        aterm_uds::host::could_not_run(format!(
            "git under this account's locale lists the remote named in bytes that are not \
             UTF-8 ({}), so the C locale changes nothing here",
            unlisted.describe()
        ));
    }
    let refused = verify_integrity(&sys, &who, &repo).unwrap_err();
    assert!(refused.contains("partial"), "{refused}");
    // The control: git fetches through that remote what the repository lacks.
    let fetched = text(
        &sys,
        &git(&sys, &who, Some(&repo))
            .env("GIT_NO_LAZY_FETCH", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(crate::checkout::NO_SIDE_RUNS)
            .args(["cat-file", "-t", blob]),
        "a read that may fetch lazily",
    )
    .unwrap();
    assert_eq!(fetched, "blob\n");
}

#[test]
fn matching_real_dirty_worktree_on_an_alternate_branch_is_only_read() {
    crate::items::github::need_native_git();
    let sys = Live::new();
    let home = Scratch::new(&sys, "github-repo-fixture").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = home.path().to_string_lossy().into_owned();
    let path = home.path().join("existing");
    text(
        &sys,
        &git(&sys, &who, None)
            .args(["init", "--template=", "--initial-branch=main"])
            .arg(&path),
        "fixture init",
    )
    .unwrap();
    text(
        &sys,
        // A commit starts automatic maintenance too, whose fork would hold this drain's
        // pipe past git's exit on a loaded machine.
        &git(&sys, &who, Some(&path))
            .args(crate::checkout::NO_SIDE_RUNS)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ]),
        "fixture commit",
    )
    .unwrap();
    text(
        &sys,
        &git(&sys, &who, Some(&path)).args([
            "remote",
            "add",
            "origin",
            "git@github.com:alice/existing.git",
        ]),
        "fixture remote",
    )
    .unwrap();
    text(
        &sys,
        &git(&sys, &who, Some(&path)).args(["switch", "-c", "topic"]),
        "fixture branch",
    )
    .unwrap();
    sys.write_file(&path.join("dirty.txt"), b"local owner edits", 0o600)
        .unwrap();
    let repo = Repo {
        owner: "alice".into(),
        name: "existing".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    };
    verify_existing(&sys, &who, "alice", &repo, &path).unwrap();
    assert_eq!(
        sys.read_file(&path.join("dirty.txt")).unwrap(),
        b"local owner edits"
    );
    assert_eq!(
        git_read(&sys, &who, &path, &["symbolic-ref", "--quiet", "HEAD"])
            .unwrap()
            .trim(),
        "refs/heads/topic"
    );
}

/// A checkout in `~` that a `git clone` left unfinished (killed, or cut off by a power loss)
/// is no repository present, whoever made it. One cut off after it fetched and before it
/// wrote its files has a commit and no index (with its lock or without), and one whose
/// branch names a commit it never fetched has a ref git cannot read (its record's
/// `for-each-ref` fails, as git 2.56's does on a ref to an object it lacks): each is left
/// untouched under a note. One cut off before it fetched anything has an unborn HEAD, as an
/// empty repository's clone has: no note, and it is the unfinished-checkout path's
/// ([`Partition::unborn`]), which GitHub's advertisement decides ([`observe_unborn`]). The
/// record's ref read fetches nothing. Before, a commit with no index read present.
/// NEGATIVE CONTROL: a finished checkout, its commit and its index there, is present.
#[test]
fn a_checkout_a_clone_left_unfinished_is_not_present() {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Case {
        Finished,
        Unborn,
        CommitNeverFetched,
        CutInCheckout,
        CutBeforeCheckout,
    }
    for case in [
        Case::Finished,
        Case::Unborn,
        Case::CommitNeverFetched,
        Case::CutInCheckout,
        Case::CutBeforeCheckout,
    ] {
        let (sys, who) = fixture();
        let path = Path::new(&who.home).join("partial");
        sys.stat_of(path.to_str().unwrap(), directory(who.uid));
        script_worktree(&sys, &who, &path, "partial");
        let refs = crate::checkout::facts::tests::main_refs(&sys, &who, &path).display();
        match case {
            Case::Finished => {
                script_head(&sys, &who, &path, Some(A));
                script_index(&sys, &path);
            }
            Case::Unborn => script_head(&sys, &who, &path, None),
            Case::CommitNeverFetched => {
                // As git 2.56's `for-each-ref` says of a ref to an object it lacks.
                sys.on(
                    &refs,
                    Ran::exit(
                        128,
                        &format!("fatal: missing object {A} for refs/heads/main\n"),
                    ),
                );
            }
            Case::CutInCheckout => {
                script_head(&sys, &who, &path, Some(A));
                // The lock sits beside the index the worktree read named.
                let lock = fake_index(&path).with_file_name("index.lock");
                sys.file(lock.to_str().unwrap(), b"");
            }
            Case::CutBeforeCheckout => script_head(&sys, &who, &path, Some(A)),
        }
        let repo = Repo {
            owner: "alice".into(),
            name: "partial".into(),
            default_branch: Some("main".into()),
            size_kib: 0,
            fork: false,
            archived: false,
            required_submodules: Vec::new(),
        };
        let standing = partition(&sys, &who, std::slice::from_ref(&repo)).unwrap();
        let noted = standing.notes.iter().any(|note| note.contains("~/partial"));
        assert_eq!(
            (
                standing.present.len(),
                standing.unborn.len(),
                noted,
                standing.missing.len()
            ),
            (
                usize::from(case == Case::Finished),
                usize::from(case == Case::Unborn),
                matches!(
                    case,
                    Case::CommitNeverFetched | Case::CutInCheckout | Case::CutBeforeCheckout
                ),
                0
            ),
            "{case:?}: {:?}",
            standing.notes
        );
        assert!(
            sys.calls().contains(&refs) && refs.contains("GIT_NO_LAZY_FETCH=1 "),
            "{case:?}: the ref read fetches nothing: {:?}",
            sys.calls()
        );
    }
}

/// A DELIBERATELY SHALLOW OR PARTIAL CHECKOUT THAT MATCHES IS PRESERVED: its record says it is
/// shallow and a promisor remote's, and [`verify_existing`] admits it all the same, with no
/// `fsck` and no fetch; only a new clone's [`verify_integrity`] refuses those.
/// NEGATIVE CONTROL: [`verify_integrity`] over the same record refuses it.
#[test]
fn deliberately_shallow_or_partial_matching_existing_checkout_is_preserved_without_integrity_probes()
 {
    let (sys, who) = fixture();
    let path = Path::new(&who.home).join("existing");
    let index = fake_index(&path);
    sys.on(
        &crate::checkout::facts::rev_parse(&sys, &who, &path).display(),
        Ran::ok(&format!(
            "true\n\n{}\ntrue\n{}\n{}\n",
            path.display(),
            index.display(),
            index.with_file_name("objects/info/alternates").display()
        )),
    );
    sys.on(
        &crate::checkout::facts::config_listing(&sys, &who, &path).display(),
        Ran::ok(&crate::checkout::facts::tests::listing_of(&[
            ("remote.origin.url", "https://github.com/alice/existing.git"),
            ("remote.origin.promisor", "true"),
            ("remote.origin.partialclonefilter", "blob:none"),
        ])),
    );
    for (reference, target) in [
        ("HEAD", "refs/heads/main\n"),
        ("refs/remotes/origin/HEAD", "refs/remotes/origin/main\n"),
    ] {
        sys.on(
            &git(&sys, &who, Some(&path))
                .args(["symbolic-ref", "--quiet", reference])
                .display(),
            Ran::ok(target),
        );
    }
    script_head(&sys, &who, &path, Some(A));
    script_index(&sys, &path);
    let repo = Repo {
        owner: "alice".into(),
        name: "existing".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    };
    verify_existing(&sys, &who, "alice", &repo, &path).unwrap();
    assert!(
        verify_integrity(&sys, &who, &path).is_err(),
        "control: the same record is no new clone's"
    );
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains("fsck") && !call.contains(" fetch")),
        "{:?}",
        sys.calls()
    );
}

#[test]
fn dirty_new_checkout_or_incorrect_tracking_is_never_published() {
    for (args, bad) in [
        (
            vec!["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            " D missing.txt\0",
        ),
        (
            vec!["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            "refs/remotes/origin/other\n",
        ),
        (vec!["config", "--get", "branch.main.remote"], "other\n"),
        (
            vec!["config", "--get", "branch.main.merge"],
            "refs/heads/other\n",
        ),
    ] {
        let (sys, who) = fixture();
        let staged = Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
        let key = git(&sys, &who, Some(&staged))
            .args(args.iter().copied())
            .display();
        if args[0] == "symbolic-ref" {
            // Every read of origin's HEAD hears it, the clone's record's included, whose ref
            // read then asks for `origin/other`, which the clone does not hold.
            let refs = crate::checkout::facts::ref_listing(
                &sys,
                &who,
                &staged,
                &[
                    "refs/heads/main".to_owned(),
                    "refs/remotes/origin/other".to_owned(),
                ],
            )
            .display();
            sys.simulate(move |_, command| {
                let shown = command.display();
                if shown == key {
                    Some(Ran::ok(bad))
                } else if shown == refs {
                    Some(Ran::ok(&format!("{A} commit refs/heads/main\n")))
                } else {
                    None
                }
            });
        } else {
            sys.on(&key, Ran::ok(bad));
        }
        let (repo, _, _) = script_clone(&sys, &who, false);
        assert!(clone_one(&sys, &who, "alice", &repo).is_err());
        assert!(sys.stat(&Path::new(&who.home).join("new")).is_err());
        assert!(sys.stat(&staged).is_err());
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.starts_with("rename-noreplace "))
        );
    }
}

/// A PARTITION READS A CHECKOUT'S RECORD OF FACTS ONCE: the record [`verify_existing`] judges
/// (its top, its origin, whether HEAD names a commit and its index is there) is the one that
/// says whether the checkout is unborn ([`crate::checkout::Facts::unborn`]), so a checkout
/// costs a partition the record's reads alone, a finished one and one with no commit alike: on
/// this fake, which holds no HEAD file, five (the `rev-parse` batch, the config listing, HEAD's
/// and origin's HEAD's `symbolic-ref`, the refs), where the old reads took three for a finished
/// checkout and six for an unborn one.
/// NEGATIVE CONTROL: a `standing` that asks [`crate::checkout::unborn`] in place of the record
/// it judged reads the record twice: ten.
#[test]
fn a_partition_reads_a_checkouts_record_once() {
    for unborn in [false, true] {
        let (sys, who) = fixture();
        let path = Path::new(&who.home).join("existing");
        sys.stat_of(path.to_str().unwrap(), directory(who.uid));
        if unborn {
            script_existing_head(&sys, &who, &path, "existing", None);
        } else {
            script_existing(&sys, &who, &path, "existing");
        }
        let here = |args: &[&str]| git(&sys, &who, Some(&path)).args(args).display();
        let expected = vec![
            crate::checkout::facts::rev_parse(&sys, &who, &path).display(),
            crate::checkout::facts::config_listing(&sys, &who, &path).display(),
            here(&["symbolic-ref", "--quiet", "HEAD"]),
            here(&["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]),
            crate::checkout::facts::tests::main_refs(&sys, &who, &path).display(),
        ];
        let repo = Repo {
            owner: "alice".into(),
            name: "existing".into(),
            default_branch: Some("main".into()),
            size_kib: 0,
            fork: false,
            archived: false,
            required_submodules: Vec::new(),
        };
        let before = sys.calls().len();
        let standing = partition(&sys, &who, std::slice::from_ref(&repo)).unwrap();
        assert_eq!(
            (standing.present.len(), standing.unborn.len()),
            (usize::from(!unborn), usize::from(unborn)),
            "{unborn}: {:?}",
            standing.notes
        );
        let within = format!("-C {}", path.display());
        let ran: Vec<String> = sys.calls()[before..]
            .iter()
            .filter(|call| call.contains(&within))
            .cloned()
            .collect();
        assert_eq!(ran, expected, "{unborn}");
    }
}
