// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared alignment against real CLT git, in scratch repositories of a scratch home.

use super::*;
use crate::items::defaults::tests::user;
use crate::port::spawns::Spawns;
use crate::port::{Live, Scratch};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// The git subcommands no run of the code under test may be: each rewrites or publishes
/// something a person made.
pub(crate) const NEVER: [&str; 14] = [
    "reset",
    "stash",
    "rebase",
    "checkout",
    "switch",
    "restore",
    "clean",
    "pull",
    "push",
    "commit",
    "cherry-pick",
    "revert",
    "am",
    "branch",
];

fn subcommand(cmd: &Cmd) -> Option<String> {
    let mut args = cmd
        .args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned());
    while let Some(arg) = args.next() {
        if arg == "-C" || arg == "-c" {
            args.next();
            continue;
        }
        return Some(arg);
    }
    None
}

/// The runs in `record` that rewrite or publish: a [`NEVER`] subcommand, or a merge that is
/// not `--ff-only`.
pub(crate) fn rewrites(record: &[(Cmd, Option<Vec<u8>>)]) -> Vec<String> {
    record
        .iter()
        .filter(|(cmd, _)| {
            let ff_only = cmd.args.iter().any(|arg| arg == "--ff-only");
            subcommand(cmd)
                .is_some_and(|sub| NEVER.contains(&sub.as_str()) || (sub == "merge" && !ff_only))
        })
        .map(|(cmd, _)| cmd.display())
        .collect()
}

/// The git subcommands `record` ran, in order.
pub(crate) fn subcommands(record: &[(Cmd, Option<Vec<u8>>)]) -> Vec<String> {
    record
        .iter()
        .filter_map(|(cmd, _)| subcommand(cmd))
        .collect()
}

/// A bare origin, a seed clone that advances it, and `work`, a clone of it: all in one
/// scratch home whose `.gitconfig` names a fixture identity and allows file-protocol
/// submodules (this home's own; nothing of the real one is read).
pub(crate) struct Repos<'a> {
    pub(crate) live: &'a Live,
    _home: Scratch<'a>,
    pub(crate) who: User,
    pub(crate) origin: PathBuf,
    pub(crate) seed: PathBuf,
    pub(crate) work: PathBuf,
}

/// An aterm workspace manifest whose repository is `origin`.
pub(crate) fn manifest(origin: &Path) -> String {
    format!(
        "[workspace]\nmembers = []\n\n[workspace.package]\nrepository = \"{}\"\n\n\
         [workspace.metadata.aterm]\nupdate_channel = \"alabsystems/aterm\"\n",
        origin.display()
    )
}

impl<'a> Repos<'a> {
    pub(crate) fn new(live: &'a Live, label: &str) -> Self {
        crate::items::github::need_clt_git();
        let home = Scratch::new(live, label).unwrap();
        let mut who = user("fixture", live.euid());
        who.home = home.path().to_string_lossy().into_owned();
        std::fs::write(
            home.path().join(".gitconfig"),
            "[user]\n\tname = Fixture\n\temail = fixture@example.invalid\n[commit]\n\tgpgsign = \
             false\n[init]\n\tdefaultBranch = main\n[protocol \"file\"]\n\tallow = always\n",
        )
        .unwrap();
        let origin = home.path().join("origin.git");
        let seed = home.path().join("seed");
        let work = home.path().join("work");
        let repos = Self {
            live,
            _home: home,
            who,
            origin,
            seed,
            work,
        };
        repos.init(&repos.origin, true);
        repos.init(&repos.seed, false);
        repos.g(
            &repos.seed,
            &["remote", "add", "origin", &repos.origin.to_string_lossy()],
        );
        std::fs::write(repos.seed.join("Cargo.toml"), manifest(&repos.origin)).unwrap();
        std::fs::write(repos.seed.join("README"), "one\n").unwrap();
        repos.g(&repos.seed, &["add", "--all"]);
        repos.g(&repos.seed, &["commit", "--quiet", "-m", "one"]);
        repos.g(&repos.seed, &["push", "--quiet", "origin", "main"]);
        repos.clone_into(&repos.origin, &repos.work);
        repos
    }

