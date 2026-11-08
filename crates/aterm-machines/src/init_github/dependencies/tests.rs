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
    sys.on(
        &local(sys, who, None).arg("--version").display(),
        Ran::ok("git version fixture with no-lazy-fetch\n"),
    );
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
    nothing_named(sys, git());
    sys.on(
        &checkout::Programs::default()
            .off(git())
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

/// The repository `git` runs in names no hook or filter driver ([`checkout::Programs::named`]).
fn nothing_named(sys: &Fake, git: Cmd) {
    sys.on(
        &checkout::Programs::listing(git).display(),
        Ran::exit(1, ""),
    );
}

pub(in crate::init_github) fn existing(sys: &Fake, who: &User, path: &Path, name: &str, pin: &str) {
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
        &super::super::repos::partial_listing(ordinary()).display(),
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
            .args([
                "rev-list",
                "--objects",
                "--no-walk",
                "--quiet",
                "--missing=error",
                &format!("{pin}^{{commit}}"),
                "--",
            ])
            .display(),
        Ran::ok(""),
    );
    sys.on(
        &ordinary()
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
    sys.executable(crate::items::github::CLT_GIT, b"fixture git");
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
fn unsupported_git_refuses_dependency_work_without_an_ordinary_fallback() {
    let (sys, who, repo, parent) = fixture(&["vendor/library"]);
    sys.simulate(|_, command| {
        command
            .args
            .iter()
            .any(|arg| arg == "--no-lazy-fetch")
            .then(|| Ran::exit(129, "unknown option: --no-lazy-fetch"))
    });
    assert!(matches!(
        ready(&sys, &who, &parent, &repo),
        Err(Unready::Git(_))
    ));
    let why = repair(&sys, &who, &parent, &repo, &|| Ok(())).unwrap_err();
    assert!(
        why.to_string().contains("must support --no-lazy-fetch"),
        "{why}"
    );
    let calls = sys.calls();
    assert_eq!(
        calls.len(),
        2,
        "one capability probe at each entry: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|call| call.contains("--no-lazy-fetch") && call.ends_with(" --version")),
        "{calls:?}"
    );
    assert!(sys.stat(&parent.join("vendor/library")).is_err());
    // A repository selecting no dependencies requires no new Git capability.
    let unselected = Repo {
        required_submodules: Vec::new(),
        ..repo
    };
    assert!(ready(&sys, &who, &parent, &unselected).unwrap());
    repair(&sys, &who, &parent, &unselected, &|| Ok(())).unwrap();
    assert_eq!(sys.calls(), calls);
}

#[test]
fn real_tracked_metadata_and_a_branch_at_the_pin_are_ready_without_moving_user_work() {
    use crate::port::Live;
    crate::items::github::need_native_git_with_no_lazy_fetch();
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
    assert!(matches!(
        ready(&sys, &who, &parent, &repo),
        Err(Unready::Preserved(why)) if why.contains("branch")
    ));
    assert!(repair(&sys, &who, &parent, &repo, &|| Ok(())).is_err());
    assert_eq!(run(&child, &["rev-parse", "HEAD"]).trim(), pin);
}

/// Reading whether a dependency is ready runs no filter driver the parent or the dependency
/// names: not on the parent's `.gitmodules` (its `diff`), nor on a dependency's file (its
/// `status`), each stat-dirty, so git would hash it again through the driver; a plain git runs
/// both.
#[test]
fn readiness_runs_no_filter_driver_a_parent_or_a_dependency_names() {
    use crate::port::Live;
    use std::os::unix::fs::PermissionsExt as _;
    crate::items::github::need_native_git_with_no_lazy_fetch();
    let sys = Live::new();
    let scratch = Scratch::new(&sys, "required-no-filter").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = scratch.path().to_string_lossy().into_owned();
    let parent = scratch.path().join("parent");
    let child = parent.join("vendor/library");
    sys.create_dir_all(&child, 0o700).unwrap();
    let marker = scratch.path().join("ran");
    let driver = scratch.path().join("driver");
    std::fs::write(
        &driver,
        format!("#!/bin/sh\necho ran >> '{}'\ncat\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&driver, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    // Stamped later than any index, so each read hashes the file again.
    let stat_dirty = |path: &Path| {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(86_400))
            .unwrap();
    };
    run(
        &child,
        &["init", "--template=", "--initial-branch=owner-work"],
    );
    run(
        &child,
        &["config", "filter.probe.clean", driver.to_str().unwrap()],
    );
    sys.write_file(
        &child.join(".gitattributes"),
        b"*.dat filter=probe\n",
        0o600,
    )
    .unwrap();
    sys.write_file(&child.join("library.dat"), b"data\n", 0o600)
        .unwrap();
    run(&child, &["add", "--", ".gitattributes", "library.dat"]);
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
    run(
        &parent,
        &["config", "filter.probe.clean", driver.to_str().unwrap()],
    );
    sys.write_file(
        &parent.join(".gitattributes"),
        b".gitmodules filter=probe\n",
        0o600,
    )
    .unwrap();
    sys.write_file(&parent.join(".gitmodules"), b"[submodule \"vendor/library\"]\npath = vendor/library\nurl = https://github.com/alice/library.git\n", 0o600).unwrap();
    run(&parent, &["add", "--", ".gitattributes", ".gitmodules"]);
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
    // The dependency moves past its pin, so its readiness reads its status.
    sys.write_file(&child.join("later.txt"), b"later\n", 0o600)
        .unwrap();
    run(&child, &["add", "--", "later.txt"]);
    run(&child, &["commit", "-m", "fixture later"]);
    stat_dirty(&parent.join(".gitmodules"));
    stat_dirty(&child.join("library.dat"));
    let _ = std::fs::remove_file(&marker);
    let repo = Repo {
        owner: "alice".into(),
        name: "parent".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: vec!["vendor/library".into()],
    };
    // Its status read, the dependency stops at its branch, the person's.
    assert!(matches!(
        ready(&sys, &who, &parent, &repo),
        Err(Unready::Preserved(why)) if why.contains("branch")
    ));
    assert!(
        !marker.exists(),
        "a driver ran: {}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
    // The control: plain reads run the driver, once for each file.
    run(&parent, &["diff", "--quiet", "HEAD", "--", ".gitmodules"]);
    run(&child, &["status", "--porcelain"]);
    assert!(
        std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .lines()
            .count()
            >= 2,
        "the driver is live"
    );
}

#[test]
fn an_existing_complete_dependency_reaches_its_preloaded_pin_without_a_working_transport() {
    use crate::port::Live;
    crate::items::github::need_native_git_with_no_lazy_fetch();
    let sys = Live::new();
    let scratch = Scratch::new(&sys, "required-offline-pin").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = scratch.path().to_string_lossy().into_owned();
    let parent = scratch.path().join("parent");
    let child = parent.join("vendor/library");
    sys.create_dir_all(&child, 0o700).unwrap();
    let run = |path: &Path, args: &[&str]| {
        text(
            &sys,
            local(&sys, &who, Some(path))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(checkout::NO_SIDE_RUNS)
                .args([
                    "-c",
                    "user.name=fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                ])
                .args(args.iter().copied()),
            "synthetic local git fixture failed",
        )
        .unwrap()
    };
    run(&child, &["init", "--template=", "--initial-branch=main"]);
    sys.write_file(
        &child.join("library.rs"),
        b"previous recorded source\n",
        0o600,
    )
    .unwrap();
    run(&child, &["add", "--", "library.rs"]);
    run(&child, &["commit", "-m", "fixture previous dependency"]);
    let old = run(&child, &["rev-parse", "HEAD"]).trim().to_owned();
    sys.write_file(
        &child.join("library.rs"),
        b"required preloaded source\n",
        0o600,
    )
    .unwrap();
    run(&child, &["commit", "-am", "fixture required dependency"]);
    let pin = run(&child, &["rev-parse", "HEAD"]).trim().to_owned();
    run(
        &child,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/alice/library.git",
        ],
    );
    // The legitimate SSH rewrite is admitted, but its transport cannot run. Any attempted
    // fetch fails before opening a connection; the exact pin already exists locally.
    run(
        &child,
        &[
            "config",
            "url.git@github.com:.insteadOf",
            "https://github.com/",
        ],
    );
    run(&child, &["config", "core.sshCommand", "/usr/bin/false"]);
    run(&child, &["checkout", "--detach", &old]);
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
    sys.write_file(
        &parent.join(".gitmodules"),
        b"[submodule \"vendor/library\"]\npath = vendor/library\nurl = ../library.git\n",
        0o600,
    )
    .unwrap();
    run(&parent, &["add", "--", ".gitmodules"]);
    run(
        &parent,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{old},vendor/library"),
        ],
    );
    run(&parent, &["commit", "-m", "fixture previous parent pin"]);
    run(
        &parent,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{pin},vendor/library"),
        ],
    );
    run(&parent, &["commit", "-m", "fixture required parent pin"]);
    let repo = Repo {
        owner: "alice".into(),
        name: "parent".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: vec!["vendor/library".into()],
    };
    assert!(!ready(&sys, &who, &parent, &repo).unwrap());
    repair(&sys, &who, &parent, &repo, &|| Ok(())).unwrap();
    assert!(ready(&sys, &who, &parent, &repo).unwrap());
    assert_eq!(run(&child, &["rev-parse", "HEAD"]).trim(), pin);
    assert_eq!(
        sys.read_file(&child.join("library.rs")).unwrap(),
        b"required preloaded source\n"
    );
    assert!(
        sys.run_complete_stdout(&local(&sys, &who, Some(&child)).args([
            "symbolic-ref",
            "--quiet",
            "HEAD"
        ]))
        .unwrap()
        .status
            == Some(1)
    );

    let backup = scratch.path().join("donor.git");
    run(
        scratch.path(),
        &[
            "clone",
            "--bare",
            "--no-hardlinks",
            "--",
            child.to_str().unwrap(),
            backup.to_str().unwrap(),
        ],
    );
    let blob = run(&child, &["rev-parse", &format!("{pin}:library.rs")])
        .trim()
        .to_owned();
    let object_path = child.join(".git/objects").join(&blob[..2]).join(&blob[2..]);
    let original = sys.read_file(&object_path).unwrap();
    // Missing objects at an already selected pin are unfinished too. Storage repair
    // must preserve the person's same-pin branch and edits without another checkout.
    run(&child, &["checkout", "-b", "owner-work"]);
    sys.write_file(&child.join("library.rs"), b"owner edits\n", 0o600)
        .unwrap();
    sys.remove(&object_path).unwrap();
    assert!(!ready(&sys, &who, &parent, &repo).unwrap());
    let why = repair(&sys, &who, &parent, &repo, &|| Ok(())).unwrap_err();
    assert!(
        why.to_string()
            .starts_with("cannot fetch the required dependency's recorded commit")
    );
    assert_eq!(
        run(&child, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/owner-work"
    );
    assert_eq!(
        sys.read_file(&child.join("library.rs")).unwrap(),
        b"owner edits\n"
    );
    sys.remove(&child.join("library.rs")).unwrap();
    assert!(repair(&sys, &who, &parent, &repo, &|| Ok(())).is_err());
    assert!(sys.stat(&child.join("library.rs")).is_err());
    sys.write_file(&object_path, &original, 0o444).unwrap();
    repair(&sys, &who, &parent, &repo, &|| Ok(())).unwrap();
    assert!(ready(&sys, &who, &parent, &repo).unwrap());
    assert!(sys.stat(&child.join("library.rs")).is_err());
    sys.write_file(
        &child.join("library.rs"),
        b"required preloaded source\n",
        0o600,
    )
    .unwrap();

    // A commit object does not imply that its checkout objects are present. Leave the
    // recorded old checkout clean, then remove only the new revision's loose blob.
    run(&child, &["checkout", "--detach", &old]);
    sys.remove(&object_path).unwrap();
    assert!(run(&child, &["status", "--porcelain"]).is_empty());
    assert!(
        sys.run_complete_stdout(&local(&sys, &who, Some(&child)).args([
            "cat-file",
            "-e",
            &format!("{pin}^{{commit}}"),
        ]))
        .unwrap()
        .success()
    );
    assert!(
        !sys.run_complete_stdout(&local(&sys, &who, Some(&child)).args(["cat-file", "-e", &blob]))
            .unwrap()
            .success()
    );
    let result = repair(&sys, &who, &parent, &repo, &|| Ok(()));
    assert!(
        result.is_err(),
        "incomplete checkout reported {result:?}; ready={:?}; head={}; file={:?}",
        ready(&sys, &who, &parent, &repo),
        run(&child, &["rev-parse", "HEAD"]).trim(),
        sys.read_file(&child.join("library.rs"))
    );
    let why = result.unwrap_err();
    assert!(
        why.to_string()
            .starts_with("cannot fetch the required dependency's recorded commit"),
        "an incomplete local checkout must try the admitted transport: {why}"
    );
    assert_eq!(run(&child, &["rev-parse", "HEAD"]).trim(), old);
    assert_eq!(
        sys.read_file(&child.join("library.rs")).unwrap(),
        b"previous recorded source\n"
    );
    // A real, isolated donor refetch restores the absent object. The ordinary repair
    // can then finish from that complete local store while SSH remains unavailable.
    read(
        &sys,
        local(&sys, &who, Some(&child))
            .args(["fetch", "--refetch", "--no-recurse-submodules", "--"])
            .arg(&backup)
            .arg(&pin),
        "isolated donor refetch failed",
    )
    .unwrap();
    repair(&sys, &who, &parent, &repo, &|| Ok(())).unwrap();
    assert!(ready(&sys, &who, &parent, &repo).unwrap());
    assert_eq!(
        sys.read_file(&child.join("library.rs")).unwrap(),
        b"required preloaded source\n"
    );
}

#[test]
fn partial_parent_metadata_reads_never_fetch_implicitly_but_explicit_fetch_still_works() {
    use crate::port::Live;
    crate::items::github::need_native_git_with_no_lazy_fetch();
    let sys = Live::new();
    let scratch = Scratch::new(&sys, "required-partial-parent").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = scratch.path().to_string_lossy().into_owned();
    let parent = scratch.path().join("parent");
    let backup = scratch.path().join("complete-backup.git");
    sys.create_dir_all(&parent, 0o700).unwrap();
    let base = || {
        checkout::git(&sys, &who, Some(&parent))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(checkout::NO_SIDE_RUNS)
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
    };
    let run = |args: &[&str]| {
        text(
            &sys,
            base().args(args.iter().copied()),
            "synthetic partial parent fixture failed",
        )
        .unwrap()
    };
    run(&["init", "--template=", "--initial-branch=main"]);
    run(&[
        "remote",
        "add",
        "origin",
        "https://github.com/alice/parent.git",
    ]);
    sys.write_file(
        &parent.join(".gitmodules"),
        b"[submodule \"vendor/library\"]\npath = vendor/library\nurl = ../library.git\n",
        0o600,
    )
    .unwrap();
    run(&["add", "--", ".gitmodules"]);
    run(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{PIN},vendor/library"),
    ]);
    run(&["commit", "-m", "fixture parent"]);
    let revision = run(&["rev-parse", "HEAD"]).trim().to_owned();
    let blob = run(&["rev-parse", "HEAD:.gitmodules"]).trim().to_owned();
    read(
        &sys,
        base()
            .args(["clone", "--bare", "--no-local", "--"])
            .arg(&parent)
            .arg(&backup),
        "cannot retain complete fixture objects",
    )
    .unwrap();
    run(&["config", "core.repositoryformatversion", "1"]);
    run(&["config", "extensions.partialclone", "origin"]);
    run(&["config", "remote.origin.promisor", "true"]);
    run(&["config", "remote.origin.partialclonefilter", "blob:none"]);
    run(&[
        "config",
        "url.git@github.com:.insteadOf",
        "https://github.com/",
    ]);
    run(&["config", "core.sshCommand", "/usr/bin/false"]);
    sys.remove(
        &parent
            .join(".git/objects")
            .join(&blob[..2])
            .join(&blob[2..]),
    )
    .unwrap();
    let query = [
        "config",
        "--blob",
        &format!("{revision}:.gitmodules"),
        "--no-includes",
        "--get-all",
        "submodule.vendor/library.url",
    ];
    let transport_started = |path: &Path| {
        String::from_utf8(sys.read_file(path).unwrap())
            .unwrap()
            .lines()
            .any(|line| {
                let event: Value = aterm_json::from_str(line).unwrap();
                event.get("event").and_then(Value::as_str) == Some("child_start")
                    && event
                        .get("argv")
                        .and_then(Value::as_array)
                        .is_some_and(|argv| argv.iter().any(|arg| arg.as_str() == Some("fetch")))
            })
    };
    // An ordinary Git read starts a promisor fetch through the existing parent's own
    // transport. Its harmless failing SSH command proves this without any connection.
    let control_trace = scratch.path().join("unprotected.trace.jsonl");
    let control = sys
        .run_complete_stdout(
            &base()
                .env("GIT_NO_LAZY_FETCH", "0")
                .env("GIT_TRACE2_EVENT", control_trace.to_string_lossy())
                .args(query),
        )
        .unwrap();
    assert!(!control.success());
    assert!(
        transport_started(&control_trace),
        "the negative control must attempt a lazy fetch"
    );
    let guarded_trace = scratch.path().join("guarded.trace.jsonl");
    let guarded = sys
        .run_complete_stdout(
            &local(&sys, &who, Some(&parent))
                .env("GIT_TRACE2_EVENT", guarded_trace.to_string_lossy())
                .args(query),
        )
        .unwrap();
    assert!(!guarded.success());
    assert!(
        !transport_started(&guarded_trace),
        "a local metadata read must never start transport"
    );
    let repo = Repo {
        owner: "alice".into(),
        name: "parent".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: vec!["vendor/library".into()],
    };
    assert!(ready(&sys, &who, &parent, &repo).is_err());
    assert!(sys.stat(&parent.join("vendor/library")).is_err());
    // Suppressing on-demand fetch leaves an explicitly requested fetch available. This
    // controlled local transport restores the blob and makes metadata readable again.
    read(
        &sys,
        local(&sys, &who, Some(&parent))
            .args(["fetch", "--refetch", "--no-recurse-submodules", "--"])
            .arg(&backup)
            .arg("HEAD"),
        "explicit fixture fetch was suppressed",
    )
    .unwrap();
    assert!(
        sys.run_complete_stdout(&local(&sys, &who, Some(&parent)).args(["cat-file", "-e", &blob]))
            .unwrap()
            .success()
    );
    assert!(!ready(&sys, &who, &parent, &repo).unwrap());
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
        nothing_named(&sys, git());
        sys.on(
            &checkout::Programs::default()
                .off(git())
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
                && !call.contains("rev-list --objects")
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
    nothing_named(&sys, git());
    sys.on(
        &checkout::Programs::default()
            .off(git())
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
    let fetched = AtomicBool::new(false);
    let inside = path.clone();
    sys.simulate(move |_, command| {
        let call = command.display();
        if call.contains("ls-remote --get-url -- https://github.com/alice/library.git") {
            return Some(Ran::ok("https://github.com/alice/library.git"));
        }
        if call.contains(" fetch ") {
            fetched.store(true, Ordering::SeqCst);
            return Some(Ran::ok(""));
        }
        if call.contains("rev-list --objects") {
            return Some(if fetched.load(Ordering::SeqCst) {
                Ran::ok("")
            } else {
                Ran::exit(1, "synthetic checkout objects not stored locally")
            });
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
            && fetch.contains("--refetch")
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
