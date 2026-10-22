// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::path::PathBuf;

/// A probe's deadline in these tests: a one-minute hang detector, never
/// [`PROBE_LIMIT`]'s latency budget — on a loaded host a scratch
/// repository's reads outrun ten seconds, and a probe cut short reads as
/// less work than the repository holds.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const HANG: Duration = Duration::from_secs(60);

/// `git <args>` in `dir`, hermetic: no system or global config, a fixed
/// author.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn scratch_git(dir: &Path, args: &[&str]) {
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("git")
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
    probe(dir, Some(dir), HANG)
}

/// THE READS' OUTPUT, PARSED ([`RepoWork::of_git`]): a tracked change
/// staged or not, a rename (its source field skipped), an untracked path,
/// stash lines, unpushed commits and commits not on the remote's default
/// branch each count; ignored paths do not; at most [`NAMED_PATHS`] paths
/// are named, each printable. NEGATIVE CONTROL: a clean status, no stash,
/// nothing unpushed and nothing unlanded is no finding, a default branch
/// named or not.
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
    // A path that would end a typed line is printed harmless.
    let w = RepoWork::of_git("/r", "?? evil\nname\0", "", 0, 0, "").expect("found");
    assert_eq!(w.paths, ["evil?name"]);
    assert_eq!(w.summary(), "/r: 1 untracked (evil?name)");
    let w = RepoWork::of_git("/r", "", "", 1, 0, "").expect("found");
    assert_eq!(w.summary(), "/r: 1 unpushed commit");
    // Commits pushed, but not to the remote's default branch.
    let w = RepoWork::of_git("/r", "", "", 0, 2, "origin/main").expect("found");
    assert_eq!((w.unpushed, w.unlanded), (0, 2));
    assert_eq!(w.summary(), "/r: 2 commits not on origin/main");
    let w = RepoWork::of_git("/r", "", "", 1, 1, "origin/master").expect("found");
    assert_eq!(
        w.summary(),
        "/r: 1 unpushed commit, 1 commit not on origin/master"
    );
    // NEGATIVE CONTROLS.
    assert_eq!(RepoWork::of_git("/r", "", "", 0, 0, ""), None);
    assert_eq!(RepoWork::of_git("/r", "", "", 0, 0, "origin/main"), None);
    assert_eq!(RepoWork::of_git("/r", "!! target/\0", "\n", 0, 0, ""), None);
    // Public findings accept the full count domain. A mixed finding must not
    // wrap to "nothing", and its path suffix still records omitted paths.
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

/// ONE BRANCH'S STASH ENTRIES ([`stashes_on`], over `git stash list
/// --format=%gs`): those Git wrote on the branch, a message given or not;
/// on a detached `HEAD`, those written with no branch. NEGATIVE CONTROLS:
/// another branch's entry, and one whose branch only begins like it
/// (`mainline` for `main`), are not counted.
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
    // NEGATIVE CONTROLS.
    assert_eq!(stashes_on(list, Some("other")), "");
    assert_eq!(stashes_on("", Some("main")), "");
}

