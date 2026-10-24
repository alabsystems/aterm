// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::path::PathBuf;

/// `git <args>` in `dir`, hermetic: no system or global config, a fixed
/// author.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn scratch_git(dir: &Path, args: &[&str]) {
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
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
            .env("GIT_CONFIG_GLOBAL", "/dev/null"),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("git {args:?} within the hang detector: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository at `<tmp>/work` with one commit, pushed to a bare remote at
/// `<tmp>/remote.git` when `remote`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn repo(tmp: &Path, remote: bool) -> PathBuf {
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
    finished(probe(dir, Some(dir), PROBE_LIMIT))
}

/// A probe that finished: its finding, `None` for a clean repository.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn finished(read: RepoRead) -> Option<RepoWork> {
    assert!(!read.is_unread(), "the probe finished within its limit");
    read.into_found()
}

#[test]
fn the_reads_output_is_counted_and_a_clean_repository_is_no_finding() {
    let status = " M src/a.rs\0M  b.rs\0R  new.rs\0old.rs\0?? notes/\0!! target/\0";
    let w = RepoWork::of_git(
        "/r\n",
        status,
        "stash@{0}: WIP on main\n",
        2,
        0,
        "origin/main",
    )
    .expect("found");
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
    let w = RepoWork::of_git("/r", "?? evil\nname\0", "", 0, 0, "").expect("found");
    assert_eq!(w.paths, ["evil?name"]);
    assert_eq!(w.summary(), "/r: 1 untracked (evil?name)");
    let w = RepoWork::of_git("/r", "", "", 1, 0, "").expect("found");
    assert_eq!(w.summary(), "/r: 1 unpushed commit");
    let w = RepoWork::of_git("/r", "", "", 0, 2, "origin/main").expect("found");
    assert_eq!((w.unpushed, w.unlanded), (0, 2));
    assert_eq!(w.summary(), "/r: 2 commits not on origin/main");
    let w = RepoWork::of_git("/r", "", "", 1, 1, "origin/master").expect("found");
    assert_eq!(
        w.summary(),
        "/r: 1 unpushed commit, 1 commit not on origin/master"
    );
    assert_eq!(RepoWork::of_git("/r", "", "", 0, 0, ""), None);
    assert_eq!(RepoWork::of_git("/r", "", "", 0, 0, "origin/main"), None);
    assert_eq!(RepoWork::of_git("/r", "!! target/\0", "\n", 0, 0, ""), None);
    let w = RepoWork::of_git("/r", "?? one\0", "", usize::MAX, 0, "").expect("mixed finding");
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
/// and pushed nothing is.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_probe_finds_uncommitted_unpushed_and_stashed_work_and_nothing_once_pushed() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    assert_eq!(found(&work), None, "clean and pushed");
    std::fs::create_dir_all(work.join("sub")).expect("sub");
    std::fs::write(work.join("sub/new.rs"), "fn main() {}\n").expect("new");
    let w = found(&work.join("sub")).expect("untracked");
    assert_eq!((w.untracked, w.changed, w.unpushed), (1, 0, 0), "{w:?}");
    assert_eq!(w.paths, ["sub/"]);
    assert!(w.repo.ends_with("/work"), "{w:?}");
    scratch_git(&work, &["add", "sub/new.rs"]);
    scratch_git(&work, &["commit", "-q", "-m", "two"]);
    let w = found(&work).expect("unpushed");
    assert_eq!((w.untracked, w.changed, w.unpushed), (0, 0, 1), "{w:?}");
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&work), None, "pushed");
    std::fs::write(work.join("a.txt"), "two\n").expect("edit");
    let w = found(&work).expect("changed");
    assert_eq!(
        (w.changed, w.paths.as_slice()),
        (1, &["a.txt".to_string()][..])
    );
    scratch_git(&work, &["stash", "-q"]);
    let w = found(&work).expect("stash");
    assert_eq!((w.changed, w.stashes), (0, 1), "{w:?}");
    scratch_git(&work, &["stash", "drop", "-q"]);
    assert_eq!(found(&work), None, "the stash dropped");
    let local = aterm_tempfile::tempdir().expect("tmp");
    let alone = repo(local.path(), false);
    assert_eq!(found(&alone), None, "a repository with no remote");
    let plain = aterm_tempfile::tempdir().expect("tmp");
    assert_eq!(found(plain.path()), None, "no repository");
}