    fn ok(&self, cmd: &Cmd) -> String {
        let ran = self.live.run(cmd).unwrap();
        assert!(ran.success(), "{}: {}", cmd.display(), ran.describe());
        ran.out().trim().to_owned()
    }

    /// A new repository at `dir` (bare when asked), with no template.
    pub(crate) fn init(&self, dir: &Path, bare: bool) {
        let mut cmd = git(self.live, &self.who, None).args(["init", "--quiet", "--template="]);
        if bare {
            cmd = cmd.arg("--bare");
        }
        self.ok(&cmd.arg("--initial-branch=main").arg(dir));
    }

    /// A clone of `from` at `to`, with no template.
    pub(crate) fn clone_into(&self, from: &Path, to: &Path) {
        self.ok(&git(self.live, &self.who, None)
            .args(["clone", "--quiet", "--template="])
            .arg(from)
            .arg(to));
    }

    /// One fixture git run in `dir` (never through the record under test).
    pub(crate) fn g(&self, dir: &Path, args: &[&str]) -> String {
        self.ok(&git(self.live, &self.who, Some(dir)).args(args.iter().copied()))
    }

    /// A new commit on origin's `main`, made in the seed clone: its id.
    pub(crate) fn advance(&self, file: &str, text: &str) -> String {
        std::fs::write(self.seed.join(file), text).unwrap();
        self.g(&self.seed, &["add", "--all"]);
        self.g(&self.seed, &["commit", "--quiet", "-m", file]);
        self.g(&self.seed, &["push", "--quiet", "origin", "main"]);
        self.head(&self.seed)
    }

    pub(crate) fn head(&self, dir: &Path) -> String {
        self.g(dir, &["rev-parse", "HEAD"])
    }

    /// What `git status` says of `dir`, untracked files included.
    pub(crate) fn status(&self, dir: &Path) -> String {
        self.g(dir, &["status", "--porcelain=v1", "--untracked-files=all"])
    }

    /// The origin `work` is admitted against: the scratch origin, exactly.
    pub(crate) fn admitted(&self) -> Origin {
        Origin(vec![Remote::Exact(
            self.origin.to_string_lossy().into_owned(),
        )])
    }
}

#[test]
fn a_clean_checkout_behind_its_origin_fast_forwards() {
    let live = Live::new();
    for fetched in [false, true] {
        let repos = Repos::new(&live, "checkout-behind");
        let tip = repos.advance("two.txt", "two\n");
        if fetched {
            repos.g(&repos.work, &["fetch", "--quiet", "origin"]);
        }
        let sys = Spawns::new(&live);
        let origin = repos.admitted();
        let before = read(&sys, &repos.who, &repos.work, &origin).unwrap();
        if fetched {
            assert_eq!(
                before,
                Stands::Behind {
                    branch: "main".into(),
                    tip: tip.clone(),
                    by: 1
                }
            );
        } else {
            assert_eq!(
                before,
                Stands::Unfetched {
                    branch: "main".into(),
                    tip: tip.clone()
                }
            );
        }
        assert!(matches!(before.probe(), Probe::Change { .. }), "{before:?}");
        let moved = align(&sys, &repos.who, &repos.work, &origin).unwrap();
        assert!(matches!(moved, Stands::Behind { by: 1, .. }), "{moved:?}");
        assert_eq!(repos.head(&repos.work), tip);
        assert_eq!(
            std::fs::read_to_string(repos.work.join("two.txt")).unwrap(),
            "two\n"
        );
        assert_eq!(repos.status(&repos.work), "");
        let after = read(&sys, &repos.who, &repos.work, &origin).unwrap();
        assert_eq!(
            after,
            Stands::Current {
                branch: "main".into(),
                head: tip.clone()
            }
        );
        let record = sys.record();
        let ran = subcommands(&record);
        assert!(ran.iter().any(|sub| sub == "fetch"), "{ran:?}");
        assert!(ran.iter().any(|sub| sub == "merge"), "{ran:?}");
        assert_eq!(rewrites(&record), Vec::<String>::new());
    }
    let repos = Repos::new(&live, "checkout-control");
    let control = Spawns::new(&live);
    let ran = control
        .run(&git(repos.live, &repos.who, Some(&repos.work)).args(["stash", "list"]))
        .unwrap();
    assert!(ran.success(), "{}", ran.describe());
    assert_eq!(
        rewrites(&control.record()).len(),
        1,
        "{:?}",
        control.record()
    );
}

