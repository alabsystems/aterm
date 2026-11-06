// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::user;
use crate::port::{Fake, Ran, Stat};

pub(in crate::init_github) const PIN: &str = "1111111111111111111111111111111111111111";
const OLD: &str = "2222222222222222222222222222222222222222";
const PARENT: &str = "3333333333333333333333333333333333333333";
const MODULES: &str = "3333333333333333333333333333333333333333:.gitmodules";

pub(in crate::init_github) fn directory(who: &User) -> Stat {
    Stat {
        uid: who.uid,
        gid: who.gid,
        mode: 0o700,
        dir: true,
        symlink: false,
        len: 0,
    }
}

/// Tracked metadata, independently scripted from the selected worktrees. The fixture
/// includes an optional entry so acquiring everything in .gitmodules is observable.
pub(in crate::init_github) fn declarations(sys: &Fake, who: &User, parent: &Path, paths: &[&str]) {
    let git = || local(sys, who, Some(parent));
    sys.on(
        &git()
            .args(["rev-parse", "--verify", "HEAD^{commit}"])
            .display(),
        Ran::ok(PARENT),
    );
    sys.on(
        &git()
            .args(["ls-tree", "-z", PARENT, "--", ".gitmodules"])
            .display(),
        Ran::ok(&format!("100644 blob {OLD}\t.gitmodules\0")),
    );
    sys.on(
        &git()
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--quiet",
                PARENT,
                "--",
                ".gitmodules",
            ])
            .display(),
        Ran::ok(""),
    );
    let mut declarations = "submodule.optional.path\noptional/huge\0".to_owned();
    for path in paths {
        sys.on(
            &git()
                .args(checkout::NO_SIDE_RUNS)
                .args([
                    "-c",
                    "submodule.recurse=false",
                    "submodule",
                    "init",
                    "--",
                    path,
                ])
                .display(),
            Ran::ok(""),
        );
        declarations.push_str(&format!("submodule.{path}.path\n{path}\0"));
        sys.on(
            &git().args(["ls-tree", "-z", PARENT, "--", path]).display(),
            Ran::ok(&format!("160000 commit {PIN}\t{path}\0")),
        );
        sys.on(
            &git()
                .args(["ls-files", "--stage", "-z", "--", path])
                .display(),
            Ran::ok(&format!("160000 {PIN} 0\t{path}\0")),
        );
        let name = path.rsplit('/').next().unwrap();
        sys.on(
            &git()
                .args([
                    "config",
                    "--blob",
                    MODULES,
                    "--no-includes",
                    "--null",
                    "--get-all",
                    &format!("submodule.{path}.url"),
                ])
                .display(),
            Ran::ok(&format!("https://github.com/alice/{name}.git\0")),
        );
    }
    sys.on(
        &git()
            .args([
                "config",
                "--blob",
                MODULES,
                "--no-includes",
                "--null",
                "--get-regexp",
                "^submodule\\..*\\.path$",
            ])
            .display(),
        Ran::ok(&declarations),
    );
}

fn existing(sys: &Fake, who: &User, path: &Path, name: &str, pin: &str) {
    existing_as(sys, who, path, "alice", name, pin);
}

fn existing_as(sys: &Fake, who: &User, path: &Path, owner: &str, name: &str, pin: &str) {
    let git = || local(sys, who, Some(path));
    let ordinary = || checkout::git(sys, who, Some(path));
    sys.on(
        &ordinary()
            .args([
                "rev-parse",
                "--is-inside-work-tree",
                "--show-prefix",
                "--show-toplevel",
            ])
            .display(),
        Ran::ok(&format!("true\n\n{}\n", path.display())),
    );
    sys.on(
        &ordinary()
            .args(["config", "--get-all", "remote.origin.url"])
            .display(),
        Ran::ok(&format!("https://github.com/{owner}/{name}.git\n")),
    );
    sys.on(
        &ordinary()
            .args(["rev-parse", "--is-shallow-repository"])
            .display(),
        Ran::ok("false\n"),
    );
    sys.on(
        &ordinary()
            .args([
                "config",
                "--get-regexp",
                "^(extensions\\.partialclone|remote\\..*\\.(promisor|partialclonefilter))$",
            ])
            .display(),
        Ran::exit(1, ""),
    );
    sys.on(
        &git().args(["rev-parse", "--show-toplevel"]).display(),
        Ran::ok(&path.to_string_lossy()),
    );
    sys.on(
        &git()
            .args(["rev-parse", "--verify", "HEAD^{commit}"])
            .display(),
        Ran::ok(pin),
    );
    sys.on(
        &git()
            .args(["rev-parse", "--git-path", "objects/info/alternates"])
            .display(),
        Ran::ok(".git/objects/info/alternates\n"),
    );
}