/// A PROBE OF A STARVED `git` STILL READS THE WHOLE WORK ([`PROBE_LIMIT`], the
/// one-minute hang detector): its first read starved 11 s, past the ten
/// seconds the probe had before (on the probe's clock: the time is taken as
/// gone, with no sleep), the probe finds the one commit no remote has.
/// A read cut by the shorter deadline is unread.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_probe_whose_reads_are_starved_still_finds_the_unpushed_commit() {
    assert_eq!(
        PROBE_LIMIT,
        aterm_spec::wait::MUST_HAPPEN,
        "the probe's limit is the one-minute hang detector"
    );
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    std::fs::write(work.join("b.txt"), "two\n").expect("b.txt");
    scratch_git(&work, &["add", "b.txt"]);
    scratch_git(&work, &["commit", "-q", "-m", "two"]);
    read_stall::set(11_000);
    let whole = probe(&work, Some(&work), PROBE_LIMIT);
    read_stall::set(11_000);
    let cut = probe(&work, Some(&work), Duration::from_secs(10));
    read_stall::set(0);
    let w = whole
        .into_found()
        .expect("the unpushed commit, found by the starved probe");
    assert_eq!((w.unpushed, w.unlanded, w.changed), (1, 1, 0), "{w:?}");
    assert_eq!(
        cut,
        RepoRead::Unread,
        "the starved probe cut at ten seconds"
    );
}

/// A PROBE CUT AT ANY READ IS NEVER A CLEAN REPOSITORY ([`RepoRead::Unread`]):
/// over a repository holding one unpushed commit and over one holding one
/// changed file, a probe whose `k`-th read is starved past its limit (on the
/// probe's clock, no sleep) reads as the work it found or as unread, for
/// every `k`. NEGATIVE CONTROL: the same repositories pushed and clean, with
/// no read starved, read clean.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_probe_cut_at_any_read_is_never_a_clean_repository() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let pushed = repo(&tmp.path().join("pushed"), true);
    let alone = repo(&tmp.path().join("alone"), false);
    assert_eq!(found(&pushed), None, "clean and pushed");
    assert_eq!(found(&alone), None, "a repository with no remote");
    std::fs::write(pushed.join("b.txt"), "two\n").expect("b.txt");
    scratch_git(&pushed, &["add", "b.txt"]);
    scratch_git(&pushed, &["commit", "-q", "-m", "two"]);
    std::fs::write(alone.join("a.txt"), "changed\n").expect("a.txt");
    for work in [&pushed, &alone] {
        let whole = found(work).expect("the work, read whole");
        let mut unread = 0;
        for k in 0..24 {
            read_stall::set_after(k, 11_000);
            let read = probe(work, Some(work), Duration::from_secs(10));
            read_stall::set(0);
            match read {
                RepoRead::Clean => panic!("read {k} cut: read as clean over {whole:?}"),
                RepoRead::Unread => unread += 1,
                RepoRead::Found(_) => {}
            }
        }
        assert!(unread > 0, "the first read cut reads as unread");
        assert_eq!(
            probe(work, Some(work), Duration::from_secs(10)),
            RepoRead::Found(whole),
            "the reads past the last one starve nothing"
        );
    }
}

/// A REPOSITORY WHOSE STATE CANNOT BE READ IS NEVER CLEAN ([`RepoRead::Unread`]):
/// with its index broken, `git status` fails in a repository `rev-parse` found,
/// and the probe reads unread whatever the repository held, where it read
/// nothing found before. NEGATIVE CONTROL: the same repositories with their
/// index whole read as they hold.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_repository_whose_state_cannot_be_read_is_never_clean() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let clean = repo(&tmp.path().join("clean"), true);
    let dirty = repo(&tmp.path().join("dirty"), false);
    std::fs::write(dirty.join("a.txt"), "changed\n").expect("a.txt");
    assert_eq!(found(&clean), None, "the control");
    assert!(found(&dirty).is_some(), "the control finds the change");
    for work in [&clean, &dirty] {
        std::fs::write(work.join(".git").join("index"), "DIRC").expect("index");
        assert_eq!(
            probe(work, Some(work), PROBE_LIMIT),
            RepoRead::Unread,
            "a broken index in {}",
            work.display()
        );
    }
}