#[test]
fn a_checkout_with_anything_of_its_own_is_left_as_it_is_with_its_reason() {
    type Setup = fn(&Repos<'_>);
    fn commit_here(r: &Repos<'_>) {
        std::fs::write(r.work.join("mine.txt"), "mine\n").unwrap();
        r.g(&r.work, &["add", "mine.txt"]);
        r.g(&r.work, &["commit", "--quiet", "-m", "mine"]);
    }
    let cases: [(&str, Setup, &str, bool); 10] = [
        (
            "dirty",
            |r| {
                r.advance("two.txt", "two\n");
                std::fs::write(r.work.join("README"), "edited here\n").unwrap();
            },
            "it has uncommitted changes or untracked files",
            false,
        ),
        (
            "untracked",
            |r| {
                r.advance("two.txt", "two\n");
                std::fs::write(r.work.join("notes.txt"), "mine\n").unwrap();
            },
            "it has uncommitted changes or untracked files",
            false,
        ),
        (
            "ahead",
            commit_here,
            "it has 1 commit that origin/main lacks",
            true,
        ),
        (
            "diverged",
            |r| {
                commit_here(r);
                r.advance("two.txt", "two\n");
                r.g(&r.work, &["fetch", "--quiet", "origin"]);
            },
            "it has diverged from origin/main (1 commit here, 1 commit there)",
            true,
        ),
        (
            "ahead of a moved origin",
            |r| {
                commit_here(r);
                r.advance("two.txt", "two\n");
            },
            "it has 1 commit that origin lacks (as last fetched)",
            true,
        ),
        (
            "detached",
            |r| {
                r.advance("two.txt", "two\n");
                r.g(&r.work, &["checkout", "--quiet", "--detach"]);
            },
            "HEAD is detached",
            false,
        ),
        (
            "another branch",
            |r| {
                r.advance("two.txt", "two\n");
                r.g(&r.work, &["switch", "--quiet", "-c", "topic"]);
            },
            "it is on topic, not origin's default branch main",
            true,
        ),
        (
            "foreign origin",
            |r| {
                r.advance("two.txt", "two\n");
                r.g(
                    &r.work,
                    &["remote", "set-url", "origin", "/elsewhere/origin.git"],
                );
            },
            "its origin is not ",
            false,
        ),
        (
            "an origin with two URLs",
            |r| {
                r.advance("two.txt", "two\n");
                r.g(
                    &r.work,
                    &[
                        "config",
                        "--add",
                        "remote.origin.url",
                        &r.origin.to_string_lossy(),
                    ],
                );
            },
            "origin has more than one URL",
            false,
        ),
        (
            "rewritten origin",
            |r| {
                r.advance("two.txt", "two\n");
                r.g(
                    &r.work,
                    &[
                        "config",
                        "url./elsewhere/.insteadOf",
                        &r.origin.to_string_lossy(),
                    ],
                );
            },
            "git rewrites its origin to a remote that is not ",
            false,
        ),
    ];
    let live = Live::new();
    for (case, setup, why, reaches_origin) in cases {
        let repos = Repos::new(&live, "checkout-kept");
        setup(&repos);
        let head = repos.head(&repos.work);
        let branch = repos.g(&repos.work, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let status = repos.status(&repos.work);
        let sys = Spawns::new(&live);
        let origin = repos.admitted();
        let stands = read(&sys, &repos.who, &repos.work, &origin).unwrap();
        let Stands::Kept(said) = &stands else {
            panic!("{case}: {stands:?}");
        };
        assert!(said.starts_with(why), "{case}: {said}");
        match stands.probe() {
            Probe::Ok { now } => assert_eq!(now, format!("left as it is: {said}"), "{case}"),
            other => panic!("{case}: {other:?}"),
        }
        assert_eq!(
            align(&sys, &repos.who, &repos.work, &origin).unwrap(),
            stands,
            "{case}"
        );
        assert_eq!(repos.head(&repos.work), head, "{case}");
        assert_eq!(
            repos.g(&repos.work, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch,
            "{case}"
        );
        assert_eq!(repos.status(&repos.work), status, "{case}");
        let record = sys.record();
        assert_eq!(rewrites(&record), Vec::<String>::new(), "{case}");
        let ran = subcommands(&record);
        assert!(
            !ran.iter().any(|sub| sub == "fetch" || sub == "merge"),
            "{case}: {ran:?}"
        );
        let reached = record
            .iter()
            .any(|(cmd, _)| cmd.args.iter().any(|arg| arg == "--symref"));
        assert_eq!(reached, reaches_origin, "{case}: {ran:?}");
    }
}

#[test]
fn a_github_origin_is_matched_over_https_and_ssh_and_never_with_a_credential() {
    let aterm = Remote::of("https://github.com/alabsystems/aterm");
    assert_eq!(
        aterm,
        Remote::Github {
            owner: "alabsystems".into(),
            name: "aterm".into()
        }
    );
    for url in [
        "https://github.com/alabsystems/aterm.git",
        "git@github.com:alabsystems/aterm.git",
        "ssh://git@github.com/ALabSystems/aterm",
        "ssh://git@github.com:22/alabsystems/aterm.git",
    ] {
        assert!(aterm.admits(url), "{url}");
    }
    for url in [
        "https://token@github.com/alabsystems/aterm.git",
        "https://github.com/alabsystems/aterm-staging.git",
        "https://mirror.invalid/alabsystems/aterm.git",
    ] {
        assert!(!aterm.admits(url), "{url}");
    }
    assert_eq!(
        Remote::of("/srv/git/aterm.git/"),
        Remote::Exact("/srv/git/aterm.git".into())
    );
    assert_eq!(
        Origin(vec![aterm, Remote::of("/srv/git/aterm.git")]).describe(),
        "github.com/alabsystems/aterm or /srv/git/aterm.git"
    );
}

#[test]
fn align_runs_no_hook_and_fetches_from_origin_alone() {
    let live = Live::new();
    let mut markers = Vec::new();
    for by_init in [true, false] {
        let repos = Repos::new(&live, "checkout-hooks");
        let home = PathBuf::from(&repos.who.home);
        let astream = home.join("astream.git");
        let astream_seed = home.join("astream-seed");
        repos.init(&astream, true);
        repos.init(&astream_seed, false);
        std::fs::write(astream_seed.join("lib.rs"), "one\n").unwrap();
        repos.g(&astream_seed, &["add", "--all"]);
        repos.g(&astream_seed, &["commit", "--quiet", "-m", "one"]);
        repos.g(
            &astream_seed,
            &["push", "--quiet", &astream.to_string_lossy(), "HEAD:main"],
        );
        repos.g(
            &repos.seed,
            &[
                "submodule",
                "add",
                "--quiet",
                &astream.to_string_lossy(),
                "vendor/astream",
            ],
        );
        repos.g(&repos.seed, &["commit", "--quiet", "-m", "astream"]);
        repos.g(&repos.seed, &["push", "--quiet", "origin", "main"]);
        let work = home.join("hooked");
        repos.clone_into(&repos.origin, &work);
        repos.g(&work, &["submodule", "update", "--init", "--quiet"]);
        repos.g(&work, &["config", "submodule.recurse", "true"]);
        repos.g(&work, &["config", "core.hooksPath", ".githooks"]);
        let marker = home.join(format!("hooks-ran-{by_init}"));
        let hook = format!("#!/bin/sh\necho \"$0\" >> '{}'\n", marker.display());
        std::fs::write(astream_seed.join("lib.rs"), "two\n").unwrap();
        repos.g(&astream_seed, &["commit", "--quiet", "--all", "-m", "two"]);
        repos.g(
            &astream_seed,
            &["push", "--quiet", &astream.to_string_lossy(), "HEAD:main"],
        );
        let two = repos.head(&astream_seed);
        let inner = repos.seed.join("vendor/astream");
        repos.g(&inner, &["fetch", "--quiet", "origin"]);
        repos.g(&inner, &["checkout", "--quiet", "--detach", &two]);
        std::fs::create_dir_all(repos.seed.join(".githooks")).unwrap();
        for name in ["post-merge", "reference-transaction"] {
            let path = repos.seed.join(".githooks").join(name);
            std::fs::write(&path, &hook).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        repos.g(&repos.seed, &["add", "--all"]);
        repos.g(&repos.seed, &["commit", "--quiet", "-m", "hook and bump"]);
        repos.g(&repos.seed, &["push", "--quiet", "origin", "main"]);
        let tip = repos.head(&repos.seed);
        let origin = Origin(vec![Remote::Exact(
            repos.origin.to_string_lossy().into_owned(),
        )]);
        if by_init {
            let sys = Spawns::new(&live);
            let moved = align(&sys, &repos.who, &work, &origin).unwrap();
            assert!(matches!(moved, Stands::Behind { by: 1, .. }), "{moved:?}");
            assert_eq!(rewrites(&sys.record()), Vec::<String>::new());
        } else {
            repos.g(&work, &["fetch", "--quiet", "origin"]);
            repos.g(&work, &["merge", "--ff-only", "--quiet", "origin/main"]);
        }
        assert_eq!(repos.head(&work), tip, "by_init {by_init}");
        let fetched_here = live
            .run(
                &git(repos.live, &repos.who, Some(&work.join("vendor/astream"))).args([
                    "cat-file",
                    "-e",
                    &format!("{two}^{{commit}}"),
                ]),
            )
            .unwrap()
            .success();
        let ran = std::fs::read_to_string(&marker).unwrap_or_default();
        markers.push((
            by_init,
            fetched_here,
            ran.contains("post-merge"),
            ran.contains("reference-transaction"),
        ));
    }
    assert_eq!(
        markers,
        [(true, false, false, false), (false, true, true, true)],
        "(by init, submodule fetched, post-merge ran, reference-transaction ran)"
    );
}

#[test]
fn an_unreachable_origin_reads_offline_and_a_refusing_one_does_not() {
    let live = Live::new();
    for (url, offline) in [
        ("http://127.0.0.1:1/origin.git", true),
        ("/nonexistent/aterm-fixture/origin.git", false),
    ] {
        let repos = Repos::new(&live, "checkout-offline");
        repos.g(&repos.work, &["remote", "set-url", "origin", url]);
        let origin = Origin(vec![Remote::Exact(url.to_owned())]);
        let sys = Spawns::new(&live);
        let unread = read(&sys, &repos.who, &repos.work, &origin).unwrap_err();
        assert_eq!(unread.offline, offline, "{url}: {unread:?}");
        assert!(!unread.why.contains(url), "{unread:?}");
        let unread = align(&sys, &repos.who, &repos.work, &origin).unwrap_err();
        assert_eq!(unread.offline, offline, "{url}: {unread:?}");
        let ran = subcommands(&sys.record());
        assert!(
            !ran.iter().any(|sub| sub == "fetch" || sub == "merge"),
            "{ran:?}"
        );
    }
    assert!(unreachable(
        b"ssh: Could not resolve hostname github.com: nodename nor servname provided"
    ));
    assert!(!unreachable(
        b"git@github.com: Permission denied (publickey).\nfatal: Could not read from remote"
    ));
}
