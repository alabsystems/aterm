// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::repo_work::PROBE_LIMIT;
use crate::harness::repo_work::tests::{repo, scratch_git};

/// What [`read_work`] reads of `dir` with `home` as the person's, nothing held back.
fn read(dir: &Path, home: &Path) -> WorkRead {
    read_work(dir, Some(home), None, PROBE_LIMIT, &|| false)
}

/// The record [`read_work`] reads of `dir` with `home` as the person's, which must be one.
fn record(dir: &Path, home: &Path) -> WorkRecord {
    match read(dir, home) {
        WorkRead::Read(record) => *record,
        other => panic!("a record of {}: {other:?}", dir.display()),
    }
}

/// The record's reads of `dir` with `home` as the person's and `held` its hold on new starts,
/// with the git starts they made.
fn counted(dir: &Path, home: &Path, held: &dyn Fn() -> bool) -> (WorkRead, u32) {
    let reads = Reads {
        deadline: Cell::new(Instant::now() + PROBE_LIMIT),
        each: Some(PROBE_LIMIT),
        home,
        xdg: None,
        cut: Cell::new(false),
        starts: Cell::new(0),
        held: Some(held),
        refused: None,
    };
    let read = work_reading(dir, &reads);
    (read, reads.starts.get())
}

/// An executable `sh` script at `path` running `body`.
fn script(path: &Path, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path.display().to_string()
}

/// A home with nothing in it, beside the repository under `tmp`.
fn empty_home(tmp: &Path) -> PathBuf {
    let home = tmp.join("home");
    std::fs::create_dir_all(&home).expect("home");
    home
}

/// A PLANTED DIFF DRIVER NEVER RUNS: a repository whose own configuration names an external diff
/// (`diff.external`), a `textconv` driver its `.gitattributes` binds to every file, and a
/// `core.fsmonitor` hook, each writing its own marker, with a committed change on a branch off
/// its origin's main and a change in its working tree. The record names the changed files and no
/// marker is written. NEGATIVE CONTROL, in the same repository and environment: a plain `git
/// diff` (a patch) writes the external marker, `git diff --no-ext-diff` the textconv one, and
/// `git status` the fsmonitor one, so each plant is live.
#[test]
fn a_planted_diff_driver_never_runs() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let home = empty_home(tmp.path());
    let marks = tmp.path().join("marks");
    std::fs::create_dir_all(&marks).expect("marks");
    let marker = |name: &str| marks.join(name);
    let plant = |name: &str, body: &str| {
        script(
            &tmp.path().join(format!("{name}.sh")),
            &format!("printf x > '{}'\n{body}", marker(name).display()),
        )
    };
    let external = plant("external", "exit 0");
    let textconv = plant("textconv", "cat \"$1\"");
    let fsmonitor = plant("fsmonitor", "exit 1");
    scratch_git(&work, &["config", "diff.external", &external]);
    scratch_git(&work, &["config", "diff.mark.textconv", &textconv]);
    scratch_git(&work, &["config", "core.fsmonitor", &fsmonitor]);
    let base = scratch_git(&work, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    scratch_git(&work, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(work.join(".gitattributes"), "* diff=mark\n").expect("attributes");
    std::fs::write(work.join("a.txt"), "two\n").expect("a.txt");
    scratch_git(&work, &["add", ".gitattributes", "a.txt"]);
    scratch_git(&work, &["commit", "-q", "-m", "two"]);
    std::fs::write(work.join("b.txt"), "new\n").expect("b.txt");
    // Setting the repository up ran its fsmonitor hook: only what the record does counts.
    std::fs::remove_dir_all(&marks).expect("clear");
    std::fs::create_dir_all(&marks).expect("marks");

    let read = record(&work, &home);
    for name in ["external", "textconv", "fsmonitor"] {
        assert!(
            !marker(name).exists(),
            "the record ran the planted {name} driver"
        );
    }
    assert_eq!(read.paths, [".gitattributes", "a.txt", "b.txt"], "{read:?}");
    assert_eq!(read.base.as_deref(), Some(base.as_str()));

    scratch_git(&work, &["diff", &base, "HEAD"]);
    assert!(
        marker("external").exists(),
        "CONTROL: git diff runs diff.external"
    );
    scratch_git(&work, &["diff", "--no-ext-diff", &base, "HEAD"]);
    assert!(
        marker("textconv").exists(),
        "CONTROL: git diff --no-ext-diff runs the textconv driver"
    );
    scratch_git(&work, &["status", "--porcelain"]);
    assert!(
        marker("fsmonitor").exists(),
        "CONTROL: git status runs core.fsmonitor"
    );
}

/// TWO WORKTREES OF ONE REPOSITORY NAME ONE COMMON DIR: a repository and a worktree added to it
/// read one common dir, and so one [`crate::harness::work_record::RepoKey`], each its own top
/// level. NEGATIVE CONTROL: a separate clone of the same origin names another common dir.
#[test]
fn two_worktrees_of_one_repository_name_one_common_dir() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let home = empty_home(tmp.path());
    let tree = tmp.path().join("tree");
    let tree_arg = tree.display().to_string();
    scratch_git(&work, &["worktree", "add", "-q", "-b", "side", &tree_arg]);
    let (main, side) = (record(&work, &home), record(&tree, &home));
    assert_eq!(main.common_dir, side.common_dir, "one repository");
    assert_eq!(main.key(), side.key());
    assert_ne!(main.tree, side.tree, "two worktrees");
    assert_eq!(
        main.common_dir,
        std::fs::canonicalize(work.join(".git")).expect("the common dir")
    );
    assert_eq!(
        side.tree,
        std::fs::canonicalize(&tree).expect("the worktree")
    );
    assert_eq!(
        (main.branch.as_deref(), side.branch.as_deref()),
        (Some("main"), Some("side"))
    );

    let origin = tmp.path().join("remote.git").display().to_string();
    scratch_git(tmp.path(), &["clone", "-q", &origin, "clone"]);
    let clone = record(&tmp.path().join("clone"), &home);
    assert_ne!(
        clone.common_dir, main.common_dir,
        "CONTROL: a separate clone is another repository"
    );
    assert_ne!(clone.key(), main.key());
}