#[test]
fn a_branchs_stash_entries_are_those_written_on_it() {
    let list = "WIP on main: 1a2b3c4 one\nOn main: a message\nWIP on side: 5d6e7f8 two\n\
                WIP on mainline: 9a8b7c6 three\nWIP on (no branch): 0f1e2d3 four\n";
    assert_eq!(
        stashes_on(list, Some("main")),
        "WIP on main: 1a2b3c4 one\nOn main: a message\n"
    );
    assert_eq!(stashes_on(list, Some("side")), "WIP on side: 5d6e7f8 two\n");
    assert_eq!(stashes_on(list, None), "WIP on (no branch): 0f1e2d3 four\n");
    assert_eq!(stashes_on(list, Some("other")), "");
    assert_eq!(stashes_on("", Some("main")), "");
}

/// A STASH ANOTHER WORKTREE MADE IS NONE OF THIS ONE'S: a repository's
/// stash is shared by its worktrees, and an entry made in another worktree,
/// on its own branch, is another session's work — the review found the
/// probe counted it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_stash_another_worktree_made_is_none_of_this_ones() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let other = tmp.path().join("other");
    let other_s = other.to_str().expect("utf-8");
    scratch_git(&work, &["worktree", "add", "-q", "-b", "side", other_s]);
    std::fs::write(other.join("a.txt"), "side\n").expect("edit");
    scratch_git(&other, &["stash", "-q"]);
    assert_eq!(found(&work), None, "the other worktree's stash");
    let w = found(&other).expect("its own stash");
    assert_eq!((w.stashes, w.changed), (1, 0), "{w:?}");
    std::fs::write(work.join("a.txt"), "here\n").expect("edit");
    scratch_git(&work, &["stash", "-q"]);
    let w = found(&work).expect("this worktree's stash");
    assert_eq!((w.stashes, w.changed), (1, 0), "{w:?}");
}

/// THE PERSON'S GLOBAL EXCLUDES HOLD ([`probe_with`]'s status read): a file
/// their global `core.excludesFile` names (`~/` expanded), or Git's default
/// `<XDG_CONFIG_HOME>/git/ignore` — `<home>/.config/git/ignore` where that
/// is unset — ignores is no untracked work, as their own `git status` says.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_persons_global_excludes_hold() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    std::fs::write(work.join(".DS_Store"), "x").expect("ds");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let probe_at = |xdg: Option<&Path>| finished(probe_with(&work, Some(&home), xdg, PROBE_LIMIT));
    let w = probe_at(None).expect("untracked");
    assert_eq!(
        (w.untracked, w.paths.as_slice()),
        (1, &[".DS_Store".to_string()][..])
    );
    std::fs::write(home.join("my-ignore"), ".DS_Store\n").expect("ignore");
    std::fs::write(
        home.join(".gitconfig"),
        "[core]\n\texcludesFile = ~/my-ignore\n",
    )
    .expect("gitconfig");
    assert_eq!(probe_at(None), None, "core.excludesFile");
    std::fs::remove_file(home.join(".gitconfig")).expect("rm");
    assert!(probe_at(None).is_some(), "the configuration gone");
    let xdg = tmp.path().join("xdg");
    std::fs::create_dir_all(xdg.join("git")).expect("xdg");
    std::fs::write(xdg.join("git/ignore"), ".DS_Store\n").expect("xdg ignore");
    assert_eq!(probe_at(Some(&xdg)), None, "XDG_CONFIG_HOME's");
    std::fs::create_dir_all(home.join(".config/git")).expect("config");
    std::fs::write(home.join(".config/git/ignore"), ".DS_Store\n").expect("home ignore");
    assert_eq!(probe_at(None), None, "the home's");
    std::fs::write(work.join("notes.md"), "x").expect("notes");
    let w = probe_at(None).expect("notes");
    assert_eq!(
        (w.untracked, w.paths.as_slice()),
        (1, &["notes.md".to_string()][..])
    );
}

