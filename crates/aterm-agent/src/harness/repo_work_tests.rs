// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::path::PathBuf;

/// `git <args>` in `dir`, hermetic: no system or global config, a fixed
/// author.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn scratch_git(dir: &Path, args: &[&str]) {
    let out = aterm_uds::spawnfd::command("git")
        .args([
            "-c",
            "user.name=Andrew Yates",
            "-c",
            "user.email=a@example.invalid",
        ])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository at `<tmp>/work` with one commit, pushed to a bare remote at
/// `<tmp>/remote.git` when `remote`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn repo(tmp: &Path, remote: bool) -> PathBuf {
    let work = tmp.join("work");
    std::fs::create_dir_all(&work).expect("work");
    scratch_git(&work, &["init", "-q", "."]);
    std::fs::write(work.join("a.txt"), "one\n").expect("a.txt");
    scratch_git(&work, &["add", "a.txt"]);
    scratch_git(&work, &["commit", "-q", "-m", "one"]);
    if remote {
        let bare = tmp.join("remote.git");
        std::fs::create_dir_all(&bare).expect("bare");
        scratch_git(&bare, &["init", "-q", "--bare", "."]);
        let url = bare.display().to_string();
        scratch_git(&work, &["remote", "add", "origin", &url]);
        scratch_git(&work, &["push", "-q", "-u", "origin", "HEAD:main"]);
    }
    work
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn found(dir: &Path) -> Option<RepoWork> {
    probe(dir, Some(dir), PROBE_LIMIT)
}

/// THE READS' OUTPUT, PARSED ([`RepoWork::of_git`]): a tracked change
/// staged or not, a rename (its source field skipped), an untracked path,
/// stash lines and unpushed commits each count; ignored paths do not; at
/// most [`NAMED_PATHS`] paths are named, each printable. NEGATIVE CONTROL: a
/// clean status, no stash and nothing unpushed is no finding.
#[test]
fn the_reads_output_is_counted_and_a_clean_repository_is_no_finding() {
    let status = " M src/a.rs\0M  b.rs\0R  new.rs\0old.rs\0?? notes/\0!! target/\0";
    let w = RepoWork::of_git("/r\n", status, "stash@{0}: WIP on main\n", 2).expect("found");
    assert_eq!(
        (
            w.repo.as_str(),
            w.changed,
            w.untracked,
            w.stashes,
            w.unpushed
        ),
        ("/r", 3, 1, 1, 2)
    );
    assert_eq!(w.paths, ["src/a.rs", "b.rs", "new.rs"]);
    assert_eq!(
        w.summary(),
        "/r: 3 changed, 1 untracked (src/a.rs, b.rs, new.rs, …), 1 stash, 2 unpushed commits"
    );
    // A path that would end a typed line is printed harmless.
    let w = RepoWork::of_git("/r", "?? evil\nname\0", "", 0).expect("found");
    assert_eq!(w.paths, ["evil?name"]);
    assert_eq!(w.summary(), "/r: 1 untracked (evil?name)");
    let w = RepoWork::of_git("/r", "", "", 1).expect("found");
    assert_eq!(w.summary(), "/r: 1 unpushed commit");
    // NEGATIVE CONTROLS.
    assert_eq!(RepoWork::of_git("/r", "", "", 0), None);
    assert_eq!(RepoWork::of_git("/r", "!! target/\0", "\n", 0), None);
    // Public findings accept the full count domain. A mixed finding must not
    // wrap to "nothing", and its path suffix still records omitted paths.
    let w = RepoWork::of_git("/r", "?? one\0", "", usize::MAX).expect("mixed finding");
    assert!(w.found());
    assert_eq!((w.untracked, w.unpushed), (1, usize::MAX));
    let w = RepoWork {
        repo: "/r".into(),
        changed: usize::MAX,
        untracked: 1,
        paths: vec!["one".into()],
        ..RepoWork::default()
    };
    assert!(w.found());
    assert!(w.summary().contains("(one, …)"));
}

/// A long path keeps its end, and the whole stays within the bound.
#[test]
fn a_long_path_keeps_its_end() {
    let long = format!("{}/tail.rs", "d".repeat(200));
    let p = printable(&long);
    assert!(p.starts_with('…') && p.ends_with("/tail.rs"), "{p}");
    assert_eq!(p.chars().count(), PATH_CHARS);
    assert_eq!(printable("short.rs"), "short.rs");
}