/// THE RECORD NAMES ITS BRANCH, BASE, BEHIND AND CHANGED FILES: a branch whose one commit touches
/// `a.txt`, origin's main two commits ahead of its base, a modified `b.txt`, an untracked
/// `c.txt`, a `d.txt` the repository ignores and an `e.txt` the person's global excludes ignore
/// names `a.txt`, `b.txt` and `c.txt`, two behind `origin/main` at the merge-base. NEGATIVE
/// CONTROL: with the person's excludes file gone, `e.txt` is named too (their excludes are read).
#[test]
fn the_record_names_its_branch_base_behind_and_changed_files() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let home = empty_home(tmp.path());
    let commit = |names: &[(&str, &str)], message: &str| {
        for (name, text) in names {
            std::fs::write(work.join(name), text).expect("write");
            scratch_git(&work, &["add", name]);
        }
        scratch_git(&work, &["commit", "-q", "-m", message]);
    };
    commit(&[("b.txt", "b\n"), (".gitignore", "d.txt\n")], "base");
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    let base = scratch_git(&work, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    scratch_git(&work, &["checkout", "-q", "-b", "feat"]);
    scratch_git(&work, &["checkout", "-q", "main"]);
    commit(&[("m1.txt", "1\n")], "main one");
    commit(&[("m2.txt", "2\n")], "main two");
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    scratch_git(&work, &["checkout", "-q", "feat"]);
    commit(&[("a.txt", "changed\n")], "feat");
    std::fs::write(work.join("b.txt"), "b, edited\n").expect("b.txt");
    for name in ["c.txt", "d.txt", "e.txt"] {
        std::fs::write(work.join(name), "x\n").expect("untracked");
    }
    let ignore = home.join(".config/git/ignore");
    std::fs::create_dir_all(ignore.parent().expect("dir")).expect("config");
    std::fs::write(&ignore, "e.txt\n").expect("ignore");

    let read = record(&work, &home);
    assert_eq!(read.branch.as_deref(), Some("feat"));
    assert_eq!(read.detached, None);
    assert_eq!(read.main_ref, "origin/main");
    assert_eq!(read.base.as_deref(), Some(base.as_str()));
    assert_eq!(read.behind, Some(2));
    assert_eq!(read.paths, ["a.txt", "b.txt", "c.txt"], "{read:?}");
    assert_eq!(read.files, 3);
    assert!(read.starts > 0 && read.read_ms > 0, "{read:?}");
    assert!(read.host.is_empty(), "the publisher names the host");

    std::fs::remove_file(&ignore).expect("rm");
    let unexcluded = record(&work, &home);
    assert_eq!(
        unexcluded.paths,
        ["a.txt", "b.txt", "c.txt", "e.txt"],
        "CONTROL: the person's excludes file gone"
    );
}

/// THE NAMES STOP AT 64 AND THE COUNT GOES ON: 70 new files give the first 64 names, sorted, and
/// a count of 70. NEGATIVE CONTROL: 64 new files give 64 and 64, nothing cut.
#[test]
fn the_names_stop_at_64_and_the_count_goes_on() {
    use crate::harness::work_record::WORK_PATHS;
    for (made, names) in [(70, WORK_PATHS), (WORK_PATHS, WORK_PATHS)] {
        let tmp = aterm_tempfile::tempdir().expect("tmp");
        let work = repo(tmp.path(), true);
        let home = empty_home(tmp.path());
        let all: Vec<String> = (0..made).map(|n| format!("f{n:03}.txt")).collect();
        for name in &all {
            std::fs::write(work.join(name), "x\n").expect("file");
        }
        let read = record(&work, &home);
        assert_eq!(
            read.files,
            u32::try_from(made).expect("count"),
            "{made} files"
        );
        assert_eq!(read.paths, all[..names], "{made} files: the first {names}");
    }
}