/// THE REPOSITORY'S OWN EXCLUDES WIN OVER THE PERSON'S ([`own_excludes`]),
/// as in their own `git status`: a repository whose local configuration sets
/// `core.excludesFile` has that file read, and the person's global one not —
/// so what only its own file ignores is no work, and what only theirs
/// ignores is.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_repositorys_own_excludes_win_over_the_persons() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    std::fs::write(work.join(".DS_Store"), "x").expect("ds");
    std::fs::write(work.join("local.tmp"), "x").expect("local");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join("my-ignore"), ".DS_Store\n").expect("ignore");
    std::fs::write(
        home.join(".gitconfig"),
        "[core]\n\texcludesFile = ~/my-ignore\n",
    )
    .expect("gitconfig");
    let own = tmp.path().join("repo-ignore");
    std::fs::write(&own, "local.tmp\n").expect("own ignore");
    let untracked = || {
        finished(probe_with(&work, Some(&home), None, PROBE_LIMIT))
            .map(|w| w.paths)
            .unwrap_or_default()
    };
    assert_eq!(untracked(), ["local.tmp"]);
    let own = own.display().to_string();
    scratch_git(&work, &["config", "core.excludesFile", &own]);
    assert_eq!(untracked(), [".DS_Store"], "the repository's own file");
}

/// THE PERSON'S EXCLUDES SET THROUGH AN INCLUDE HOLD ([`global_excludes`]
/// reads with `--includes`): a global configuration whose `[include]` names
/// the file that sets `core.excludesFile` ignores what that file names, as
/// their own `git status` does.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_persons_excludes_set_through_an_include_hold() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    std::fs::write(work.join(".DS_Store"), "x").expect("ds");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join("my-ignore"), ".DS_Store\n").expect("ignore");
    std::fs::write(
        home.join("extra.gitconfig"),
        "[core]\n\texcludesFile = ~/my-ignore\n",
    )
    .expect("extra");
    let probe_at = || finished(probe_with(&work, Some(&home), None, PROBE_LIMIT));
    std::fs::write(
        home.join(".gitconfig"),
        "[include]\n\tpath = ~/extra.gitconfig\n",
    )
    .expect("gitconfig");
    assert_eq!(probe_at(), None, "the included excludes hold");
    std::fs::write(home.join(".gitconfig"), "[user]\n\tname = A\n").expect("gitconfig");
    assert_eq!(
        probe_at().map(|w| w.paths),
        Some(vec![".DS_Store".to_string()])
    );
}

/// THE PERSON'S EXCLUDES SET THROUGH AN `includeIf "gitdir:"` HOLD
/// ([`global_excludes`] reads in the repository, so the condition is judged
/// against it): a global configuration that includes the file setting
/// `core.excludesFile` only for repositories under one folder ignores what
/// that file names there, as their own `git status` does.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_persons_excludes_set_for_this_repository_by_an_include_if_hold() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    std::fs::write(work.join(".DS_Store"), "x").expect("ds");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join("my-ignore"), ".DS_Store\n").expect("ignore");
    std::fs::write(
        home.join("extra.gitconfig"),
        "[core]\n\texcludesFile = ~/my-ignore\n",
    )
    .expect("extra");
    let gitdir = |dir: &Path| {
        let dir = std::fs::canonicalize(dir).expect("canonical");
        format!(
            "[includeIf \"gitdir:{}/\"]\n\tpath = ~/extra.gitconfig\n",
            dir.display()
        )
    };
    std::fs::write(home.join(".gitconfig"), gitdir(&work)).expect("gitconfig");
    let probe_at = || finished(probe_with(&work, Some(&home), None, PROBE_LIMIT));
    assert_eq!(probe_at(), None, "the conditional excludes hold");
    std::fs::write(home.join(".gitconfig"), gitdir(&home)).expect("gitconfig");
    assert_eq!(
        probe_at().map(|w| w.paths),
        Some(vec![".DS_Store".to_string()])
    );
}