/// A selected dependency's staged download, with ready reads for its later published path.
/// No actual GitHub traffic is involved; filesystem publication is the real System port.
pub(in crate::init_github) fn download(
    sys: &Fake,
    who: &User,
    parent: &Path,
    path: &str,
    sequence: u64,
) -> String {
    download_as(sys, who, parent, path, "alice", sequence)
}

fn download_as(
    sys: &Fake,
    who: &User,
    parent: &Path,
    path: &str,
    owner: &str,
    sequence: u64,
) -> String {
    let name = path.rsplit('/').next().unwrap();
    let dependency = Dependency {
        path: path.into(),
        pin: PIN.into(),
        owner: owner.into(),
        name: name.into(),
        declared_url: format!("https://github.com/{owner}/{name}.git"),
    };
    let ordinary = local(sys, who, None).args(checkout::NO_SIDE_RUNS).args([
        "-c",
        "submodule.recurse=false",
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "protocol.ssh.allow=always",
        "-c",
        "http.followRedirects=false",
    ]);
    sys.on(
        &ordinary
            .args(["ls-remote", "--get-url", "--", &dependency.url()])
            .display(),
        Ran::ok(&dependency.url()),
    );
    let stage =
        Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", sequence).join("repo");
    let command = network(sys, who, None, &dependency)
        .unwrap()
        .budget(BUDGET)
        .args([
            "clone",
            "--no-checkout",
            "--no-recurse-submodules",
            "--no-single-branch",
            "--tags",
            "--origin",
            "origin",
            "--template=",
            "--",
            &dependency.url(),
        ])
        .arg(&stage);
    let key = command.display();
    sys.on(&key, Ran::ok(""));
    sys.on_stat(&key, &stage, directory(who));
    sys.on_stat(&key, &stage.join(".git"), directory(who));
    sys.on_write(
        &key,
        &stage.join("fixture.rs"),
        b"selected dependency contents",
    );
    sys.on_write(
        &key,
        &stage.join(".git/fixture"),
        b"complete object database",
    );
    existing_as(sys, who, &stage, owner, name, PIN);
    existing_as(sys, who, &parent.join(path), owner, name, PIN);
    sys.on(
        &local(sys, who, Some(&stage))
            .args(["cat-file", "-e", &format!("{PIN}^{{commit}}")])
            .display(),
        Ran::ok(""),
    );
    sys.on(
        &local(sys, who, Some(&stage))
            .args(checkout::NO_SIDE_RUNS)
            .args([
                "-c",
                "submodule.recurse=false",
                "checkout",
                "--detach",
                "--no-recurse-submodules",
                PIN,
            ])
            .display(),
        Ran::ok(""),
    );
    key
}

fn fixture(paths: &[&str]) -> (Fake, User, Repo, PathBuf) {
    let mut sys = Fake::root();
    sys.euid = 501;
    let who = user("alice", 501);
    let parent = Path::new(&who.home).join("parent");
    sys.file(crate::items::github::CLT_GIT, b"fixture git");
    sys.stat_of(&who.home, directory(&who));
    sys.stat_of(parent.to_str().unwrap(), directory(&who));
    repos::tests::script_existing(&sys, &who, &parent, "parent");
    declarations(&sys, &who, &parent, paths);
    let repo = Repo {
        owner: "alice".into(),
        name: "parent".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: paths.iter().map(|path| (*path).into()).collect(),
    };
    (sys, who, repo, parent)
}