/// A GUARDED FOLDER IS NEVER READ: a repository under a folder macOS guards (the home's
/// `Documents`) reads as nothing to show, and no git starts. NEGATIVE CONTROL: the same
/// repository outside it is read.
#[test]
fn a_guarded_folder_is_never_read() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = std::fs::canonicalize(tmp.path())
        .expect("owned tmp")
        .join("home");
    let guarded = repo(&home.join("Documents"), false);
    let open = repo(&home.join("Developer"), false);
    assert_eq!(read(&guarded, &home), WorkRead::None);
    assert_eq!(
        counted(&guarded, &home, &|| false),
        (WorkRead::None, 0),
        "no git started"
    );
    let (read, starts) = counted(&open, &home, &|| false);
    assert!(
        matches!(&read, WorkRead::Read(r) if r.branch.as_deref() == Some("main")),
        "CONTROL: the same repository outside it: {read:?}"
    );
    assert!(starts > 0, "CONTROL: git ran");
}

/// A READ THE HANG DETECTOR CUTS IS UNREAD: a read starved past [`PROBE_LIMIT`], the first or any
/// later one of the record's, makes it [`WorkRead::Unread`], never an empty record. NEGATIVE
/// CONTROL: unstarved, the same repository is read.
#[test]
fn a_read_the_hang_detector_cuts_is_unread() {
    use crate::harness::repo_work::read_stall;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let home = empty_home(tmp.path());
    std::fs::write(work.join("b.txt"), "x\n").expect("b.txt");
    let whole = record(&work, &home);
    assert_eq!(whole.paths, ["b.txt"], "CONTROL: unstarved, it is read");
    let hung = u64::try_from(PROBE_LIMIT.as_millis()).unwrap_or(u64::MAX) + 1_000;
    for k in 0..whole.starts {
        read_stall::set_after(k, hung);
        let cut = read(&work, &home);
        read_stall::set(0);
        assert_eq!(cut, WorkRead::Unread, "read {k} past its limit");
    }
}

/// A HOLD THAT COMES MID-READ STARTS NO MORE GIT: a hold on new starts that turns on before the
/// record's `k`-th git, for every `k` its reads make, ends the read there, unread, after exactly
/// the `k` gits before it. NEGATIVE CONTROL: with no hold the same repository is read whole.
#[test]
fn a_hold_that_comes_mid_read_starts_no_more_git() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let home = empty_home(tmp.path());
    std::fs::write(work.join("b.txt"), "x\n").expect("b.txt");
    let (whole, all) = counted(&work, &home, &|| false);
    assert!(
        matches!(&whole, WorkRead::Read(r) if r.paths == ["b.txt"]),
        "CONTROL: with no hold it is read: {whole:?}"
    );
    for k in 0..all {
        let looks = Cell::new(0u32);
        let held = || {
            let n = looks.get();
            looks.set(n + 1);
            n >= k
        };
        assert_eq!(
            counted(&work, &home, &held),
            (WorkRead::Unread, k),
            "held from git {k} on"
        );
    }
}

/// A MERGE-BASE GIT CANNOT MAKE IS UNREAD: a branch two commits off origin's main whose first
/// commit's object is gone makes git refuse its merge-base, and the read is unread, never a record
/// that names none of the files the branch committed. NEGATIVE CONTROLS: before the object went,
/// the record names both files at the base; a branch with no commit yet beside the same main is
/// read, its working tree's names and no base; and a branch with no commit in common with main
/// (git's quiet "none") is read with no base and nothing behind.
#[test]
fn a_merge_base_git_cannot_make_is_unread() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let home = empty_home(tmp.path());
    let commit = |name: &str| {
        std::fs::write(work.join(name), "x\n").expect("write");
        scratch_git(&work, &["add", name]);
        scratch_git(&work, &["commit", "-q", "-m", name]);
        scratch_git(&work, &["rev-parse", "HEAD"])
            .trim()
            .to_string()
    };
    let base = scratch_git(&work, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    scratch_git(&work, &["checkout", "-q", "-b", "feat"]);
    let first = commit("f1.txt");
    commit("f2.txt");
    let whole = record(&work, &home);
    assert_eq!(
        (whole.base.as_deref(), whole.behind),
        (Some(base.as_str()), Some(0)),
        "CONTROL: the whole branch's base"
    );
    assert_eq!(whole.paths, ["f1.txt", "f2.txt"], "CONTROL: its names");

    let object = work
        .join(".git/objects")
        .join(&first[..2])
        .join(&first[2..]);
    std::fs::remove_file(&object).expect("the first commit's loose object");
    assert_eq!(read(&work, &home), WorkRead::Unread, "a merge-base refused");

    scratch_git(&work, &["checkout", "-q", "--orphan", "fresh"]);
    let unborn = record(&work, &home);
    assert_eq!(unborn.branch.as_deref(), Some("fresh"));
    assert_eq!(
        (unborn.base, unborn.behind),
        (None, None),
        "CONTROL: no commit yet, no base"
    );
    assert_eq!(
        unborn.paths,
        ["a.txt", "f1.txt", "f2.txt"],
        "CONTROL: its working tree's names"
    );

    scratch_git(&work, &["commit", "-q", "-m", "apart"]);
    let apart = record(&work, &home);
    assert_eq!(
        (apart.base, apart.behind, apart.paths.len()),
        (None, None, 0),
        "CONTROL: no commit in common, read with no base"
    );
}