/// A REPOSITORY'S OWN `~/` PATH IS READ UNDER THE PERSON'S HOME: Git expands
/// `~` in a path value (here the repository's own `core.excludesFile =
/// ~/…`) with `HOME`, and a probe's `git` with none dies on it (`fatal:
/// failed to expand user dir`) at its first read, so the probe found nothing
/// and a dirty repository could land.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_repositorys_own_home_relative_path_is_read_under_the_persons_home() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    std::fs::write(work.join("local.tmp"), "x").expect("local");
    std::fs::write(work.join("notes.md"), "x").expect("notes");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join("repo-ignore"), "local.tmp\n").expect("ignore");
    scratch_git(&work, &["config", "core.excludesFile", "~/repo-ignore"]);
    let w =
        finished(probe_with(&work, Some(&home), None, PROBE_LIMIT)).expect("the untracked file");
    assert_eq!(w.paths, ["notes.md"], "{w:?}");
    std::fs::write(
        home.join(".gitconfig"),
        "[core]\n\texcludesFile = ~/no-such-file\n[status]\n\tshowUntrackedFiles = no\n",
    )
    .expect("gitconfig");
    let w = finished(probe_with(&work, Some(&home), None, PROBE_LIMIT)).expect("still read");
    assert_eq!(w.paths, ["notes.md"], "{w:?}");
}

/// THE PROBE COUNTS WHAT THE REMOTE'S DEFAULT BRANCH LACKS (Autopilot Land:
/// all of the work in the remote main, no exceptions): a commit pushed only
/// to a feature branch is no unpushed commit, and it is unlanded, named
/// against `origin/main`; merged into the remote main (a fast-forward push)
/// nothing is found.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_commit_pushed_only_to_a_feature_branch_is_unlanded_until_merged() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    assert_eq!(found(&work), None, "clean and landed");
    scratch_git(&work, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(work.join("b.txt"), "two\n").expect("b.txt");
    scratch_git(&work, &["add", "b.txt"]);
    scratch_git(&work, &["commit", "-q", "-m", "two"]);
    scratch_git(&work, &["push", "-q", "origin", "HEAD:feature"]);
    let w = found(&work).expect("pushed to a feature branch alone");
    assert_eq!(
        (w.changed, w.untracked, w.stashes, w.unpushed, w.unlanded),
        (0, 0, 0, 0, 1),
        "{w:?}"
    );
    assert_eq!(w.main_ref, "origin/main");
    assert!(
        w.summary().ends_with(": 1 commit not on origin/main"),
        "{}",
        w.summary()
    );
    scratch_git(&work, &["checkout", "-q", "main"]);
    assert_eq!(found(&work), None, "another branch is not HEAD");
    scratch_git(&work, &["checkout", "-q", "feature"]);
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&work), None, "merged into origin/main");
}

/// THE REMOTE'S DEFAULT BRANCH IS `origin/HEAD`'S TARGET FIRST, then
/// `origin/main`, then `origin/master`: a remote whose HEAD names `develop`
/// counts against `origin/develop` though `origin/main` holds the commit; a
/// remote with `master` alone counts against `origin/master`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_default_branch_is_origin_head_then_main_then_master_else_unknown() {
    let commit = |work: &Path, name: &str| {
        std::fs::write(work.join(name), "x\n").expect("file");
        scratch_git(work, &["add", name]);
        scratch_git(work, &["commit", "-q", "-m", name]);
    };
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    scratch_git(&work, &["push", "-q", "origin", "HEAD:develop"]);
    scratch_git(&work, &["remote", "set-head", "origin", "develop"]);
    commit(&work, "b.txt");
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    let w = found(&work).expect("not on origin/develop");
    assert_eq!((w.unpushed, w.unlanded), (0, 1), "{w:?}");
    assert_eq!(w.main_ref, "origin/develop");
    scratch_git(&work, &["remote", "set-head", "origin", "--delete"]);
    assert_eq!(found(&work), None, "origin/main holds it");
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), false);
    let bare = tmp.path().join("remote.git");
    std::fs::create_dir_all(&bare).expect("bare");
    scratch_git(&bare, &["init", "-q", "--bare", "."]);
    scratch_git(
        &work,
        &["remote", "add", "origin", &bare.display().to_string()],
    );
    scratch_git(&work, &["push", "-q", "origin", "HEAD:master"]);
    commit(&work, "b.txt");
    scratch_git(&work, &["push", "-q", "origin", "HEAD:feature"]);
    let w = found(&work).expect("not on origin/master");
    assert_eq!((w.unpushed, w.unlanded), (0, 1), "{w:?}");
    assert_eq!(w.main_ref, "origin/master");
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), false);
    let bare = tmp.path().join("remote.git");
    std::fs::create_dir_all(&bare).expect("bare");
    scratch_git(&bare, &["init", "-q", "--bare", "."]);
    scratch_git(
        &work,
        &["remote", "add", "origin", &bare.display().to_string()],
    );
    scratch_git(&work, &["push", "-q", "origin", "HEAD:trunk"]);
    commit(&work, "b.txt");
    scratch_git(&work, &["push", "-q", "origin", "HEAD:feature"]);
    assert_eq!(found(&work), None, "no default branch: unknown");
}