/// A STASH ANOTHER WORKTREE MADE IS NONE OF THIS ONE'S: a repository's
/// stash is shared by its worktrees, and an entry made in another worktree,
/// on its own branch, is another session's work — the review found the
/// probe counted it. NEGATIVE CONTROLS: that worktree's probe finds it, and
/// a stash made here is found here.
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
    // NEGATIVE CONTROLS.
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
/// The review found `.DS_Store` read as untracked work, so Land never
/// landed. NEGATIVE CONTROLS: a home with no excludes finds it, and a file
/// the excludes do not name is found beside it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_persons_global_excludes_hold() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    std::fs::write(work.join(".DS_Store"), "x").expect("ds");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let probe_at = |xdg: Option<&Path>| probe_with(&work, Some(&home), xdg, HANG);
    // NEGATIVE CONTROL: no excludes.
    let w = probe_at(None).expect("untracked");
    assert_eq!(
        (w.untracked, w.paths.as_slice()),
        (1, &[".DS_Store".to_string()][..])
    );
    // `core.excludesFile` in the global configuration.
    std::fs::write(home.join("my-ignore"), ".DS_Store\n").expect("ignore");
    std::fs::write(
        home.join(".gitconfig"),
        "[core]\n\texcludesFile = ~/my-ignore\n",
    )
    .expect("gitconfig");
    assert_eq!(probe_at(None), None, "core.excludesFile");
    std::fs::remove_file(home.join(".gitconfig")).expect("rm");
    assert!(probe_at(None).is_some(), "the configuration gone");
    // Git's default, under XDG_CONFIG_HOME and under the home.
    let xdg = tmp.path().join("xdg");
    std::fs::create_dir_all(xdg.join("git")).expect("xdg");
    std::fs::write(xdg.join("git/ignore"), ".DS_Store\n").expect("xdg ignore");
    assert_eq!(probe_at(Some(&xdg)), None, "XDG_CONFIG_HOME's");
    std::fs::create_dir_all(home.join(".config/git")).expect("config");
    std::fs::write(home.join(".config/git/ignore"), ".DS_Store\n").expect("home ignore");
    assert_eq!(probe_at(None), None, "the home's");
    // NEGATIVE CONTROL: a file they do not ignore.
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
/// ignores is. The review found the probe's `-c` with the global file
/// override the repository's own. NEGATIVE CONTROL: with no local setting,
/// the global file holds and the repository's file is no excludes at all.
/// The person's global configuration is a scratch home's, never the real one.
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
        probe_with(&work, Some(&home), None, HANG)
            .map(|w| w.paths)
            .unwrap_or_default()
    };
    // NEGATIVE CONTROL: no local setting, the person's file holds.
    assert_eq!(untracked(), ["local.tmp"]);
    let own = own.display().to_string();
    scratch_git(&work, &["config", "core.excludesFile", &own]);
    assert_eq!(untracked(), [".DS_Store"], "the repository's own file");
}

/// THE PERSON'S EXCLUDES SET THROUGH AN INCLUDE HOLD ([`global_excludes`]
/// reads with `--includes`): a global configuration whose `[include]` names
/// the file that sets `core.excludesFile` ignores what that file names, as
/// their own `git status` does. The review found the read missed it.
/// NEGATIVE CONTROL: the include gone, the file is untracked work again.
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
    let probe_at = || probe_with(&work, Some(&home), None, HANG);
    std::fs::write(
        home.join(".gitconfig"),
        "[include]\n\tpath = ~/extra.gitconfig\n",
    )
    .expect("gitconfig");
    assert_eq!(probe_at(), None, "the included excludes hold");
    // NEGATIVE CONTROL: no include.
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
/// that file names there, as their own `git status` does. The recheck of
/// ea564676a3 found the read run from `/`, where no repository matches, so
/// the ignored file counted as untracked work for good. NEGATIVE CONTROL: a
/// condition naming another folder includes nothing, and the file is work.
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
    let probe_at = || probe_with(&work, Some(&home), None, HANG);
    assert_eq!(probe_at(), None, "the conditional excludes hold");
    // NEGATIVE CONTROL: the condition names another folder.
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
/// and a dirty repository could land. The recheck of ea564676a3 found it.
/// The probe's reads now run with the person's home as `HOME`, their
/// global configuration still shut out. NEGATIVE CONTROL: what the
/// repository's own file does not ignore is still found.
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
    let w = probe_with(&work, Some(&home), None, HANG).expect("the untracked file");
    assert_eq!(w.paths, ["notes.md"], "{w:?}");
    // The person's own global configuration is no part of the reads.
    std::fs::write(
        home.join(".gitconfig"),
        "[core]\n\texcludesFile = ~/no-such-file\n[status]\n\tshowUntrackedFiles = no\n",
    )
    .expect("gitconfig");
    let w = probe_with(&work, Some(&home), None, HANG).expect("still read");
    assert_eq!(w.paths, ["notes.md"], "{w:?}");
}

/// THE PROBE COUNTS WHAT THE REMOTE'S DEFAULT BRANCH LACKS (Autopilot Land:
/// all of the work in the remote main, no exceptions): a commit pushed only
/// to a feature branch is no unpushed commit, and it is unlanded, named
/// against `origin/main`; merged into the remote main (a fast-forward push)
/// nothing is found. NEGATIVE CONTROL: the same feature commit with HEAD
/// back on the landed main is no finding — only HEAD is read, never another
/// branch.
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
    // NEGATIVE CONTROL: HEAD on the landed main, the feature branch's
    // commit still unlanded on its own branch.
    scratch_git(&work, &["checkout", "-q", "main"]);
    assert_eq!(found(&work), None, "another branch is not HEAD");
    // Merged into the remote main: nothing.
    scratch_git(&work, &["checkout", "-q", "feature"]);
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&work), None, "merged into origin/main");
}