#[test]
fn policy_accepts_only_canonical_nonoverlapping_relative_paths() {
    assert!(paths(None).unwrap().is_empty());
    assert_eq!(
        paths(Some(&aterm_json::json!(["vendor/library"]))).unwrap(),
        ["vendor/library"]
    );
    for value in [
        aterm_json::json!(["/absolute"]),
        aterm_json::json!(["../outside"]),
        aterm_json::json!(["vendor/../outside"]),
        aterm_json::json!(["vendor//library"]),
        aterm_json::json!(["vendor\\library"]),
        aterm_json::json!(["vendor/.GIT/objects"]),
        aterm_json::json!(["vendor/library", "Vendor/Library"]),
        aterm_json::json!(["vendor", "vendor/library"]),
        aterm_json::json!([false]),
        aterm_json::json!(null),
    ] {
        assert!(paths(Some(&value)).is_err(), "{value}");
    }
}

#[test]
fn real_tracked_metadata_and_a_branch_at_the_pin_are_ready_without_moving_user_work() {
    use crate::port::Live;
    crate::items::github::need_clt_git();
    let sys = Live::new();
    let scratch = Scratch::new(&sys, "required-gitlink").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = scratch.path().to_string_lossy().into_owned();
    let parent = scratch.path().join("parent");
    let child = parent.join("vendor/library");
    sys.create_dir_all(&child, 0o700).unwrap();
    let run = |path: &Path, args: &[&str]| {
        let command = local(&sys, &who, Some(path))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(checkout::NO_SIDE_RUNS)
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args.iter().copied());
        text(&sys, command, "synthetic local git fixture failed").unwrap()
    };
    run(
        &child,
        &["init", "--template=", "--initial-branch=owner-work"],
    );
    sys.write_file(&child.join("library.rs"), b"required source\n", 0o600)
        .unwrap();
    run(&child, &["add", "--", "library.rs"]);
    run(&child, &["commit", "-m", "fixture dependency"]);
    run(
        &child,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/alice/library.git",
        ],
    );
    let pin = run(&child, &["rev-parse", "HEAD"]).trim().to_owned();
    run(&parent, &["init", "--template=", "--initial-branch=main"]);
    run(
        &parent,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/alice/parent.git",
        ],
    );
    sys.write_file(&parent.join(".gitmodules"), b"[submodule \"vendor/library\"]\npath = vendor/library\nurl = https://github.com/alice/library.git\nbranch = elsewhere\n", 0o600).unwrap();
    run(&parent, &["add", "--", ".gitmodules"]);
    run(
        &parent,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{pin},vendor/library"),
        ],
    );
    run(&parent, &["commit", "-m", "fixture parent"]);
    let repo = Repo {
        owner: "alice".into(),
        name: "parent".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: vec!["vendor/library".into()],
    };
    assert!(ready(&sys, &who, &parent, &repo).unwrap());
    repair(&sys, &who, &parent, &repo, &|| Ok(())).unwrap();
    assert_eq!(
        run(&parent, &["submodule", "status", "--", "vendor/library"])
            .chars()
            .next(),
        Some(' ')
    );
    assert_eq!(
        run(&child, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "owner-work"
    );
    // A different parent pin cannot detach or move the person's named branch.
    run(
        &parent,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{OLD},vendor/library"),
        ],
    );
    run(&parent, &["commit", "-m", "fixture later pin"]);
    assert!(
        ready(&sys, &who, &parent, &repo)
            .unwrap_err()
            .contains("branch")
    );
    assert!(repair(&sys, &who, &parent, &repo, &|| Ok(())).is_err());
    assert_eq!(run(&child, &["rev-parse", "HEAD"]).trim(), pin);
}

#[test]
fn a_required_gitlink_is_published_at_its_pin_without_optional_recursion() {
    let (sys, who, repo, parent) = fixture(&["vendor/library"]);
    download(&sys, &who, &parent, "vendor/library", 0);
    assert!(!ready(&sys, &who, &parent, &repo).unwrap());
    repair(&sys, &who, &parent, &repo, &|| Ok(())).unwrap();
    assert!(ready(&sys, &who, &parent, &repo).unwrap());
    assert_eq!(
        sys.read_file(&parent.join("vendor/library/fixture.rs"))
            .unwrap(),
        b"selected dependency contents"
    );
    assert!(sys.stat(&parent.join("optional/huge")).is_err());
    assert_eq!(sys.guarded_renames(), [parent.join("vendor/library")]);
    let calls = sys.calls();
    assert_eq!(
        calls.iter().filter(|call| call.contains(" clone ")).count(),
        1
    );
    assert!(calls.iter().all(|call| !call.contains("--recursive")
        && !call.contains("--remote")
        && !call.contains("--force")
        && !call.contains("optional/huge")
        && !call.contains("install-git-hooks")));
    let network = calls.iter().find(|call| call.contains(" clone ")).unwrap();
    assert!(
        network.contains("protocol.allow=never")
            && network.contains("core.hooksPath=/dev/null")
            && network.contains("credential.helper= ")
            && network.contains("--no-checkout"),
        "{network}"
    );
}