/// Directory admission resolves aliases before deciding whether the probe
/// may start Git.
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
            removable: false,
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
    assert_eq!(probe(&docs, Some(&home), PROBE_LIMIT), RepoRead::Clean);
    assert_eq!(unattended_dir(&work, None, &table), None);
    assert_eq!(probe(&work, None, PROBE_LIMIT), RepoRead::Clean);
    assert_eq!(
        probe(Path::new("/Volumes/unasked"), None, PROBE_LIMIT),
        RepoRead::Clean
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
            removable: false,
            ..table.mounts[0].clone()
        });
        assert_eq!(unattended_dir(&work, Some(&home), &unavailable), None);
    }
}

#[test]
fn the_session_works_in_the_transcript_s_last_cwd_else_where_it_started() {
    // Absolute as this platform reads a path: Windows roots one at a drive.
    let abs = |unix: &str| {
        if cfg!(windows) {
            format!("C:{unix}")
        } else {
            unix.to_owned()
        }
    };
    let (started, moved) = (abs("/Users//a/aterm"), abs("/Users//a/Developer/H-wt"));
    let tail = format!(
        "{{\"type\":\"user\",\"cwd\":\"{started}\"}}\n\
         {{\"type\":\"assistant\",\"cwd\":\"{moved}\"}}\n\
         {{\"type\":\"summary\"}}\n\
         {{\"cwd\":\"{}\n",
        abs("/Users//a/Developer/cut-off")
    );
    assert_eq!(
        session_dir(&started, Some(&tail)),
        Some(PathBuf::from(&moved))
    );
    assert_eq!(
        session_dir(&started, Some("{\"cwd\":\"relative\"}\n")),
        Some(PathBuf::from(&started))
    );
    assert_eq!(session_dir(&started, None), Some(PathBuf::from(&started)));
    assert_eq!(session_dir("", None), None);
    assert_eq!(session_dir("relative", Some("no json\n")), None);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn the_works_state_tells_apart_work_the_counts_cannot() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    std::fs::write(work.join("a.txt"), "two\n").expect("edit");
    let first = found(&work).expect("changed");
    assert!(first.state.is_some(), "{first:?}");
    let again = found(&work).expect("changed");
    assert!(
        RepoWork::same(Some(&first), Some(&again)),
        "{first:?} {again:?}"
    );
    std::fs::write(work.join("a.txt"), "two, then three\n").expect("edit");
    let edited = found(&work).expect("changed");
    assert_eq!(
        (edited.changed, edited.untracked, &edited.paths),
        (first.changed, first.untracked, &first.paths)
    );
    assert!(!RepoWork::same(Some(&first), Some(&edited)), "{edited:?}");
    std::fs::create_dir_all(work.join("sub")).expect("sub");
    std::fs::write(work.join("sub/one.rs"), "fn one() {}\n").expect("one");
    let one = found(&work).expect("untracked");
    std::fs::write(work.join("sub/two.rs"), "fn two() {}\n").expect("two");
    let two = found(&work).expect("untracked");
    assert_eq!((one.untracked, &one.paths), (two.untracked, &two.paths));
    assert!(!RepoWork::same(Some(&one), Some(&two)), "{two:?}");
    let unread = RepoWork {
        state: None,
        ..two.clone()
    };
    assert!(!RepoWork::same(Some(&unread), Some(&unread.clone())));
    assert!(!RepoWork::same(Some(&two), None));
    assert!(RepoWork::same(None, None));
}