/// A STATUS IN A PARTIAL CLONE FETCHES NOTHING: a blobless clone whose index holds a move its
/// `HEAD` lacks, the old file's blob never fetched, is read with both paths named and no fetch
/// made (its origin's `uploadpack` writes a marker each time it runs). NEGATIVE CONTROL: a plain
/// `git status` there, which looks for renames, fetches the blob and writes the marker.
#[test]
fn a_status_in_a_partial_clone_fetches_nothing() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let home = empty_home(tmp.path());
    let bare = tmp.path().join("remote.git");
    scratch_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    std::fs::write(work.join("old.rs"), "one\ntwo\nthree\nfour\nfive\n").expect("old.rs");
    scratch_git(&work, &["add", "old.rs"]);
    scratch_git(&work, &["commit", "-q", "-m", "old"]);
    scratch_git(&work, &["mv", "old.rs", "new.rs"]);
    std::fs::write(work.join("new.rs"), "one\ntwo\nthree\nfour\nfive\nsix\n").expect("new.rs");
    scratch_git(&work, &["commit", "-q", "-am", "moved"]);
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    let url = format!("file://{}", bare.display());
    scratch_git(
        tmp.path(),
        &[
            "clone",
            "-q",
            "--filter=blob:none",
            "-b",
            "main",
            &url,
            "partial",
        ],
    );
    let partial = tmp.path().join("partial");
    let marker = tmp.path().join("fetched");
    let upload = script(
        &tmp.path().join("upload.sh"),
        &format!(
            "printf x >> '{}'\nexec '{}' upload-pack \"$@\"",
            marker.display(),
            crate::test_program::git().display()
        ),
    );
    scratch_git(&partial, &["config", "remote.origin.uploadpack", &upload]);
    scratch_git(&partial, &["reset", "-q", "--soft", "HEAD~1"]);

    let read = record(&partial, &home);
    assert_eq!(read.paths, ["new.rs", "old.rs"], "{read:?}");
    assert!(!marker.exists(), "the record's read fetched");

    scratch_git(&partial, &["status", "--porcelain"]);
    assert!(
        marker.exists(),
        "CONTROL: a plain git status fetches the old blob"
    );
}

/// A NAME IN BYTES THAT ARE NO UTF-8 IS NAMED LOSSILY: an index entry so named (staged, its file
/// never written) is read, its name decoded with a replacement character, as the committed names
/// are. NEGATIVE CONTROL: the same repository before it is read with no such name.
#[test]
fn a_name_in_no_utf8_is_named_lossily() {
    use std::os::unix::ffi::OsStrExt as _;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let home = empty_home(tmp.path());
    assert!(
        record(&work, &home).paths.is_empty(),
        "CONTROL: nothing changed"
    );
    let blob = scratch_git(&work, &["hash-object", "-w", "a.txt"])
        .trim()
        .to_string();
    let mut entry = format!("100644,{blob},bad").into_bytes();
    entry.extend_from_slice(b"\xffname");
    let mut cmd = aterm_uds::spawnfd::command(crate::test_program::git());
    if let Some(exec_path) = crate::test_program::git_exec_path() {
        cmd.env("GIT_EXEC_PATH", exec_path);
    }
    let staged = aterm_uds::deadline::output_until(
        cmd.args(["update-index", "--add", "--cacheinfo"])
            .arg(std::ffi::OsStr::from_bytes(&entry))
            .current_dir(&work)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null"),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("update-index within the hang detector");
    assert!(staged.status.success(), "update-index: {staged:?}");
    assert_eq!(record(&work, &home).paths, ["bad\u{fffd}name"]);
}