#[test]
fn relative_urls_acquire_only_the_admitted_github_sibling_or_owner() {
    for (relative, owner) in [
        ("../library.git", "alice"),
        ("../../team/library.git", "team"),
    ] {
        let (sys, who, repo, parent) = fixture(&["vendor/library"]);
        download_as(&sys, &who, &parent, "vendor/library", owner, 0);
        sys.simulate(move |_, command| {
            command
                .display()
                .contains("--get-all submodule.vendor/library.url")
                .then(|| Ran::ok(&format!("{relative}\0")))
        });
        repair(&sys, &who, &parent, &repo, &|| Ok(())).unwrap();
        assert!(ready(&sys, &who, &parent, &repo).unwrap());
        let calls = sys.calls();
        let clone = calls.iter().find(|call| call.contains(" clone ")).unwrap();
        assert!(
            clone.contains(&format!("https://github.com/{owner}/library.git")),
            "{clone}"
        );
    }
    let (_, _, repo, _) = fixture(&[]);
    for invalid in [
        "../../../elsewhere/repo.git",
        "../../../../github.com/alice/repo.git",
        "//elsewhere/repo.git",
        "../https://elsewhere/repo.git",
        "../%2e%2e/repo.git",
        "../../alice/repo.git/extra",
        "../repo.git?url=bad",
        "file:///personal/repository",
        "ext::command",
        "https://token@github.com/alice/repo.git",
        "../\\elsewhere/repo.git",
        "../../alice//repo.git",
    ] {
        assert!(dependency_identity(invalid, &repo).is_err(), "{invalid}");
    }
}

#[test]
fn a_parent_or_intermediate_symlink_is_never_traversed_for_dependencies() {
    for linked_parent in [false, true] {
        let (sys, who, repo, parent) = fixture(&["vendor/library"]);
        let path = if linked_parent {
            parent.clone()
        } else {
            parent.join("vendor")
        };
        sys.stat_of(
            path.to_str().unwrap(),
            Stat {
                symlink: true,
                ..directory(&who)
            },
        );
        assert!(ready(&sys, &who, &parent, &repo).is_err());
        assert!(repair(&sys, &who, &parent, &repo, &|| Ok(())).is_err());
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(" clone ") && !call.starts_with("rename-noreplace "))
        );
    }
}

#[test]
fn stop_or_source_changes_during_dependency_download_publish_nothing_and_end_the_batch() {
    for changed in [false, true] {
        let (sys, who, repo, parent) = fixture(&["vendor/first", "vendor/second"]);
        download(&sys, &who, &parent, "vendor/first", 0);
        download(&sys, &who, &parent, "vendor/second", 1);
        let current = || {
            if changed && sys.calls().iter().any(|call| call.contains(" clone ")) {
                Err("late Stop or changed credential snapshot".into())
            } else {
                Ok(())
            }
        };
        let result = repair(&sys, &who, &parent, &repo, &current);
        assert_eq!(result.is_ok(), !changed, "{result:?}");
        assert_eq!(sys.stat(&parent.join("vendor/first")).is_ok(), !changed);
        assert_eq!(sys.stat(&parent.join("vendor/second")).is_ok(), !changed);
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.contains(" clone "))
                .count(),
            if changed { 1 } else { 2 }
        );
    }
}