/// THE PROBE OVER A REAL REPOSITORY: an uncommitted file, a commit no
/// remote has and a stash are each found, and once the work is committed
/// and pushed nothing is. NEGATIVE CONTROLS: a clean, pushed repository; a
/// repository with no remote at all (nowhere to push, so its commits are no
/// finding); a folder that is no repository.
// The unattended path resolver has mount-table backends on macOS and Linux.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_probe_finds_uncommitted_unpushed_and_stashed_work_and_nothing_once_pushed() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    assert_eq!(found(&work), None, "clean and pushed");
    // An untracked file, read from a folder below the top level.
    std::fs::create_dir_all(work.join("sub")).expect("sub");
    std::fs::write(work.join("sub/new.rs"), "fn main() {}\n").expect("new");
    let w = found(&work.join("sub")).expect("untracked");
    assert_eq!((w.untracked, w.changed, w.unpushed), (1, 0, 0), "{w:?}");
    assert_eq!(w.paths, ["sub/"]);
    assert!(w.repo.ends_with("/work"), "{w:?}");
    // Committed, not pushed.
    scratch_git(&work, &["add", "sub/new.rs"]);
    scratch_git(&work, &["commit", "-q", "-m", "two"]);
    let w = found(&work).expect("unpushed");
    assert_eq!((w.untracked, w.changed, w.unpushed), (0, 0, 1), "{w:?}");
    // Pushed: nothing.
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&work), None, "pushed");
    // A tracked change, staged or not.
    std::fs::write(work.join("a.txt"), "two\n").expect("edit");
    let w = found(&work).expect("changed");
    assert_eq!(
        (w.changed, w.paths.as_slice()),
        (1, &["a.txt".to_string()][..])
    );
    // Stashed: the change is gone from the tree, the stash is found.
    scratch_git(&work, &["stash", "-q"]);
    let w = found(&work).expect("stash");
    assert_eq!((w.changed, w.stashes), (0, 1), "{w:?}");
    scratch_git(&work, &["stash", "drop", "-q"]);
    assert_eq!(found(&work), None, "the stash dropped");
    // NEGATIVE CONTROLS: no remote at all, and no repository.
    let local = aterm_tempfile::tempdir().expect("tmp");
    let alone = repo(local.path(), false);
    assert_eq!(found(&alone), None, "a repository with no remote");
    let plain = aterm_tempfile::tempdir().expect("tmp");
    assert_eq!(found(plain.path()), None, "no repository");
}

/// Directory admission resolves aliases before deciding whether the probe
/// may start Git. All paths and links are owned; mount facts are captured.
#[cfg(unix)]
#[test]
fn a_guarded_folder_is_never_probed() {
    use aterm_sysprobe::volumes::{Mount, Table};
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let base = std::fs::canonicalize(tmp.path()).expect("owned tmp");
    let home = base.join("home");
    let work = home.join("Developer/work");
    let docs = home.join("Documents/work");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::create_dir_all(&docs).expect("docs");
    let table = Table {
        mounts: vec![Mount {
            on: PathBuf::from("/"),
            from: "owned fixture".into(),
            fstype: "captured local".into(),
            local: true,
            automounted: false,
            read_only: false,
            container: None,
        }],
        complete: true,
        ..Table::default()
    };
    assert_eq!(
        unattended_dir(&work, Some(&home), &table),
        Some(work.clone())
    );
    assert_eq!(unattended_dir(&docs, Some(&home), &table), None);
    assert_eq!(probe(&docs, Some(&home), PROBE_LIMIT), None);
    assert_eq!(unattended_dir(&work, None, &table), None);
    assert_eq!(probe(&work, None, PROBE_LIMIT), None);
    assert_eq!(
        probe(Path::new("/Volumes/unasked"), None, PROBE_LIMIT),
        None
    );
    assert_eq!(
        unattended_dir(
            &home.join("Developer/../Documents/work"),
            Some(&home),
            &table
        ),
        None
    );
    assert_eq!(
        unattended_dir(&work, Some(&home.join("../home")), &table),
        None
    );
    assert_eq!(
        unattended_dir(&work, Some(Path::new("relative")), &table),
        None
    );
    assert_eq!(
        unattended_dir(Path::new("relative"), Some(&home), &table),
        None
    );
    assert_eq!(
        unattended_dir(&home.join("missing"), Some(&home), &table),
        None
    );
    let alias = home.join("Developer/alias");
    std::os::unix::fs::symlink(&docs, &alias).expect("owned directory alias");
    assert_eq!(unattended_dir(&alias, Some(&home), &table), None);
    let home_alias = base.join("home-alias");
    std::os::unix::fs::symlink(&home, &home_alias).expect("owned home alias");
    assert_eq!(unattended_dir(&docs, Some(&home_alias), &table), None);
    assert_eq!(
        unattended_dir(&work, Some(&home_alias), &table),
        Some(work.clone())
    );
    let incomplete = Table {
        complete: false,
        ..table.clone()
    };
    assert_eq!(unattended_dir(&work, Some(&home), &incomplete), None);
    for (local, automounted) in [(false, false), (true, true)] {
        let mut unavailable = table.clone();
        unavailable.mounts.push(Mount {
            on: work.clone(),
            local,
            automounted,
            ..table.mounts[0].clone()
        });
        assert_eq!(unattended_dir(&work, Some(&home), &unavailable), None);
    }
}

/// THE SESSION'S DIRECTORY ([`session_dir`]): the transcript's last `cwd`
/// (a `cd` into a worktree moves it), else where the agent started; a row
/// that cannot be read, or names no absolute directory, is passed over.
/// NEGATIVE CONTROL: no transcript and no absolute start names none.
#[test]
fn the_session_works_in_the_transcript_s_last_cwd_else_where_it_started() {
    let tail = concat!(
        "{\"type\":\"user\",\"cwd\":\"/Users//a/aterm\"}\n",
        "{\"type\":\"assistant\",\"cwd\":\"/Users//a/Developer/H-wt\"}\n",
        "{\"type\":\"summary\"}\n",
        "{\"cwd\":\"/Users//a/Developer/cut-off\n",
    );
    assert_eq!(
        session_dir("/Users//a/aterm", Some(tail)),
        Some(PathBuf::from("/Users//a/Developer/H-wt"))
    );
    assert_eq!(
        session_dir("/Users//a/aterm", Some("{\"cwd\":\"relative\"}\n")),
        Some(PathBuf::from("/Users//a/aterm"))
    );
    assert_eq!(
        session_dir("/Users//a/aterm", None),
        Some(PathBuf::from("/Users//a/aterm"))
    );
    assert_eq!(session_dir("", None), None);
    assert_eq!(session_dir("relative", Some("no json\n")), None);
}