#[test]
fn the_state_reads_the_listing_by_its_entries() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    std::fs::write(root.join("new.rs"), "x\n").expect("new");
    let listed = "R  new.rs\0old.rs\0 D gone.rs\0";
    let state = state_of(root, "abc", "", listed).expect("read");
    assert_eq!(
        Some(state),
        state_of(root, "abc", "", "R  new.rs\0older.rs\0 D gone.rs\0")
    );
    assert_ne!(
        Some(state),
        state_of(root, "abc", "", "R  new.rs\0old.rs\0"),
        "the deletion counts"
    );
    assert_ne!(
        Some(state),
        state_of(root, "def", "", listed),
        "another HEAD"
    );
    assert_ne!(
        Some(state),
        state_of(root, "abc", "wip", listed),
        "another stash"
    );
    let many: String = (0..=STATE_FILES).map(|k| format!("?? f{k}.rs\0")).collect();
    assert_eq!(state_of(root, "abc", "", &many), None);
    let enough: String = (0..STATE_FILES).map(|k| format!("?? f{k}.rs\0")).collect();
    assert!(state_of(root, "abc", "", &enough).is_some());
}

#[test]
fn repo_hygiene_summarises_drift_worktrees_and_stale_branches() {
    let wt = "worktree /repo\nHEAD 111\nbranch refs/heads/main\n\n\
              worktree /repo-wt\nHEAD 222\nbranch refs/heads/feat-a\n";
    let h = RepoHygiene::of_git(
        "origin/main",
        Some("feat-a"),
        "3\t2\n",
        wt,
        "feat-a\nfeat-b\n",
        "main\nfeat-a\nold-done\n",
    );
    assert_eq!((h.ahead, h.behind, h.worktrees), (2, 3, 2));
    assert_eq!(h.unmerged_branches, ["feat-a", "feat-b"]);
    assert_eq!(h.stale_branches, ["old-done"]);
    assert_eq!(
        h.notes(),
        [
            "\u{2191}2 ahead \u{b7} \u{2193}3 behind origin/main",
            "\u{2387}  2 worktrees \u{b7} 2 unmerged (feat-a, feat-b) \u{b7} 1 stale (old-done)",
        ]
    );

    let clean = RepoHygiene::of_git(
        "origin/main",
        Some("main"),
        "0\t0\n",
        "worktree /repo\n",
        "",
        "main\n",
    );
    assert_eq!(
        clean.notes(),
        [
            "\u{2713}  in sync with origin/main",
            "\u{2387}  1 worktree \u{b7} no stale branches",
        ]
    );

    let with_disk = h.with_local_and_disk(
        " M src/lib.rs\0?? notes.txt\0",
        "WIP on feat-a: 1a2b3c4 wip\n",
        1,
        512 * 1024 * 1024,
        2 * 1024 * 1024 * 1024,
        Some(120 * 1024 * 1024 * 1024),
    );
    assert_eq!(
        with_disk.notes(),
        [
            "\u{2191}2 ahead \u{b7} \u{2193}3 behind origin/main \u{b7} 1 changed, 1 untracked, 1 unpushed, 1 stash",
            "\u{2387}  2 worktrees \u{b7} 2 unmerged (feat-a, feat-b) \u{b7} 1 stale (old-done)",
            "\u{26c1}  worktrees 512.0 MiB \u{b7} build/temp 2.0 GiB \u{b7} 120.0 GiB free",
        ]
    );
    assert_eq!(
        with_disk.folder_notes(),
        [
            "\u{26c1}  2 worktrees \u{b7} worktrees 512.0 MiB \u{b7} build/temp 2.0 GiB \u{b7} 120.0 GiB free",
            "\u{2387}  1 changed \u{b7} 1 untracked \u{b7} 1 unpushed \u{b7} 1 stash",
        ]
    );
}