#[test]
fn dirty_branched_partial_unknown_and_linked_dependencies_stay_unfinished_and_untouched() {
    for mode in ["dirty", "branch", "partial", "unknown", "linked", "foreign"] {
        let (sys, who, repo, parent) = fixture(&["vendor/library"]);
        let path = parent.join("vendor/library");
        sys.stat_of(parent.join("vendor").to_str().unwrap(), directory(&who));
        sys.stat_of(path.to_str().unwrap(), directory(&who));
        sys.stat_of(path.join(".git").to_str().unwrap(), directory(&who));
        sys.file(path.join("work.rs").to_str().unwrap(), b"owner work");
        existing(&sys, &who, &path, "library", OLD);
        if mode == "partial" {
            sys.simulate(|_, command| {
                (command.display().contains("config --get-regexp")
                    && command.display().contains("partialclone"))
                .then(|| Ran::ok("remote.origin.promisor true\n"))
            });
        }
        let git = || local(&sys, &who, Some(&path));
        sys.on(
            &git()
                .args([
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                    "--ignore-submodules=all",
                ])
                .display(),
            Ran::ok(if mode == "dirty" { "?? work.rs\0" } else { "" }),
        );
        sys.on(
            &git().args(["symbolic-ref", "--quiet", "HEAD"]).display(),
            if mode == "branch" {
                Ran::ok("refs/heads/owner-work")
            } else {
                Ran::exit(1, "")
            },
        );
        sys.on(
            &local(&sys, &who, Some(&parent))
                .args([
                    "log",
                    "-1",
                    "--format=%H",
                    &format!("--find-object={OLD}"),
                    "HEAD",
                    "--",
                    "vendor/library",
                ])
                .display(),
            Ran::ok(""),
        );
        if mode == "linked" {
            sys.link(path.join(".git").to_str().unwrap(), "/personal/.git");
            sys.stat_of(
                path.join(".git").to_str().unwrap(),
                Stat {
                    symlink: true,
                    ..directory(&who)
                },
            );
        }
        if mode == "foreign" {
            sys.stat_of(
                path.to_str().unwrap(),
                Stat {
                    uid: 999,
                    ..directory(&who)
                },
            );
        }
        assert!(ready(&sys, &who, &parent, &repo).is_err(), "{mode}");
        assert!(
            repair(&sys, &who, &parent, &repo, &|| Ok(())).is_err(),
            "{mode}"
        );
        assert_eq!(sys.read_file(&path.join("work.rs")).unwrap(), b"owner work");
        assert!(
            sys.calls().iter().all(|call| !call.contains(" clone ")
                && !call.contains(" fetch ")
                && !call.contains(" checkout ")
                && !call.starts_with("rename-noreplace ")),
            "{mode}"
        );
    }
}

#[test]
fn an_existing_clean_detached_recorded_pin_is_fetched_then_aligned_without_recursion() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (sys, who, repo, parent) = fixture(&["vendor/library"]);
    let path = parent.join("vendor/library");
    sys.stat_of(parent.join("vendor").to_str().unwrap(), directory(&who));
    sys.stat_of(path.to_str().unwrap(), directory(&who));
    sys.stat_of(path.join(".git").to_str().unwrap(), directory(&who));
    sys.file(
        path.join("library.rs").to_str().unwrap(),
        b"recorded source",
    );
    existing(&sys, &who, &path, "library", OLD);
    let git = || local(&sys, &who, Some(&path));
    sys.on(
        &git()
            .args([
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=all",
            ])
            .display(),
        Ran::ok(""),
    );
    sys.on(
        &git().args(["symbolic-ref", "--quiet", "HEAD"]).display(),
        Ran::exit(1, ""),
    );
    sys.on(
        &local(&sys, &who, Some(&parent))
            .args([
                "log",
                "-1",
                "--format=%H",
                &format!("--find-object={OLD}"),
                "HEAD",
                "--",
                "vendor/library",
            ])
            .display(),
        Ran::ok(PARENT),
    );
    let moved = AtomicBool::new(false);
    let inside = path.clone();
    sys.simulate(move |_, command| {
        let call = command.display();
        if call.contains("ls-remote --get-url -- https://github.com/alice/library.git") {
            return Some(Ran::ok("https://github.com/alice/library.git"));
        }
        if call.contains(" fetch ") {
            return Some(Ran::ok(""));
        }
        if call.contains(" checkout ") {
            moved.store(true, Ordering::SeqCst);
            return Some(Ran::ok(""));
        }
        if command
            .args
            .windows(2)
            .any(|args| args[0] == "-C" && Path::new(&args[1]) == inside)
            && command.args.windows(3).any(|args| {
                args[0] == "rev-parse" && args[1] == "--verify" && args[2] == "HEAD^{commit}"
            })
        {
            return Some(Ran::ok(if moved.load(Ordering::SeqCst) {
                PIN
            } else {
                OLD
            }));
        }
        None
    });
    assert!(!ready(&sys, &who, &parent, &repo).unwrap());
    repair(&sys, &who, &parent, &repo, &|| Ok(())).unwrap();
    assert!(ready(&sys, &who, &parent, &repo).unwrap());
    let calls = sys.calls();
    let fetch = calls.iter().find(|call| call.contains(" fetch ")).unwrap();
    assert!(
        fetch.contains("credential.helper= ")
            && fetch.contains("protocol.allow=never")
            && fetch.contains("--no-recurse-submodules"),
        "{fetch}"
    );
    let checkout = calls
        .iter()
        .find(|call| call.contains(" checkout "))
        .unwrap();
    assert!(
        checkout.contains(PIN)
            && checkout.contains("--detach --no-recurse-submodules")
            && checkout.contains("core.hooksPath=/dev/null"),
        "{checkout}"
    );
    assert!(sys.guarded_renames().is_empty());
}