/// THE REMOTE'S DEFAULT BRANCH IS `origin/HEAD`'S TARGET FIRST, then
/// `origin/main`, then `origin/master`: a remote whose HEAD names `develop`
/// counts against `origin/develop` though `origin/main` holds the commit; a
/// remote with `master` alone counts against `origin/master`. NEGATIVE
/// CONTROL: a remote with none of them (`trunk` alone) leaves the count
/// unknown, and a commit pushed to its feature branch is no finding — as a
/// probe that cannot run finds nothing.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_default_branch_is_origin_head_then_main_then_master_else_unknown() {
    let commit = |work: &Path, name: &str| {
        std::fs::write(work.join(name), "x\n").expect("file");
        scratch_git(work, &["add", name]);
        scratch_git(work, &["commit", "-q", "-m", name]);
    };
    // origin/HEAD names develop: the commit on origin/main is unlanded.
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    scratch_git(&work, &["push", "-q", "origin", "HEAD:develop"]);
    scratch_git(&work, &["remote", "set-head", "origin", "develop"]);
    commit(&work, "b.txt");
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    let w = found(&work).expect("not on origin/develop");
    assert_eq!((w.unpushed, w.unlanded), (0, 1), "{w:?}");
    assert_eq!(w.main_ref, "origin/develop");
    // Without origin/HEAD, origin/main holds it: nothing.
    scratch_git(&work, &["remote", "set-head", "origin", "--delete"]);
    assert_eq!(found(&work), None, "origin/main holds it");
    // A remote with master alone.
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
    // NEGATIVE CONTROL: trunk alone, no default branch known.
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
    assert_eq!(probe(&docs, Some(&home), HANG), None);
    assert_eq!(unattended_dir(&work, None, &table), None);
    assert_eq!(probe(&work, None, HANG), None);
    assert_eq!(probe(Path::new("/Volumes/unasked"), None, HANG), None);
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

/// THE WORK'S STATE TELLS APART WORK THE COUNTS CANNOT ([`RepoWork::state`],
/// [`RepoWork::same`]): an edit to a file already changed, and a new file in
/// a directory already untracked, leave the counts and the named paths as
/// they were, and only the state moves — the review of b8/residuals found a
/// wake's edit to an already changed path compared equal on the counts and
/// forgiven as someone else's. NEGATIVE CONTROLS: the same work probed
/// twice is the same; a finding with no state read is never the same as
/// anything, itself included; two probes that found nothing are.
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
    // The same file edited again: the same counts and paths.
    std::fs::write(work.join("a.txt"), "two, then three\n").expect("edit");
    let edited = found(&work).expect("changed");
    assert_eq!(
        (edited.changed, edited.untracked, &edited.paths),
        (first.changed, first.untracked, &first.paths)
    );
    assert!(!RepoWork::same(Some(&first), Some(&edited)), "{edited:?}");
    // A new file in a directory already untracked: still `sub/`, one entry.
    std::fs::create_dir_all(work.join("sub")).expect("sub");
    std::fs::write(work.join("sub/one.rs"), "fn one() {}\n").expect("one");
    let one = found(&work).expect("untracked");
    std::fs::write(work.join("sub/two.rs"), "fn two() {}\n").expect("two");
    let two = found(&work).expect("untracked");
    assert_eq!((one.untracked, &one.paths), (two.untracked, &two.paths));
    assert!(!RepoWork::same(Some(&one), Some(&two)), "{two:?}");
    // NEGATIVE CONTROLS.
    let unread = RepoWork {
        state: None,
        ..two.clone()
    };
    assert!(!RepoWork::same(Some(&unread), Some(&unread.clone())));
    assert!(!RepoWork::same(Some(&two), None));
    assert!(RepoWork::same(None, None));
}

/// [`state_of`] OVER `git status -z`: a rename's source field is no entry of
/// its own, a deleted path hashes as no file, and past [`STATE_FILES`]
/// entries the state is not read. NEGATIVE CONTROL: another `HEAD` or
/// another stash is another state over the same listing.
#[test]
fn the_state_reads_the_listing_by_its_entries() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    std::fs::write(root.join("new.rs"), "x\n").expect("new");
    let listed = "R  new.rs\0old.rs\0 D gone.rs\0";
    let state = state_of(root, "abc", "", listed).expect("read");
    // The rename's source is its own field, no entry of its own: another
    // source reads the same.
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