#[test]
fn a_destination_file_arriving_during_download_is_preserved() {
    let (sys, who, repo, parent) = fixture(&["vendor/library"]);
    let download = download(&sys, &who, &parent, "vendor/library", 0);
    let destination = parent.join("vendor/library");
    sys.on_write(&download, &destination, b"late owner work");
    assert!(repair(&sys, &who, &parent, &repo, &|| Ok(())).is_err());
    assert_eq!(sys.read_file(&destination).unwrap(), b"late owner work");
    assert!(sys.guarded_renames().is_empty());
}

#[test]
fn changed_gitlinks_urls_index_and_off_host_routes_refuse_publication() {
    for mode in ["pin", "url", "index", "route"] {
        let (sys, who, repo, parent) = fixture(&["vendor/library"]);
        download(&sys, &who, &parent, "vendor/library", 0);
        let changed = std::sync::atomic::AtomicBool::new(false);
        let current = || {
            if sys.calls().iter().any(|call| call.contains(" clone "))
                && !changed.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                let git = local(&sys, &who, Some(&parent));
                match mode {
                    "pin" => {
                        sys.on(
                            &git.args(["ls-tree", "-z", PARENT, "--", "vendor/library"])
                                .display(),
                            Ran::ok(&format!("160000 commit {OLD}\tvendor/library\0")),
                        );
                    }
                    "url" => {
                        sys.on(
                            &git.args([
                                "config",
                                "--blob",
                                MODULES,
                                "--no-includes",
                                "--null",
                                "--get-all",
                                "submodule.vendor/library.url",
                            ])
                            .display(),
                            Ran::ok("https://github.com/alice/other.git\0"),
                        );
                    }
                    "index" => {
                        sys.on(
                            &git.args(["ls-files", "--stage", "-z", "--", "vendor/library"])
                                .display(),
                            Ran::ok(&format!("160000 {OLD} 0\tvendor/library\0")),
                        );
                    }
                    _ => {}
                }
            }
            Ok(())
        };
        if mode == "route" {
            // The existing route fixture is consumed once by setup; this last response
            // becomes the network step's admission and cannot be bypassed by a local URL.
            let dependency = resolve(&sys, &who, &parent, &repo, "vendor/library").unwrap();
            let ordinary = local(&sys, &who, None).args(checkout::NO_SIDE_RUNS).args([
                "-c",
                "submodule.recurse=false",
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
                "-c",
                "protocol.ssh.allow=always",
                "-c",
                "http.followRedirects=false",
            ]);
            let command = ordinary.args(["ls-remote", "--get-url", "--", &dependency.url()]);
            sys.on(&command.display(), Ran::ok("file:///personal/repository"));
            sys.run_complete_stdout(&command).unwrap();
        }
        assert!(
            repair(&sys, &who, &parent, &repo, &current).is_err(),
            "{mode}"
        );
        assert!(sys.stat(&parent.join("vendor/library")).is_err(), "{mode}");
        assert!(sys.guarded_renames().is_empty(), "{mode}");
    }
}
