// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::path::PathBuf;

/// `git <args>` in `dir`, hermetic: no system or global config, a fixed
/// author; its output.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn scratch_git(dir: &Path, args: &[&str]) -> String {
    scratch_git_dated(dir, None, args)
}

/// [`scratch_git`] with its commits and reflog entries dated `when` (Unix seconds), where given.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn scratch_git_dated(dir: &Path, when: Option<u64>, args: &[&str]) -> String {
    let mut cmd = aterm_uds::spawnfd::command(crate::test_program::git());
    if let Some(exec_path) = crate::test_program::git_exec_path() {
        cmd.env("GIT_EXEC_PATH", exec_path);
    }
    if let Some(when) = when {
        let stamp = format!("@{when} +0000");
        cmd.env("GIT_COMMITTER_DATE", &stamp)
            .env("GIT_AUTHOR_DATE", &stamp);
    }
    let out = aterm_uds::deadline::output_until(
        cmd.args([
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
    String::from_utf8_lossy(&out.stdout).into_owned()
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
    assert!(!read.is_unread(), "every read ended within its limit");
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

/// `text` written to `name` in `work`, committed and pushed to its `origin`; the commit.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn pushed(work: &Path, name: &str, text: &str) -> String {
    std::fs::write(work.join(name), text).expect("write");
    scratch_git(work, &["add", name]);
    scratch_git(work, &["commit", "-q", "-m", name]);
    scratch_git(work, &["push", "-q", "origin", "HEAD:main"]);
    scratch_git(work, &["rev-parse", "HEAD"]).trim().to_string()
}

/// A SUBMODULE WHOSE CHECKOUT ONLY LAGS ITS RECORD IS NO WORK: the one commit its entry asks for
/// would record the older commit (on aterm's shared checkout, after a `git pull` that did not
/// recurse, `vendor/astream` 535 commits back). At a commit this checkout recorded for it at a
/// point its `HEAD` moved through, its record fetched into it or not, the probe and the hygiene
/// read count nothing; an edit inside it, a bump not yet committed, a branch switched to (its
/// commit on a remote, the record never fetched), and an unpushed commit in it each stay work.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_submodule_checkout_that_only_lags_its_record_is_no_work() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (lib_dir, app_dir) = (tmp.path().join("lib"), tmp.path().join("app"));
    std::fs::create_dir_all(&lib_dir).expect("lib");
    std::fs::create_dir_all(&app_dir).expect("app");
    let lib = repo(&lib_dir, true);
    let app = repo(&app_dir, true);
    let url = lib_dir.join("remote.git").display().to_string();
    scratch_git(
        &app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &url,
            "vendor/lib",
        ],
    );
    scratch_git(&app, &["commit", "-q", "-m", "lib"]);
    scratch_git(&app, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&app), None, "the submodule at its record");
    let sub = app.join("vendor/lib");
    let first = scratch_git(&sub, &["rev-parse", "HEAD"]).trim().to_string();
    let lagging = |why: &str| {
        assert_eq!(found(&app), None, "{why}");
        let hygiene = probe_hygiene(&app, Some(&app), PROBE_LIMIT).expect("hygiene");
        assert_eq!(
            (hygiene.changed, hygiene.untracked),
            (0, 0),
            "{why}: {hygiene:?}"
        );
    };
    let work = |why: &str| {
        let w = found(&app).unwrap_or_else(|| panic!("{why}"));
        assert_eq!(
            (w.changed, w.paths.as_slice()),
            (1, &["vendor/lib".to_string()][..]),
            "{why}: {w:?}"
        );
        let hygiene = probe_hygiene(&app, Some(&app), PROBE_LIMIT).expect("hygiene");
        assert_eq!(hygiene.changed, 1, "{why}: {hygiene:?}");
    };

    pushed(&lib, "b.txt", "two\n");
    scratch_git(&sub, &["fetch", "-q", "origin"]);
    scratch_git(&sub, &["checkout", "-q", "origin/main"]);
    work("a bump not yet committed");
    scratch_git(&app, &["commit", "-q", "-am", "lib two"]);
    scratch_git(&app, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&app), None, "the bump committed and pushed");
    scratch_git(&sub, &["checkout", "-q", &first]);
    lagging("a checkout behind its record");
    std::fs::write(sub.join("a.txt"), "edited\n").expect("edit");
    work("an edit inside a lagging submodule");
    scratch_git(&sub, &["checkout", "-q", "--", "a.txt"]);
    lagging("the edit undone");

    let unfetched = pushed(&lib, "c.txt", "three\n");
    scratch_git(
        &app,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{unfetched},vendor/lib"),
        ],
    );
    scratch_git(&app, &["commit", "-q", "-m", "lib three"]);
    scratch_git(&app, &["push", "-q", "origin", "HEAD:main"]);
    lagging("a published checkout whose record it never fetched");
    scratch_git(&lib, &["checkout", "-q", "-b", "feature", &first]);
    std::fs::write(lib.join("e.txt"), "five\n").expect("e.txt");
    scratch_git(&lib, &["add", "e.txt"]);
    scratch_git(&lib, &["commit", "-q", "-m", "feature"]);
    scratch_git(&lib, &["push", "-q", "origin", "feature"]);
    scratch_git(&sub, &["fetch", "-q", "origin", "feature"]);
    scratch_git(&sub, &["checkout", "-q", "FETCH_HEAD"]);
    work("a branch switched to, its record never fetched");
    scratch_git(&sub, &["checkout", "-q", &first]);
    lagging("back at a commit it recorded");
    std::fs::write(sub.join("d.txt"), "four\n").expect("d.txt");
    scratch_git(&sub, &["add", "d.txt"]);
    scratch_git(&sub, &["commit", "-q", "-m", "local"]);
    work("an unpushed commit in a submodule whose record it never fetched");
}

/// A SUBMODULE A MERGE MOVED PAST IS NO WORK: this checkout bumps it to `t2` while upstream bumps
/// it to `t3`, and a merge that does not recurse fast-forwards the record to `t3` and leaves the
/// checkout at `t2`. History simplification hides `t2` from a `git log -- <path>` of the merge
/// (it follows upstream, whose record the merge kept); the checkout's own `HEAD` held it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_submodule_checkout_a_merge_moved_past_is_no_work() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (lib_dir, app_dir) = (tmp.path().join("lib"), tmp.path().join("app"));
    std::fs::create_dir_all(&lib_dir).expect("lib");
    std::fs::create_dir_all(&app_dir).expect("app");
    let lib = repo(&lib_dir, true);
    let app = repo(&app_dir, true);
    let url = lib_dir.join("remote.git").display().to_string();
    scratch_git(
        &app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &url,
            "vendor/lib",
        ],
    );
    scratch_git(&app, &["commit", "-q", "-m", "lib"]);
    let t2 = pushed(&lib, "b.txt", "two\n");
    let t3 = pushed(&lib, "c.txt", "three\n");
    let sub = app.join("vendor/lib");
    scratch_git(&sub, &["fetch", "-q", "origin"]);
    scratch_git(&app, &["checkout", "-q", "-b", "up"]);
    scratch_git(&sub, &["checkout", "-q", &t3]);
    scratch_git(&app, &["commit", "-q", "-am", "up: lib three"]);
    scratch_git(&app, &["checkout", "-q", "main"]);
    scratch_git(&sub, &["checkout", "-q", &t2]);
    scratch_git(&app, &["commit", "-q", "-am", "lib two"]);
    scratch_git(
        &app,
        &[
            "-c",
            "submodule.recurse=false",
            "merge",
            "-q",
            "--no-edit",
            "up",
        ],
    );
    scratch_git(&app, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(
        scratch_git(&app, &["rev-parse", "HEAD:vendor/lib"]).trim(),
        t3,
        "the merge records upstream's bump"
    );
    assert_eq!(
        scratch_git(&app, &["status", "--porcelain"]),
        " M vendor/lib\n",
        "the checkout stays at this checkout's own bump"
    );
    assert_eq!(found(&app), None);
    let hygiene = probe_hygiene(&app, Some(&app), PROBE_LIMIT).expect("hygiene");
    assert_eq!((hygiene.changed, hygiene.untracked), (0, 0), "{hygiene:?}");
}

/// `<tmp>/app/work` with `<tmp>/lib/work`'s bare origin added at `vendor/lib` over `file://`,
/// committed and pushed: `(lib, app)`. Neither sets `submodule.recurse`, git's default.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn app_with_lib(tmp: &Path) -> (PathBuf, PathBuf) {
    let (lib_dir, app_dir) = (tmp.join("lib"), tmp.join("app"));
    std::fs::create_dir_all(&lib_dir).expect("lib");
    std::fs::create_dir_all(&app_dir).expect("app");
    let lib = repo(&lib_dir, true);
    let app = repo(&app_dir, true);
    let url = format!("file://{}", lib_dir.join("remote.git").display());
    scratch_git(
        &app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &url,
            "vendor/lib",
        ],
    );
    scratch_git(&app, &["commit", "-q", "-m", "lib"]);
    scratch_git(&app, &["push", "-q", "origin", "HEAD:main"]);
    (lib, app)
}

/// `sweep`, given what `probe` reads, with this thread's reads memoized ([`read_memo`]), for a
/// sweep that probes a repository it leaves as it is: a read a probe before it made is answered
/// from memory and starts no `git` again. First `probe` reads alike with the memo off, while
/// filling it, and answered from it under [`read_memo::Memo::checking`]: every read of that probe
/// is one the memo answers, and each answer is the output its `git` gives again, read by read,
/// whether or not it changes the reading. Once the sweep ends, a probe with the memo off reads as
/// the first: the sweep starved nothing and left the repository reading as it found it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[track_caller]
fn memoized<T>(probe: impl Fn() -> RepoRead, sweep: impl FnOnce(&RepoRead) -> T) -> T {
    let real = probe();
    let memo = read_memo::on();
    let filled = probe();
    assert_eq!(filled, real, "a probe filling the memo");
    let missed = memo.missed();
    let answered = memo.checking(&probe);
    assert_eq!(answered, real, "a probe the memo answered");
    assert_eq!(
        memo.missed(),
        missed,
        "a probe the memo answered: every read answered from it"
    );
    let swept = sweep(&real);
    drop(memo);
    assert_eq!(
        probe(),
        real,
        "after the sweep, with the memo off: nothing starved, the repository read as before it"
    );
    swept
}

/// THE READ MEMO ([`read_memo`]) KNOWS A READ BY ITS WHOLE KEY: with no memo held a read runs its
/// `git` every time; held, a read runs once, counted as missed, and the same read is then answered
/// with that output without running, while a read in another folder or fed other input runs its
/// own, and one a signal ended is not kept. The key reads a fed input without moving the file's
/// offset, so the `git` it feeds still reads all of it. Under [`read_memo::Memo::checking`] an
/// answer runs its `git` again. While a memo is held a second is refused, and the one held still
/// answers. NEGATIVE CONTROLS: a checked answer its `git` no longer gives panics; a dropped memo
/// answers nothing.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_read_memo_knows_a_read_by_its_whole_key() {
    use read_memo::{Read, through};
    use std::io::Read as _;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{ExitStatus, Output};
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let reads = Reads {
        deadline: Cell::new(Instant::now() + PROBE_LIMIT),
        each: Some(PROBE_LIMIT),
        home: tmp.path(),
        xdg: None,
        cut: Cell::new(false),
        starts: Cell::new(0),
        held: None,
        refused: None,
    };
    let read = |dir: &str, feed: Option<&std::fs::File>| {
        read_memo::read(Path::new(dir), &["status"], &reads, false, true, feed)
    };
    let said = |text: &str| Output {
        status: ExitStatus::from_raw(0),
        stdout: text.into(),
        stderr: Vec::new(),
    };
    let runs = Cell::new(0_u32);
    let run = |output: Output| {
        runs.set(runs.get().saturating_add(1));
        Ok::<_, ()>(output)
    };
    let not_run = || -> Result<Output, ()> { panic!("a read the memo holds ran its git") };
    for _ in 0..2 {
        assert_eq!(through(read("/a", None), || run(said("a"))), Ok(said("a")));
    }
    assert_eq!(runs.get(), 2, "with no memo held, every read runs its git");
    let memo = read_memo::on();
    assert_eq!(through(read("/a", None), || run(said("a"))), Ok(said("a")));
    assert_eq!(
        through(read("/a", None), not_run),
        Ok(said("a")),
        "the same read, answered from memory"
    );
    assert_eq!(
        through(read("/b", None), || run(said("b"))),
        Ok(said("b")),
        "a read in another folder runs its own"
    );
    let one = open_git_stdin_feed(&tmp.path().join("one"), b"one\n").expect("one");
    let two = open_git_stdin_feed(&tmp.path().join("two"), b"two\n").expect("two");
    assert_eq!(
        through(read("/a", Some(&one)), || run(said("1"))),
        Ok(said("1"))
    );
    assert_eq!(
        through(read("/a", Some(&two)), || run(said("2"))),
        Ok(said("2")),
        "a read fed other input runs its own"
    );
    assert_eq!(
        through(read("/a", Some(&one)), not_run),
        Ok(said("1")),
        "a fed read, answered from memory"
    );
    let mut fed = String::new();
    (&one).read_to_string(&mut fed).expect("one");
    assert_eq!(
        fed, "one\n",
        "the key read the input without moving the offset"
    );
    let ended = Output {
        status: ExitStatus::from_raw(9),
        ..said("")
    };
    assert_eq!(through(read("/k", None), || run(ended.clone())), Ok(ended));
    assert_eq!(
        through(read("/k", None), || run(said("k"))),
        Ok(said("k")),
        "a read a signal ended is not kept"
    );
    assert_eq!(
        runs.get(),
        8,
        "each read the memo could not answer ran its git"
    );
    assert_eq!(memo.missed(), 6, "the reads the memo could not answer");
    assert_eq!(
        memo.checking(|| through(read("/a", None), || run(said("a")))),
        Ok(said("a"))
    );
    assert_eq!(runs.get(), 9, "a checked answer runs its git again");
    let differs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        memo.checking(|| through(read("/a", None), || run(said("not a"))))
    }));
    assert!(differs.is_err(), "a checked answer its git no longer gives");
    assert_eq!(
        through(read("/a", None), not_run),
        Ok(said("a")),
        "past the checks, answered without running"
    );
    assert_eq!(memo.missed(), 6, "a checked answer is no miss");
    let nested = std::panic::catch_unwind(read_memo::on);
    assert!(nested.is_err(), "a second memo on this thread is refused");
    assert_eq!(
        through(read("/a", None), not_run),
        Ok(said("a")),
        "the memo held still answers"
    );
    drop(memo);
    assert!(
        matches!(read("/a", None), Read::Off),
        "a dropped memo answers nothing"
    );
}

/// Every read of a probe of `dir` cut in turn ([`read_stall`], on the probe's clock), as far as the
/// first probe that ends before its cut: none reads clean, `what` naming the work it holds. The sweep
/// runs [`memoized`], so each read the memo answers gives the same output on every run. THE GAP: a
/// read it cannot answer (one whose fed input it could not read back) may still make one run read
/// fewer than the next, which stops the sweep there, and the reads only the longer run takes are
/// not cut.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn never_clean_when_cut(dir: &Path, used: &Used, what: &str) {
    let probe_dir = || probe_session(dir, Some(dir), used, Duration::from_secs(10));
    memoized(probe_dir, |_| {
        for k in 0..40 {
            read_stall::set_after(k, 11_000);
            let read = probe_dir();
            let past_the_last_read = read_stall::pending();
            read_stall::set(0);
            assert!(
                !read.is_clean(),
                "read {k} cut: {read:?} over {what} read as clean"
            );
            if past_the_last_read {
                break;
            }
        }
    });
}

/// A SUBMODULE'S COMMIT ON NO REMOTE IS WORK (D2-1): a commit made in `vendor/lib` and recorded
/// by a bump the superproject pushed, but never pushed to the submodule's own remote, is one
/// nobody cloning the remote main can fetch, though `git status` shows nothing; the probe counts
/// it and names the submodule, and a probe cut at any read over it is never clean. NEGATIVE
/// CONTROL: the submodule at its first record, and the same bump once its commit is pushed, read
/// clean.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_submodule_commit_on_no_remote_is_work() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (_lib, app) = app_with_lib(tmp.path());
    assert_eq!(found(&app), None, "the submodule at a pushed record");
    let sub = app.join("vendor/lib");
    std::fs::write(sub.join("d.txt"), "four\n").expect("d.txt");
    scratch_git(&sub, &["add", "d.txt"]);
    scratch_git(&sub, &["commit", "-q", "-m", "local"]);
    scratch_git(&app, &["add", "vendor/lib"]);
    scratch_git(&app, &["commit", "-q", "-m", "bump"]);
    scratch_git(&app, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(scratch_git(&app, &["status", "--porcelain"]), "");
    let w = found(&app).expect("the submodule's commit on no remote");
    assert_eq!(
        (w.changed, w.untracked, w.stashes, w.unpushed, w.unlanded),
        (0, 0, 0, 0, 0),
        "{w:?}"
    );
    assert!(
        w.summary().ends_with(": 1 unpushed commit in vendor/lib"),
        "{}",
        w.summary()
    );
    never_clean_when_cut(&app, &Used::default(), "the submodule's commit");
    scratch_git(&sub, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&app), None, "the submodule's commit pushed");
}

/// A submodule's silent omission of a broken remote ref proves no clean repository. The
/// superproject's status stays clean, but `rev-list --ignore-missing` silently drops the broken
/// ref. Controls: the same pushed submodule before and after the broken ref reads checked, and
/// an index record the submodule has not fetched is still optional to its own commit count;
/// a present non-commit record is unread, never treated as a missing commit.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_submodule_count_that_ignores_a_broken_ref_is_not_clean() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (_lib, app) = app_with_lib(tmp.path());
    let read = || probe(&app, Some(&app), PROBE_LIMIT);
    assert!(read().checked().is_some(), "the pushed submodule");
    let sub = app.join("vendor/lib");
    let bad = scratch_git(
        &sub,
        &["rev-parse", "--git-path", "refs/remotes/origin/broken"],
    );
    let bad = sub.join(bad.trim());
    std::fs::write(&bad, "invalid-ref-sentinel\n").expect("the broken ref");
    assert_eq!(scratch_git(&app, &["status", "--porcelain"]), "");
    assert_eq!(read(), RepoRead::Unread, "the omitted submodule reference");
    std::fs::remove_file(&bad).expect("remove the broken ref");
    assert!(
        read().checked().is_some(),
        "the same complete references again"
    );

    let missing = "0000000000000000000000000000000000000001";
    scratch_git(
        &app,
        &[
            "update-index",
            "--cacheinfo",
            "160000",
            missing,
            "vendor/lib",
        ],
    );
    // The public probe resolves its root before entering this helper; `/tmp` aliases
    // `/private/tmp` on macOS, so supply the same canonical root here.
    let canonical = app.canonicalize().expect("the canonical project root");
    let reads = Reads {
        deadline: Cell::new(Instant::now() + PROBE_LIMIT),
        each: Some(PROBE_LIMIT),
        home: &canonical,
        xdg: None,
        cut: Cell::new(false),
        starts: Cell::new(0),
        held: None,
        refused: None,
    };
    let count = submodules_unpushed(&canonical, &aterm_sysprobe::volumes::table(), &reads)
        .expect("an unfetched record is optional, the checkout still read strictly");
    assert_eq!(count.count, 0, "the checkout's commit is pushed");
    assert!(!reads.cut.get(), "no ref or checkout query was omitted");

    let blob = scratch_git(&sub, &["rev-parse", "HEAD:a.txt"]);
    scratch_git(
        &app,
        &[
            "update-index",
            "--cacheinfo",
            "160000",
            blob.trim(),
            "vendor/lib",
        ],
    );
    assert!(
        submodules_unpushed(&canonical, &aterm_sysprobe::volumes::table(), &reads).is_none(),
        "a present blob is no missing commit"
    );
}

/// A `.gitmodules` GIT CANNOT READ IS NEVER A CLEAN READING (D2-1): `git status` reads clean over
/// a committed and pushed `.gitmodules` with a bad line, but the submodule read git refuses
/// counted nothing, so the probe is unread. NEGATIVE CONTROL: the same repository with that
/// file whole (its submodule never cloned here) reads checked.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_gitmodules_git_cannot_read_is_never_a_clean_reading() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    pushed(
        &work,
        ".gitmodules",
        "[submodule \"x\"]\n\tpath = x\n\turl = ../x.git\n",
    );
    let whole = probe(&work, Some(&work), PROBE_LIMIT);
    assert!(whole.checked().is_some(), "{whole:?}");
    pushed(&work, ".gitmodules", "[submodule \"x\"\n\tpath = x\n");
    assert_eq!(scratch_git(&work, &["status", "--porcelain"]), "");
    assert_eq!(probe(&work, Some(&work), PROBE_LIMIT), RepoRead::Unread);
}

/// `vendor/<name>` added to `app` from `url` (a path: a local clone), its record staged.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stage_submodule(app: &Path, url: &str, name: &str) {
    let path = format!("vendor/{name}");
    scratch_git(
        app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            url,
            &path,
        ],
    );
}

/// A commit made in `dir`, writing `name`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn local_commit(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), "x\n").expect("write");
    scratch_git(dir, &["add", name]);
    scratch_git(dir, &["commit", "-q", "-m", name]);
}

/// The submodule's checkout committed in `app` and pushed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn bump_pushed(app: &Path, path: &str) {
    scratch_git(app, &["add", path]);
    scratch_git(app, &["commit", "-q", "-m", "bump"]);
    scratch_git(app, &["push", "-q", "origin", "HEAD:main"]);
}

/// EVERY SUBMODULE IS READ, HOWEVER MANY (R-1): a project of 33 populated submodules, all
/// pushed, reads checked, never unread for its count, and a commit on no remote in the last one
/// read is counted and named. NEGATIVE CONTROL: once that commit is pushed, checked again.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn every_submodule_is_read_however_many() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (_lib, app) = app_with_lib(tmp.path());
    let url = tmp.path().join("lib/remote.git").display().to_string();
    // Their records in one commit: what is read is each checkout and its record, not their history.
    for n in 1..=32 {
        stage_submodule(&app, &url, &format!("lib{n:02}"));
    }
    scratch_git(
        &app,
        &["commit", "-q", "-m", "vendor/lib01 to vendor/lib32"],
    );
    scratch_git(&app, &["push", "-q", "origin", "HEAD:main"]);
    let all = probe(&app, Some(&app), PROBE_LIMIT);
    assert!(
        all.checked().is_some(),
        "33 submodules, all pushed: {all:?}"
    );
    let last = app.join("vendor/lib32");
    local_commit(&last, "d.txt");
    bump_pushed(&app, "vendor/lib32");
    let w = found(&app).expect("the 33rd submodule's commit on no remote");
    assert!(
        w.summary().ends_with(": 1 unpushed commit in vendor/lib32"),
        "{}",
        w.summary()
    );
    scratch_git(&last, &["push", "-q", "origin", "HEAD:refs/heads/side"]);
    let pushed = probe(&app, Some(&app), PROBE_LIMIT);
    assert!(pushed.checked().is_some(), "{pushed:?}");
}

/// A RECORD ON NO REMOTE HOLDS THOUGH ITS CHECKOUT LAGS (R-5): a commit made in `vendor/lib`,
/// recorded by a bump the superproject pushed, with the submodule then checked out back at its
/// first record, is a checkout `git status` lists and the lag read drops; only the index record
/// names the commit, and it is counted. NEGATIVE CONTROL: once that commit is on the submodule's
/// remote, the lagging checkout reads clean.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_record_on_no_remote_holds_though_its_checkout_lags() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (_lib, app) = app_with_lib(tmp.path());
    let sub = app.join("vendor/lib");
    let first = scratch_git(&sub, &["rev-parse", "HEAD"]).trim().to_string();
    local_commit(&sub, "d.txt");
    let made = scratch_git(&sub, &["rev-parse", "HEAD"]).trim().to_string();
    bump_pushed(&app, "vendor/lib");
    scratch_git(&sub, &["checkout", "-q", &first]);
    assert_eq!(
        scratch_git(&app, &["status", "--porcelain"]),
        " M vendor/lib\n"
    );
    let w = found(&app).expect("the recorded commit on no remote");
    assert_eq!((w.changed, w.sub_unpushed), (0, 1), "{w:?}");
    scratch_git(
        &sub,
        &["push", "-q", "origin", &format!("{made}:refs/heads/main")],
    );
    assert_eq!(found(&app), None, "the record pushed, the checkout lagging");
}

/// A SHALLOW SUBMODULE AT A FETCHED COMMIT IS NO WORK (R-4): a clone with `--shallow-submodules`
/// fetches the record one commit deep, below a tip of the submodule's remote that does not reach
/// it here; the record came from that remote and is not counted. NEGATIVE CONTROL: a commit made
/// on it, recorded by a bump the superproject pushed, is one commit on no remote.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_shallow_submodule_at_a_fetched_commit_is_no_work() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (lib, _app) = app_with_lib(tmp.path());
    pushed(&lib, "b.txt", "two\n");
    let lib_bare = tmp.path().join("lib/remote.git");
    scratch_git(
        &lib_bare,
        &["config", "uploadpack.allowReachableSHA1InWant", "true"],
    );
    let url = format!("file://{}", tmp.path().join("app/remote.git").display());
    scratch_git(
        tmp.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            "-q",
            "--recurse-submodules",
            "--shallow-submodules",
            &url,
            "clone",
        ],
    );
    let clone = tmp.path().join("clone");
    let sub = clone.join("vendor/lib");
    assert_eq!(
        scratch_git(&sub, &["rev-parse", "--is-shallow-repository"]).trim(),
        "true"
    );
    assert_eq!(scratch_git(&clone, &["status", "--porcelain"]), "");
    let at = probe(&clone, Some(&clone), PROBE_LIMIT);
    assert!(at.checked().is_some(), "{at:?}");
    local_commit(&sub, "d.txt");
    bump_pushed(&clone, "vendor/lib");
    let w = found(&clone).expect("a commit made on the shallow record");
    assert_eq!(w.sub_unpushed, 1, "{w:?}");
}

/// A SHALLOW SUBMODULE FETCHED MORE THAN ONE COMMIT DEEP IS NO WORK (`ATERM-KI-1020-2`):
/// `git submodule update --init --depth 3` at a detached commit no remote-tracking ref reaches
/// leaves `shallow` two commits below `HEAD`, while `FETCH_HEAD` records `HEAD` as fetched from
/// `origin`; neither `HEAD` nor the fetched ancestor between `HEAD` and `shallow` is counted.
/// NEGATIVE CONTROLS: a commit made on top of that shallow checkout is one unpushed commit (never
/// clean when cut), and a commit fetched into `FETCH_HEAD` from an unconfigured local repository
/// still counts as unpushed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_shallow_submodule_fetched_deeper_than_one_commit_is_no_work() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (lib, app) = app_with_lib(tmp.path());
    pushed(&lib, "b.txt", "two\n");
    pushed(&lib, "c.txt", "three\n");
    let pinned = pushed(&lib, "d.txt", "four\n");
    let sub_orig = app.join("vendor/lib");
    scratch_git(&sub_orig, &["fetch", "-q", "origin"]);
    scratch_git(&sub_orig, &["checkout", "-q", &pinned]);
    bump_pushed(&app, "vendor/lib");
    for n in 5..=10 {
        pushed(&lib, &format!("f{n}.txt"), &format!("{n}\n"));
    }
    let lib_bare = tmp.path().join("lib/remote.git");
    scratch_git(
        &lib_bare,
        &["config", "uploadpack.allowReachableSHA1InWant", "true"],
    );
    let url = format!("file://{}", tmp.path().join("app/remote.git").display());
    scratch_git(
        tmp.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            "-q",
            &url,
            "clone",
        ],
    );
    let clone = tmp.path().join("clone");
    scratch_git(
        &clone,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "-q",
            "--init",
            "--depth",
            "3",
        ],
    );
    let sub = clone.join("vendor/lib");
    assert_eq!(
        scratch_git(&sub, &["rev-parse", "--is-shallow-repository"]).trim(),
        "true"
    );
    // `origin/main` (shallow at c10..c8) does not reach `pinned` (c4), and `shallow` holds c2, so
    // `rev-list HEAD --not --remotes` lists c4, c3 and c2 before `FETCH_HEAD` is consulted.
    let unreached: Vec<String> = scratch_git(&sub, &["rev-list", "HEAD", "--not", "--remotes"])
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(unreached.len(), 3, "{unreached:?}");
    let at = probe(&clone, Some(&clone), PROBE_LIMIT);
    assert!(at.checked().is_some(), "depth-3 shallow submodule: {at:?}");

    // Negative control 1: a local commit on top of the depth-3 fetched commit holds as 1 unpushed.
    local_commit(&sub, "local.txt");
    bump_pushed(&clone, "vendor/lib");
    let w = found(&clone).expect("a commit made on the depth-3 shallow record");
    assert_eq!(w.sub_unpushed, 1, "{w:?}");
    never_clean_when_cut(&clone, &Used::default(), "depth-3 local commit");

    // Negative control 2: fetching a commit into `FETCH_HEAD` from an unconfigured local repo does
    // not prove any configured remote of the submodule has it.
    let rogue_dir = tmp.path().join("rogue");
    std::fs::create_dir_all(&rogue_dir).expect("rogue");
    let rogue = repo(&rogue_dir, false);
    let rogue_url = rogue.display().to_string();
    scratch_git(
        &sub,
        &[
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "-q",
            &rogue_url,
            "main",
        ],
    );
    scratch_git(&sub, &["checkout", "-q", "FETCH_HEAD"]);
    bump_pushed(&clone, "vendor/lib");
    let w = found(&clone).expect("a commit fetched from an unconfigured local repo");
    assert_eq!(w.sub_unpushed, 1, "{w:?}");
}

#[test]
fn fetch_head_display_url_matches_git_store_updated_refs() {
    assert_eq!(
        fetch_head_display_url("https://token:x-oauth-basic@github.com/org/repo.git"),
        "https://github.com/org/repo"
    );
    assert_eq!(
        fetch_head_display_url("https://github.com/org/repo.git/"),
        "https://github.com/org/repo"
    );
    assert_eq!(
        fetch_head_display_url("git@github.com:org/repo.git"),
        "git@github.com:org/repo"
    );
    assert_eq!(
        fetch_head_display_url("file:///tmp/rem.git/"),
        "file:///tmp/rem"
    );
    assert_eq!(fetch_head_display_url("/tmp/src/"), "/tmp/src");
    assert_eq!(fetch_head_display_url("/tmp/src/.git/"), "/tmp/src/");
}

/// A NESTED SUBMODULE'S COMMIT ON NO REMOTE IS WORK (R-6): `app` holds `vendor/mid`, which holds
/// `vendor/lib`; a commit made in the innermost, recorded by bumps pushed at both levels above it,
/// is counted and named by its path from `app`. NEGATIVE CONTROL: everything pushed, and that
/// commit once pushed, read clean.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_nested_submodules_commit_on_no_remote_is_work() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    app_with_lib(&tmp.path().join("mid"));
    let app_dir = tmp.path().join("app");
    std::fs::create_dir_all(&app_dir).expect("app");
    let app = repo(&app_dir, true);
    let url = format!("file://{}", tmp.path().join("mid/app/remote.git").display());
    scratch_git(
        &app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &url,
            "vendor/mid",
        ],
    );
    scratch_git(
        &app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "-q",
            "--init",
            "--recursive",
        ],
    );
    scratch_git(&app, &["commit", "-q", "-m", "mid"]);
    scratch_git(&app, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&app), None, "every level pushed");
    let (mid, lib) = (app.join("vendor/mid"), app.join("vendor/mid/vendor/lib"));
    local_commit(&lib, "d.txt");
    bump_pushed(&mid, "vendor/lib");
    bump_pushed(&app, "vendor/mid");
    assert_eq!(scratch_git(&app, &["status", "--porcelain"]), "");
    let w = found(&app).expect("the innermost commit on no remote");
    assert!(
        w.summary()
            .ends_with(": 1 unpushed commit in vendor/mid/vendor/lib"),
        "{}",
        w.summary()
    );
    scratch_git(&lib, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&app), None, "the innermost commit pushed");
}

/// A SESSION INSIDE A SUBMODULE READS THE PROJECT AROUND IT (D2-4): a session whose last `cd` was
/// into `vendor/lib` probes the superproject, so an edit there that no commit holds is found,
/// named in the outer repository. NEGATIVE CONTROL: with nothing left anywhere, the probe from
/// inside the submodule reads clean, naming the outer repository it checked.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_session_inside_a_submodule_reads_the_project_around_it() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (_lib, app) = app_with_lib(tmp.path());
    let sub = app.join("vendor/lib");
    let clean = probe(&sub, Some(&sub), PROBE_LIMIT);
    assert!(clean.is_clean() && !clean.is_unread(), "{clean:?}");
    std::fs::write(app.join("a.txt"), "edited\n").expect("edit");
    let w = found(&sub).expect("the outer project's edit");
    assert_eq!(
        (w.changed, w.paths.as_slice()),
        (1, &["a.txt".to_string()][..]),
        "{w:?}"
    );
    assert!(w.repo.ends_with("/app/work"), "{w:?}");
    scratch_git(&app, &["checkout", "-q", "--", "a.txt"]);
    let at = probe(&sub, Some(&sub), PROBE_LIMIT);
    assert!(
        at.checked()
            .is_some_and(|at| at.repo.ends_with("/app/work")),
        "{at:?}"
    );
}

/// A STATEMENT READ CUT OR REFUSED NEVER UNREADS A CLEAN READING (design B10): what a head
/// states of how it was judged is the probe's last read, so a probe whose last read is starved
/// past its limit still reads checked, its statement unread, in a session inside a submodule too,
/// whose inner repository is read after the project around it; and one git refuses reads checked,
/// its statement unread. CONTROLS: unstarved, each states [`Statement::Missing`] (a plain commit
/// on main). NEGATIVE CONTROL (run by hand): the statement read inside `repo_reads`, before the
/// inner repository's reads, makes the submodule session's last read an inner one, and starving
/// it reads unread.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_statement_read_cut_or_refused_never_unreads_a_clean_reading() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (_lib, app) = app_with_lib(tmp.path());
    let sub = app.join("vendor/lib");
    let starved = u64::try_from(PROBE_LIMIT.as_millis())
        .unwrap_or(u64::MAX)
        .saturating_add(1_000);
    let statement = |read: &RepoRead| read.checked().map(|at| at.statement.clone());
    for dir in [&app, &sub] {
        let probe_dir = || probe(dir, Some(dir), PROBE_LIMIT);
        memoized(probe_dir, |control| {
            assert_eq!(statement(control), Some(Statement::Missing), "{control:?}");
            // How many reads the probe makes: the first `k` it never reaches (a binary search, as
            // a probe that never reaches read `k` reaches no later one).
            let never_reaches = |k: u32| {
                read_stall::set_after(k, 1);
                let _ = probe_dir();
                let never = read_stall::pending();
                read_stall::set(0);
                never
            };
            let (mut lo, mut hi) = (1, 128);
            assert!(never_reaches(hi), "a probe reads fewer than {hi} times");
            while lo < hi {
                let mid = (lo + hi) / 2;
                if never_reaches(mid) {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            let reads = lo;
            read_stall::set_after(reads - 1, starved);
            let cut = probe_dir();
            read_stall::set(0);
            assert_eq!(
                statement(&cut),
                Some(Statement::Unread),
                "{}: the last read cut: {cut:?}",
                dir.display()
            );
        });
    }
    let refused = Reads {
        deadline: Cell::new(Instant::now() + PROBE_LIMIT),
        each: Some(PROBE_LIMIT),
        home: &app,
        xdg: None,
        cut: Cell::new(false),
        starts: Cell::new(0),
        held: None,
        refused: Some(&|args: &[&str]| args.iter().any(|a| a.starts_with("--format=%(trailers"))),
    };
    let read = probe_reading(&app, &Used::default(), &refused);
    assert_eq!(statement(&read), Some(Statement::Unread), "{read:?}");
}

/// A SESSION INSIDE A SUBMODULE STILL READS THE SUBMODULE (D2-4): its commit pushed to a branch of
/// the submodule's remote but not to that remote's main, recorded by a bump the superproject
/// pushed, holds the landing as it did before the probe read the project around it, and a read of
/// the project git refuses is unread though the submodule reads clean. NEGATIVE CONTROLS: a
/// session in the superproject reads nothing there, since its commit is on a remote; once on the
/// submodule's main, the session inside reads checked.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_session_inside_a_submodule_still_reads_the_submodule() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let (_lib, app) = app_with_lib(tmp.path());
    let sub = app.join("vendor/lib");
    scratch_git(&sub, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(sub.join("d.txt"), "four\n").expect("d.txt");
    scratch_git(&sub, &["add", "d.txt"]);
    scratch_git(&sub, &["commit", "-q", "-m", "feat"]);
    scratch_git(&sub, &["push", "-q", "origin", "feat"]);
    scratch_git(&app, &["add", "vendor/lib"]);
    scratch_git(&app, &["commit", "-q", "-m", "bump"]);
    scratch_git(&app, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(found(&app), None, "from the superproject");
    let w = found(&sub).expect("the submodule's commit not on its main");
    assert_eq!(
        (w.unlanded, w.main_ref.as_str()),
        (1, "origin/main"),
        "{w:?}"
    );
    assert!(w.repo.ends_with("/app/work/vendor/lib"), "{w:?}");
    scratch_git(&sub, &["push", "-q", "origin", "feat:main"]);
    let clean = probe(&sub, Some(&sub), PROBE_LIMIT);
    assert!(clean.checked().is_some(), "{clean:?}");
    pushed(&app, ".gitmodules", "[submodule \"vendor/lib\"\n");
    assert_eq!(
        probe(&sub, Some(&sub), PROBE_LIMIT),
        RepoRead::Unread,
        "a read of the superproject git refused, the submodule clean"
    );
}

/// A REMOTE CONFIGURED BUT NEVER PUSHED TO HOLDS EVERY COMMIT (D2-5): a repository whose `origin`
/// was added, or that was cloned from an empty repository, with no remote-tracking ref yet,
/// counts each commit on `HEAD` unpushed, which the worker can push. NEGATIVE CONTROL: with no
/// remote configured it reads clean with `remote=false` (the owner's 2026-10-25 answer 4), and
/// once pushed, clean with `remote=true`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_configured_remote_never_pushed_to_holds_every_commit() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), false);
    let alone = probe(&work, Some(&work), PROBE_LIMIT);
    assert!(alone.checked().is_some_and(|at| !at.remote), "{alone:?}");
    let bare = tmp.path().join("origin.git");
    std::fs::create_dir_all(&bare).expect("bare");
    scratch_git(&bare, &["init", "-q", "--bare", "."]);
    let url = format!("file://{}", bare.display());
    scratch_git(&work, &["remote", "add", "origin", &url]);
    let w = found(&work).expect("a remote added, never pushed to");
    assert_eq!((w.unpushed, w.unlanded, w.changed), (1, 0, 0), "{w:?}");
    never_clean_when_cut(&work, &Used::default(), "a commit never pushed");
    scratch_git(&work, &["push", "-q", "-u", "origin", "HEAD:main"]);
    let pushed = probe(&work, Some(&work), PROBE_LIMIT);
    assert!(pushed.checked().is_some_and(|at| at.remote), "{pushed:?}");

    let empty = tmp.path().join("empty.git");
    std::fs::create_dir_all(&empty).expect("empty");
    scratch_git(&empty, &["init", "-q", "--bare", "."]);
    let url = format!("file://{}", empty.display());
    scratch_git(tmp.path(), &["clone", "-q", &url, "clone"]);
    let clone = tmp.path().join("clone");
    std::fs::write(clone.join("a.txt"), "one\n").expect("a.txt");
    scratch_git(&clone, &["add", "a.txt"]);
    scratch_git(&clone, &["commit", "-q", "-m", "one"]);
    let w = found(&clone).expect("a clone of an empty repository, never pushed");
    assert_eq!((w.unpushed, w.unlanded), (1, 0), "{w:?}");
}

/// WORK THIS SESSION PARKED ON ANOTHER BRANCH HOLDS THE LANDING (D2-3): a branch this worktree
/// moved through after the session began (its own `HEAD` reflog), or one the session's own
/// command created in a worktree it then removed (its transcript), holding commits neither
/// `HEAD` nor the remote main has, is counted and named, and a stash made on it is counted.
/// NEGATIVE CONTROLS: a branch this worktree used before the session began, another worktree's
/// branch, and any probe naming no session read as before; once the branches are merged and
/// pushed and the stash dropped, nothing is found.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn work_this_session_parked_on_another_branch_holds_the_landing() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let (before, began, during) = (1_700_000_000, 1_700_000_100, 1_700_000_200);
    let at = |dir: &Path, when: u64, args: &[&str]| scratch_git_dated(dir, Some(when), args);
    let commit = |dir: &Path, when: u64, name: &str| {
        std::fs::write(dir.join(name), "x\n").expect("file");
        at(dir, when, &["add", name]);
        at(dir, when, &["commit", "-q", "-m", name]);
    };
    at(&work, before, &["checkout", "-q", "-b", "theirs"]);
    commit(&work, before, "theirs.txt");
    at(&work, before, &["checkout", "-q", "main"]);
    let other = tmp.path().join("other");
    let other_s = other.to_str().expect("utf-8");
    at(
        &work,
        during,
        &["worktree", "add", "-q", "-b", "side", other_s],
    );
    commit(&other, during, "side.txt");
    let session = Used {
        since: Some(began),
        branches: Vec::new(),
    };
    let read = |used: &Used| finished(probe_session(&work, Some(&work), used, PROBE_LIMIT));
    assert_eq!(
        read(&session),
        None,
        "before the session, or another worktree's"
    );

    at(&work, during, &["checkout", "-q", "-b", "feat"]);
    commit(&work, during, "feat.txt");
    at(&work, during, &["checkout", "-q", "main"]);
    assert_eq!(
        found(&work),
        None,
        "a probe naming no session reads HEAD alone"
    );
    let w = read(&session).expect("the branch this session left");
    assert_eq!((w.changed, w.stashes, w.unpushed, w.unlanded), (0, 0, 0, 0));
    assert!(
        w.summary()
            .ends_with(": 1 commit on feat not on origin/main"),
        "{}",
        w.summary()
    );
    never_clean_when_cut(&work, &session, "the branch this session left");

    at(&work, during, &["checkout", "-q", "feat"]);
    std::fs::write(work.join("a.txt"), "parked\n").expect("edit");
    at(&work, during, &["stash", "-q"]);
    at(&work, during, &["checkout", "-q", "main"]);
    let w = read(&session).expect("the stash on it");
    assert_eq!(w.stashes, 1, "{w:?}");

    let wt = tmp.path().join("wt");
    let wt_s = wt.to_str().expect("utf-8");
    at(
        &work,
        during,
        &["worktree", "add", "-q", "-b", "wt-feat", wt_s],
    );
    commit(&wt, during, "wt.txt");
    at(&work, during, &["worktree", "remove", wt_s]);
    let command = format!("git worktree add -b wt-feat {wt_s} && cd {wt_s}");
    let input = aterm_json::json!({ "command": command });
    let tail = format!(
        "{}\n",
        aterm_json::json!({
            "type": "assistant",
            "timestamp": "2023-11-14T22:16:40.000Z",
            "message": {"content": [{"type": "tool_use", "name": "Bash", "input": input}]}
        })
    );
    let named = Used::of_session(Some(began), Some(&tail));
    assert_eq!(named.branches, ["wt-feat"], "{named:?}");
    let w = read(&session).expect("feat and its stash");
    assert!(!w.summary().contains("wt-feat"), "{}", w.summary());
    let w = read(&named).expect("the removed worktree's branch");
    assert!(
        w.summary()
            .contains("2 commits on feat, wt-feat not on origin/main"),
        "{}",
        w.summary()
    );

    at(
        &work,
        during,
        &["merge", "-q", "--no-edit", "feat", "wt-feat"],
    );
    at(&work, during, &["push", "-q", "origin", "HEAD:main"]);
    at(&work, during, &["stash", "drop", "-q"]);
    assert_eq!(read(&named), None, "merged, pushed and the stash dropped");
}

/// A BRANCH PUSHED TO `origin` AND DELETED HERE IS STILL PARKED (R-2): the session made `feat/x` in
/// a worktree, pushed it to `origin/feat/x` and deleted its own branch, which git allows without
/// `-D` once the remote branch has it; its commit is on no remote main, so it is counted and
/// named. NEGATIVE CONTROLS: another remote's branch of a name the session used is none of its
/// work; once merged into the remote main, nothing is found.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_branch_pushed_to_origin_and_deleted_here_is_still_parked() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let fork = tmp.path().join("fork.git");
    std::fs::create_dir_all(&fork).expect("fork");
    scratch_git(&fork, &["init", "-q", "--bare", "."]);
    let fork_url = format!("file://{}", fork.display());
    scratch_git(&work, &["remote", "add", "fork", &fork_url]);
    scratch_git(&work, &["checkout", "-q", "-b", "feat/y"]);
    local_commit(&work, "y.txt");
    scratch_git(&work, &["push", "-q", "fork", "feat/y"]);
    scratch_git(&work, &["checkout", "-q", "main"]);
    scratch_git(&work, &["branch", "-q", "-D", "feat/y"]);
    let wt = tmp.path().join("wt");
    let wt_s = wt.to_str().expect("utf-8");
    scratch_git(&work, &["worktree", "add", "-q", "-b", "feat/x", wt_s]);
    local_commit(&wt, "x.txt");
    scratch_git(&wt, &["push", "-q", "-u", "origin", "feat/x"]);
    scratch_git(&work, &["worktree", "remove", wt_s]);
    scratch_git(&work, &["branch", "-q", "-d", "feat/x"]);
    let used = Used {
        since: None,
        branches: vec!["feat/x".to_string(), "feat/y".to_string()],
    };
    let read = |used: &Used| finished(probe_session(&work, Some(&work), used, PROBE_LIMIT));
    let w = read(&used).expect("the branch pushed to origin, deleted here");
    assert!(
        w.summary()
            .ends_with(": 1 commit on feat/x not on origin/main"),
        "{}",
        w.summary()
    );
    scratch_git(&work, &["merge", "-q", "--no-edit", "origin/feat/x"]);
    scratch_git(&work, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(read(&used), None, "merged into the remote main");
}

/// A BRANCH THE SESSION USED THAT GIT CANNOT READ IS NEVER A CLEAN READING (R-5): a commit of
/// `feat` whose object is gone fails the read of the branches the session used, though every
/// other read answers, so the probe is unread. NEGATIVE CONTROL: a probe naming no session never
/// reads `feat`, and reads checked.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_branch_the_session_used_that_git_cannot_read_is_never_a_clean_reading() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    scratch_git(&work, &["checkout", "-q", "-b", "feat"]);
    local_commit(&work, "b.txt");
    let gone = scratch_git(&work, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    local_commit(&work, "c.txt");
    scratch_git(&work, &["checkout", "-q", "main"]);
    let (dir, file) = gone.split_at(2);
    std::fs::remove_file(work.join(".git/objects").join(dir).join(file)).expect("the object");
    let plain = probe(&work, Some(&work), PROBE_LIMIT);
    assert!(plain.checked().is_some(), "{plain:?}");
    let used = Used {
        since: None,
        branches: vec!["feat".to_string()],
    };
    assert_eq!(
        probe_session(&work, Some(&work), &used, PROBE_LIMIT),
        RepoRead::Unread
    );
}

/// THE SESSION'S OWN BRANCHES AND START ([`Used::of_session`]): each branch its commands
/// created or checked out (`checkout -b`, `switch -c`, `worktree add -b`, `branch <name>`, a
/// worktree named after its folder, through `-C` and chained commands), newest first, each
/// once, and the earlier of the agent's start and the first stamp its transcript holds.
/// CONTROLS: a branch deleted, listed or renamed from, a file checked out, and words no Bash
/// call ran name nothing.
#[test]
fn the_session_names_its_own_branches_and_start() {
    let call = |command: &str| {
        let input = aterm_json::json!({ "command": command });
        format!(
            "{}\n",
            aterm_json::json!({
                "type": "assistant",
                "timestamp": "2023-11-14T22:13:20.000Z",
                "message": {"content": [{"type": "tool_use", "name": "Bash", "input": input}]}
            })
        )
    };
    let tail: String = [
        "git checkout -b a1",
        "cd /r && git -C /r switch -c a2; git switch --create=a3",
        "git worktree add -B a4 ../x && git worktree add ../wt-dir",
        "git worktree add --detach ../y && git branch a5 origin/main",
        "git -c x=y checkout --conflict diff3 a6 && git checkout -- a.txt",
        "git branch -D gone && git branch --list 'a*' && git branch --sort=-committerdate 'a*' && git branch --merged=main && git branch -m old a7",
        "echo git checkout -b quoted",
        "git checkout -b a1",
    ]
    .iter()
    .map(|c| call(c))
    .collect::<String>()
        + "{\"type\":\"user\",\"message\":{\"content\":\"git checkout -b typed\"}}\n";
    let used = Used::of_session(Some(1_800_000_000), Some(&tail));
    assert_eq!(
        used.since,
        Some(1_700_000_000),
        "the transcript's first stamp"
    );
    assert_eq!(
        used.branches,
        ["a1", "a7", "a6", "a5", "wt-dir", "a4", "a3", "a2"],
        "{used:?}"
    );
    assert_eq!(
        Used::of_session(Some(1_600_000_000), Some(&tail)).since,
        Some(1_600_000_000),
        "the agent's start, the earlier"
    );
    assert_eq!(Used::of_session(None, None), Used::default());
}

/// THE REFLOG'S TAIL STARTS AT A WHOLE LINE: the first line that starts at or past the last
/// [`LAG_REFLOG`] bytes, one that starts exactly there included, and a shorter file whole.
#[test]
fn a_reflogs_tail_starts_at_its_first_whole_line() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let size = usize::try_from(LAG_REFLOG).expect("usize");
    for (name, text) in [
        // 16-byte lines: the tail starts exactly on one.
        (
            "even",
            (0..(size / 16 + 100))
                .map(|i| format!("line {i:010}\n"))
                .collect::<String>(),
        ),
        // 12-byte lines after a 2-byte one: it starts inside one.
        (
            "odd",
            std::iter::once("x\n".to_string())
                .chain((0..(size / 12 + 100)).map(|i| format!("line {i:06}\n")))
                .collect(),
        ),
        ("short", "a b c\nd e f\n".to_string()),
    ] {
        let path = tmp.path().join(name);
        std::fs::write(&path, &text).expect("write");
        let mut tail = String::new();
        std::io::Read::read_to_string(&mut reflog_tail(&path).expect("a file"), &mut tail)
            .expect("read");
        let from = text.len().saturating_sub(size);
        let want = (from..text.len())
            .find(|&i| i == 0 || text.as_bytes()[i - 1] == b'\n')
            .expect("a line");
        assert_eq!(tail, text[want..], "{name}");
    }
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

/// A PROBE WHOSE EVERY READ IS SLOW STILL READS THE WHOLE REPOSITORY
/// ([`PROBE_LIMIT`] is each read's hang detector, not the probe's budget):
/// with every read starved 6 s (on the probe's clock, no sleep), so that its
/// reads lose more than a minute together, the probe reads a pushed
/// repository clean and finds the one commit no remote has in another (under a
/// CPU clamp beside 56 busy loops, a read took up to 3.9 s and a probe's
/// thirteen reads 24 s together), its clean reading naming the commit it read
/// ([`RepoRead::Checked`], slice 9a). NEGATIVE CONTROL: one read starved past
/// [`PROBE_LIMIT`] is unread, the first or any later one, whose limit runs
/// from the end of the read before it, not from the reads before that.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_probe_whose_every_read_is_slow_still_reads_the_whole_repository() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let pushed = repo(&tmp.path().join("pushed"), true);
    let ahead = repo(&tmp.path().join("ahead"), true);
    std::fs::write(ahead.join("b.txt"), "two\n").expect("b.txt");
    scratch_git(&ahead, &["add", "b.txt"]);
    scratch_git(&ahead, &["commit", "-q", "-m", "two"]);
    let slow = |work: &Path| {
        read_stall::every(6_000);
        let read = probe(work, Some(work), PROBE_LIMIT);
        read_stall::set(0);
        read
    };
    let clean = slow(&pushed);
    assert!(
        clean.checked().is_some(),
        "the pushed repository, read whole: {clean:?}"
    );
    let w = slow(&ahead)
        .into_found()
        .expect("the unpushed commit, found by the slow probe");
    assert_eq!((w.unpushed, w.unlanded, w.changed), (1, 1, 0), "{w:?}");
    let hung = u64::try_from(PROBE_LIMIT.as_millis()).unwrap_or(u64::MAX) + 1_000;
    read_stall::set(hung);
    let first = probe(&ahead, Some(&ahead), PROBE_LIMIT);
    read_stall::set(0);
    assert_eq!(first, RepoRead::Unread, "the first read past its limit");
    // A pushed repository's probe makes at least nine reads (the top level,
    // two excludes, the status, the branch, the stash, the remotes, the
    // unpushed count and the remote's HEAD); a cut at any of them reads
    // nothing found.
    for k in 1..9 {
        read_stall::set_after(k, hung);
        let later = probe(&pushed, Some(&pushed), PROBE_LIMIT);
        read_stall::set(0);
        assert_eq!(later, RepoRead::Unread, "read {k} past its limit");
    }
}

/// A PROBE THAT READS ON IS ANSWERED AT A LATER ASK ([`Probing::ask`]): with
/// the probe's reads held, an ask that waits nothing answers `None` and leaves
/// the probe reading; the next ask, once the reads go on, answers what they
/// found and begins no probe; and the ask after that begins a new one. A
/// probe that panics reads unread, never clean. NEGATIVE CONTROL: a probe
/// whose reads end at once is answered at its first ask.
#[test]
fn a_probe_that_reads_on_is_answered_at_a_later_ask() {
    use std::sync::atomic::AtomicUsize;
    let probing = Probing::default();
    let stop = AtomicBool::new(false);
    let begun = AtomicUsize::new(0);
    let begin = |answer: RepoRead, held: Option<mpsc::Receiver<()>>| {
        begun.fetch_add(1, Ordering::SeqCst);
        move || {
            if let Some(held) = held {
                let _ = held.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
            }
            answer
        }
    };
    let found = RepoRead::Found(RepoWork {
        repo: "/r".to_string(),
        changed: 1,
        ..RepoWork::default()
    });
    let (go, held) = mpsc::channel::<()>();
    let first = probing.ask(|| begin(found.clone(), Some(held)), Duration::ZERO, &stop);
    assert_eq!(first, None, "the reads are held");
    go.send(()).expect("the probe waits on its reads");
    let wait = aterm_spec::wait::MUST_HAPPEN;
    let second = probing.ask(|| begin(RepoRead::Clean, None), wait, &stop);
    assert_eq!(second, Some(found), "the held probe's finding");
    assert_eq!(begun.load(Ordering::SeqCst), 1, "no probe begun beside it");
    let third = probing.ask(|| begin(RepoRead::Clean, None), wait, &stop);
    assert_eq!(
        third,
        Some(RepoRead::Clean),
        "a new probe, answered at once"
    );
    assert_eq!(begun.load(Ordering::SeqCst), 2);
    let panicked = probing.ask(
        || || -> RepoRead { panic!("the probe's reads panicked") },
        wait,
        &stop,
    );
    assert_eq!(panicked, Some(RepoRead::Unread), "a probe that panicked");
}

/// A STOPPED ASKER IS NOT KEPT WAITING ([`Probing::ask`]): with the probe's
/// reads held and the asker's stop raised, an ask that would wait ten minutes
/// answers `None` within the one-minute hang detector, the probe left
/// reading. NEGATIVE CONTROL: with no stop, the next ask answers the held
/// probe's finding once its reads go on.
#[test]
fn a_stopped_asker_is_not_kept_waiting() {
    let wait = aterm_spec::wait::MUST_HAPPEN;
    let probing = std::sync::Arc::new(Probing::default());
    let stop = std::sync::Arc::new(AtomicBool::new(true));
    let (go, held) = mpsc::channel::<()>();
    let (said, answer) = mpsc::channel();
    {
        let (probing, stop) = (
            std::sync::Arc::clone(&probing),
            std::sync::Arc::clone(&stop),
        );
        std::thread::spawn(move || {
            let read = probing.ask(
                move || {
                    move || {
                        let _ = held.recv_timeout(wait);
                        RepoRead::Clean
                    }
                },
                10 * wait,
                &stop,
            );
            let _ = said.send(read);
        });
    }
    let stopped = answer
        .recv_timeout(wait)
        .expect("the stopped ask answered within the hang detector");
    assert_eq!(stopped, None, "the reads are held");
    stop.store(false, Ordering::SeqCst);
    go.send(()).expect("the probe waits on its reads");
    assert_eq!(
        probing.ask(|| || RepoRead::Unread, wait, &stop),
        Some(RepoRead::Clean),
        "the held probe's answer, once its reads went on"
    );
}

/// A PROBE CUT AT ANY READ IS NEVER A CLEAN REPOSITORY ([`RepoRead::Unread`]):
/// over a repository holding one unpushed commit and over one holding one
/// changed file, a probe whose `k`-th read is starved past its limit (on the
/// probe's clock, no sleep) reads as the work it found or as unread, for
/// every `k` as far as the first the probe never reached, which reads whole
/// and has no later `k` to differ from. NEGATIVE CONTROL: the same repositories
/// pushed and clean, with no read starved, read clean.
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
        let probe_work = || probe(work, Some(work), Duration::from_secs(10));
        // The control, read with the memo off once the sweep ends ([`memoized`]): the reads past
        // the last one starve nothing, and the work reads whole again.
        memoized(probe_work, |real| {
            let whole = finished(real.clone()).expect("the work, read whole");
            let mut unread = 0;
            for k in 0..24 {
                read_stall::set_after(k, 11_000);
                let read = probe_work();
                let past_the_last_read = read_stall::pending();
                read_stall::set(0);
                match &read {
                    RepoRead::Clean | RepoRead::Checked(_) | RepoRead::Gone(_) => {
                        panic!("read {k} cut: read as clean over {whole:?}")
                    }
                    RepoRead::Unread => unread += 1,
                    RepoRead::Found(_) => {}
                }
                if past_the_last_read {
                    assert_eq!(
                        read,
                        RepoRead::Found(whole.clone()),
                        "read {k} never reached, so nothing was cut"
                    );
                    break;
                }
            }
            assert!(unread > 0, "the first read cut reads as unread");
        });
    }
}

/// A CLEAN REPOSITORY READS ITS HEAD AND MAIN ([`RepoRead::Checked`],
/// `docs/DESIGN-harness-autopilot-2026-10-15.md` §14.9, slice 9a): over a
/// repository pushed to a bare origin, the probe that finds nothing names the
/// repository, the commit `HEAD` names (the one `git rev-parse HEAD` prints,
/// and the next one once that is pushed), `origin/main` and a remote; it is
/// clean, finds nothing and is the same as a clean reading. CONTROLS: the
/// same repository with no remote reads `remote=false` and no main; a
/// repository with no commit yet and a plain folder name nothing and read
/// [`RepoRead::Clean`]. Each names `HEAD`'s branch, and a detached `HEAD` none
/// (slice 9c). A DELETED FOLDER READS GONE (§14.17 item 4, slice 9c), named,
/// and decides as clean; a relative one is never read. NEGATIVE CONTROL (run
/// by hand): `gone_dir`'s `NotFound` test dropped reads the plain folder gone.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_clean_repository_reads_its_head_and_main() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let head_of = |work: &Path| scratch_git(work, &["rev-parse", "HEAD"]).trim().to_string();
    let checked = |work: &Path| {
        let read = probe(work, Some(work), PROBE_LIMIT);
        assert!(
            read.is_clean() && read.found().is_none() && !read.is_unread(),
            "{read:?}"
        );
        assert!(RepoRead::same(&read, &RepoRead::Clean), "{read:?}");
        read.checked().cloned()
    };
    let pushed = repo(&tmp.path().join("pushed"), true);
    let at = checked(&pushed).expect("a clean pushed repository names what it checked");
    assert_eq!(
        (at.head.as_str(), at.main_ref.as_str(), at.remote),
        (head_of(&pushed).as_str(), "origin/main", true),
        "{at:?}"
    );
    assert_eq!(at.head.len(), 40, "{at:?}");
    assert!(at.repo.ends_with("/pushed/work"), "{at:?}");
    let branch = scratch_git(&pushed, &["symbolic-ref", "--short", "HEAD"]);
    assert!(
        !branch.trim().is_empty() && at.branch == branch.trim(),
        "{at:?}"
    );
    std::fs::write(
        pushed.join("b.txt"),
        "two
",
    )
    .expect("b.txt");
    scratch_git(&pushed, &["add", "b.txt"]);
    scratch_git(&pushed, &["commit", "-q", "-m", "two"]);
    assert!(found(&pushed).is_some(), "the new commit, unpushed");
    scratch_git(&pushed, &["push", "-q", "origin", "HEAD:main"]);
    let next = checked(&pushed).expect("pushed again");
    assert_eq!(next.head, head_of(&pushed), "{next:?}");
    assert_ne!(next.head, at.head, "the probe reads HEAD each time");
    let alone = repo(&tmp.path().join("alone"), false);
    let at = checked(&alone).expect("a clean repository with no remote");
    assert_eq!(
        (at.head.as_str(), at.main_ref.as_str(), at.remote),
        (head_of(&alone).as_str(), "", false),
        "{at:?}"
    );
    assert!(!at.branch.is_empty(), "{at:?}");
    scratch_git(&alone, &["checkout", "-q", "--detach"]);
    let at = checked(&alone).expect("a detached HEAD");
    assert_eq!(
        at.branch,
        crate::harness::repo_work::DETACHED,
        "a detached HEAD names git's own word: {at:?}"
    );
    let unborn = tmp.path().join("unborn");
    std::fs::create_dir_all(&unborn).expect("unborn");
    scratch_git(&unborn, &["init", "-q", "."]);
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).expect("plain");
    for (dir, what) in [(&unborn, "no commit yet"), (&plain, "a plain folder")] {
        assert_eq!(
            probe(dir, Some(tmp.path()), PROBE_LIMIT),
            RepoRead::Clean,
            "{what}"
        );
    }
    let gone = tmp.path().join("gone").join("proj");
    std::fs::create_dir_all(&gone).expect("gone");
    std::fs::remove_dir_all(tmp.path().join("gone")).expect("deleted");
    let read = probe(&gone, Some(tmp.path()), PROBE_LIMIT);
    assert_eq!(read, RepoRead::Gone(printable(&gone.to_string_lossy())));
    assert!(
        read.is_clean()
            && RepoRead::same(&read, &RepoRead::Clean)
            && read.found().is_none()
            && read.checked().is_none(),
        "{read:?}"
    );
    assert_eq!(
        probe(Path::new("gone/proj"), Some(tmp.path()), PROBE_LIMIT),
        RepoRead::Clean,
        "a relative folder"
    );
}

/// A CLEAN PROBE CUT AT ITS HEAD READ READS AS IT DID BEFORE THE HEAD WAS READ
/// (§14.9, slice 9a: no decision moves): over a clean pushed repository, a
/// probe whose `k`-th read is starved past its limit reads unread for every
/// read before the head read, as the probe read before it read `HEAD`; clean,
/// naming nothing, where only the head read is cut, as that probe read with
/// every read finished; and checked past it. NEGATIVE CONTROL: the same
/// probes with no read starved read checked. The starved read alone is cut:
/// the probe's own limit is the one-minute hang detector for the real reads.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_clean_probe_cut_at_its_head_read_reads_as_before_the_head_was_read() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let starved = u64::try_from(PROBE_LIMIT.as_millis())
        .unwrap_or(u64::MAX)
        .saturating_add(1_000);
    let probe_work = || probe(&work, Some(&work), PROBE_LIMIT);
    // The control, with nothing starved, reads checked, and reads as it once the sweep ends, with
    // the memo off ([`memoized`]).
    memoized(probe_work, |real| {
        assert!(real.checked().is_some(), "the control: nothing starved");
        let mut seen = String::new();
        for k in 0..24 {
            read_stall::set_after(k, starved);
            let read = probe_work();
            read_stall::set(0);
            seen.push(match read {
                RepoRead::Unread => 'u',
                RepoRead::Clean => 'c',
                RepoRead::Checked(_) => 'k',
                RepoRead::Found(w) => panic!("read {k} cut: found {w:?} in a clean repository"),
                RepoRead::Gone(g) => panic!("read {k} cut: {g:?} read as gone"),
            });
        }
        let (unread, rest) = seen.split_at(seen.find('c').unwrap_or(seen.len()));
        assert!(
            !unread.is_empty()
                && unread.chars().all(|c| c == 'u')
                && rest.len() > 1
                && rest.starts_with('c')
                && rest[1..].chars().all(|c| c == 'k'),
            "unread before the head read, clean at it, checked past it: {seen}"
        );
    });
}

/// A COUNT GIT REFUSED AT A COMMIT `HEAD` NAMES IS NEVER A CLEAN REPOSITORY
/// ([`RepoRead::Unread`]): where `HEAD` names a commit, a read the counts rest
/// on that git refused reads unread however clean `status` read, as what it
/// would have counted is work a landing must not end over: a stash list over a
/// stash ref to an object the repository lacks (one stash on the branch), and
/// an unpushed count over a remote-tracking ref to one (one unpushed commit,
/// with no `origin` main to count it as unlanded). CONTROLS: the same
/// repositories before the broken ref read the stash and the commit; a
/// repository with no commit yet, whose fetched remote-tracking refs make git
/// refuse its counts, reads [`RepoRead::Clean`], as its `HEAD` names nothing to
/// count; and a clean pushed repository given a packed remote-tracking ref
/// named in bytes that are no UTF-8 is read whole, `Checked` with
/// `remote=true`, as the remote read asks only whether a ref is there.
/// The broken stash also reads unread with any one of its reads starved past
/// its limit, the head read among them: a refused count with its head read
/// cut is unread too. And an unlanded count git refused (a commit pushed only
/// to a feature branch, a commit of its history missing here, so the
/// unpushed count still answers 0) reads unread; its control, the same
/// repository before the object went, finds both commits unlanded.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_count_git_refused_at_a_commit_head_is_never_a_clean_repository() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let missing = "0123456789abcdef0123456789abcdef01234567\n";
    let stashed = repo(&tmp.path().join("stashed"), true);
    std::fs::write(stashed.join("a.txt"), "changed\n").expect("a.txt");
    scratch_git(&stashed, &["stash", "-q"]);
    assert_eq!(
        found(&stashed).map(|w| w.stashes),
        Some(1),
        "the control: the stash"
    );
    std::fs::write(stashed.join(".git/refs/stash"), missing).expect("refs/stash");
    let starved = u64::try_from(PROBE_LIMIT.as_millis())
        .unwrap_or(u64::MAX)
        .saturating_add(1_000);
    let probe_stashed = || probe(&stashed, Some(&stashed), PROBE_LIMIT);
    memoized(probe_stashed, |real| {
        assert_eq!(real, &RepoRead::Unread, "a stash list git refused");
        let mut seen = String::new();
        for k in 0..24 {
            read_stall::set_after(k, starved);
            let read = probe_stashed();
            read_stall::set(0);
            seen.push(if read == RepoRead::Unread { 'u' } else { 'c' });
        }
        assert_eq!(
            seen,
            "u".repeat(24),
            "the refused stash list with the k-th read cut, the head read among them"
        );
    });
    let feature = repo(&tmp.path().join("feature"), true);
    let mut history = Vec::new();
    for (name, text) in [("b.txt", "two\n"), ("c.txt", "three\n")] {
        std::fs::write(feature.join(name), text).expect(name);
        scratch_git(&feature, &["add", name]);
        scratch_git(&feature, &["commit", "-q", "-m", name]);
        history.push(
            scratch_git(&feature, &["rev-parse", "HEAD"])
                .trim()
                .to_string(),
        );
    }
    scratch_git(&feature, &["push", "-q", "origin", "HEAD:feature"]);
    assert_eq!(
        found(&feature).map(|w| (w.unpushed, w.unlanded)),
        Some((0, 2)),
        "the control: pushed to a feature branch, unlanded"
    );
    let (dir, file) = history[0].split_at(2);
    std::fs::remove_file(feature.join(".git/objects").join(dir).join(file))
        .expect("a loose commit of the history");
    assert_eq!(
        probe(&feature, Some(&feature), PROBE_LIMIT),
        RepoRead::Unread,
        "an unlanded count git refused"
    );
    let upstream = repo(&tmp.path().join("upstream"), true);
    scratch_git(&upstream, &["remote", "rename", "origin", "upstream"]);
    std::fs::write(upstream.join("b.txt"), "two\n").expect("b.txt");
    scratch_git(&upstream, &["add", "b.txt"]);
    scratch_git(&upstream, &["commit", "-q", "-m", "two"]);
    assert_eq!(
        found(&upstream).map(|w| (w.unpushed, w.unlanded)),
        Some((1, 0)),
        "the control: the unpushed commit, no origin main"
    );
    std::fs::write(upstream.join(".git/refs/remotes/upstream/gone"), missing)
        .expect("a remote-tracking ref");
    assert_eq!(
        probe(&upstream, Some(&upstream), PROBE_LIMIT),
        RepoRead::Unread,
        "an unpushed count git refused"
    );
    let unborn = tmp.path().join("unborn");
    std::fs::create_dir_all(&unborn).expect("unborn");
    scratch_git(&unborn, &["init", "-q", "."]);
    let origin = tmp.path().join("stashed").join("remote.git");
    scratch_git(
        &unborn,
        &["remote", "add", "origin", &origin.display().to_string()],
    );
    scratch_git(&unborn, &["fetch", "-q", "origin"]);
    assert_eq!(
        probe(&unborn, Some(&unborn), PROBE_LIMIT),
        RepoRead::Clean,
        "no commit yet: nothing to count"
    );
    let work = repo(&tmp.path().join("named"), true);
    let head = scratch_git(&work, &["rev-parse", "HEAD"]);
    let packed = work.join(".git/packed-refs");
    let mut refs: Vec<u8> = std::fs::read(&packed)
        .unwrap_or_default()
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .flat_map(|line| line.iter().copied().chain(*b"\n"))
        .collect();
    refs.extend_from_slice(head.trim().as_bytes());
    refs.extend_from_slice(b" refs/remotes/o/\xff\n");
    std::fs::write(&packed, refs).expect("packed-refs");
    let at = probe(&work, Some(&work), PROBE_LIMIT);
    assert!(
        at.checked().is_some_and(|at| at.remote),
        "a ref named in no UTF-8 is counted: {at:?}"
    );
}

/// Refused counts and an ordinary failed HEAD read cannot jointly prove a repository clean.
/// Only HEAD-dependent counts may be absent in a branch independently proved unborn; a
/// refused stash list or failed unborn confirmation remains unread. Every unselected Git
/// query runs against a real repository, including the positive and metadata-only controls.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_refused_count_needs_independent_proof_an_unread_head_is_unborn() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let pushed = repo(&tmp.path().join("pushed"), true);
    let read = |work: &Path, refused: &dyn Fn(&[&str]) -> bool| {
        let reads = Reads {
            deadline: Cell::new(Instant::now() + PROBE_LIMIT),
            each: Some(PROBE_LIMIT),
            home: work,
            xdg: None,
            cut: Cell::new(false),
            starts: Cell::new(0),
            held: None,
            refused: Some(refused),
        };
        probe_reading(work, &Used::default(), &reads)
    };
    let head = |args: &[&str]| args == ["rev-parse", "--verify", "--quiet", "HEAD"];
    let stash = |args: &[&str]| args.first() == Some(&"stash");
    assert!(
        read(&pushed, &|_| false).checked().is_some(),
        "a fully read pushed repository"
    );
    assert_eq!(
        read(&pushed, &|args| head(args) || stash(args)),
        RepoRead::Unread,
        "a failed stash count and HEAD read are no proof of an unborn branch"
    );
    assert_eq!(
        read(&pushed, &|args| {
            head(args) || args.first() == Some(&"rev-list")
        }),
        RepoRead::Unread,
        "a refused HEAD-dependent count still needs an explicit unborn proof"
    );
    assert_eq!(
        read(&pushed, &head),
        RepoRead::Clean,
        "all counts answered: HEAD contributes only optional receipt metadata"
    );
    let unborn = tmp.path().join("unborn");
    std::fs::create_dir_all(&unborn).expect("unborn");
    scratch_git(&unborn, &["init", "-q", "."]);
    let remote = tmp.path().join("pushed/remote.git");
    scratch_git(
        &unborn,
        &["remote", "add", "origin", &remote.display().to_string()],
    );
    scratch_git(&unborn, &["fetch", "-q", "origin"]);
    assert_eq!(
        read(&unborn, &|_| false),
        RepoRead::Clean,
        "valid empty branch proof excuses only HEAD-dependent counts"
    );
    assert_eq!(
        read(&unborn, &stash),
        RepoRead::Unread,
        "a stash refusal is independent of whether HEAD has a commit"
    );
    assert_eq!(
        read(&unborn, &|args| {
            args.first() == Some(&"for-each-ref") && args.last() == Some(&"refs/heads/main")
        }),
        RepoRead::Unread,
        "a failed confirmation proves no unborn branch"
    );
    let raw_branch = repo(&tmp.path().join("raw-branch"), true);
    let committed = scratch_git(&raw_branch, &["rev-parse", "HEAD"]);
    let mut packed = committed.trim().as_bytes().to_vec();
    packed.extend_from_slice(b" refs/heads/caf\xe9\n");
    std::fs::write(raw_branch.join(".git/packed-refs"), packed).expect("packed raw branch");
    std::fs::write(raw_branch.join(".git/HEAD"), b"ref: refs/heads/caf\xe9\n")
        .expect("raw branch HEAD");
    assert!(
        read(&raw_branch, &|_| false).checked().is_some(),
        "a fully read committed branch may display its name lossily"
    );
    assert_eq!(
        read(&raw_branch, &|args| {
            head(args) || args.first() == Some(&"rev-list")
        }),
        RepoRead::Unread,
        "lossy replacement bytes name no ref whose absence proves the real branch unborn"
    );
    let broken = unborn.join(".git/refs/heads/main/broken");
    std::fs::create_dir_all(broken.parent().expect("refs directory")).expect("refs directory");
    std::fs::write(broken, "not-a-ref\n").expect("malformed ref");
    assert_eq!(
        read(&unborn, &|_| false),
        RepoRead::Unread,
        "Git's success with an ignored-ref warning is no complete count or empty proof"
    );
}

/// A GUARDED REPOSITORY IS NEVER READ, AND ITS CLEAN READING NAMES NOTHING
/// (§14.9, slice 9a: the leg a landing's words must be true on with nothing
/// read): a repository with untracked work under the home's `Documents`
/// reads [`RepoRead::Clean`] and names no check, as the probe never enters
/// it. CONTROL: the same repository under `Developer` reads its work.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_guarded_repository_with_work_reads_clean_and_names_nothing() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = std::fs::canonicalize(tmp.path()).expect("owned tmp");
    for (place, guarded) in [("Documents", true), ("Developer", false)] {
        let work = repo(&home.join(place), false);
        std::fs::write(work.join("draft.txt"), "draft\n").expect("draft");
        let read = probe(&work, Some(&home), PROBE_LIMIT);
        if guarded {
            assert_eq!(read, RepoRead::Clean, "{place}");
            assert!(read.checked().is_none(), "{place}");
        } else {
            assert!(
                read.found().is_some_and(|w| w.untracked > 0),
                "{place}: {read:?}"
            );
        }
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
    let unread_dir = repo(&tmp.path().join("unread-dir"), true);
    let hidden = unread_dir.join("hidden");
    std::fs::create_dir_all(&hidden).expect("hidden");
    std::fs::write(hidden.join("work.txt"), "work that Git must not erase\n").expect("work.txt");
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&hidden).expect("meta").permissions();
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let denied = std::fs::read_dir(&hidden).is_err();
        let restricted = probe(&unread_dir, Some(&unread_dir), PROBE_LIMIT);
        std::fs::set_permissions(&hidden, perms).expect("restore");
        if denied {
            assert_eq!(restricted, RepoRead::Unread);
        } else {
            assert!(restricted.found().is_some_and(|w| w.untracked == 1));
        }
    }
    assert_eq!(
        found(&unread_dir).map(|w| w.paths),
        Some(vec!["hidden/".to_string()])
    );
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

/// `path` with the last `from` in it replaced by `to`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rewrite_last(path: &Path, from: &[u8], to: &[u8]) {
    let bytes = std::fs::read(path).expect("the file");
    let at = bytes
        .windows(from.len())
        .rposition(|w| w == from)
        .unwrap_or_else(|| panic!("{:?} in {}", from, path.display()));
    let mut out = bytes[..at].to_vec();
    out.extend_from_slice(to);
    out.extend_from_slice(&bytes[at + from.len()..]);
    std::fs::write(path, out).expect("rewritten");
}

/// A STASH OR BRANCH NAMED IN BYTES THAT ARE NO UTF-8 IS COUNTED ON ITS BRANCH
/// ALONE ([`stashes_on`] over the lossy branch and stash reads): beside another
/// worktree's stash whose message is Latin-1 (`caf\xe9`), a clean pushed
/// worktree reads `Checked` with `remote=true`, as that stash is another
/// session's, and the other worktree counts it as its one stash; a stash
/// written on a branch whose name is Latin-1 is counted once `HEAD` names that
/// branch. NEGATIVE CONTROLS: the same stash with its message in UTF-8 reads
/// `Checked` here; and the Latin-1 branch's stash reads `Checked` while `HEAD`
/// names `main`, as it is that branch's alone.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_stash_or_branch_named_in_no_utf8_is_counted_on_its_branch_alone() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let other = tmp.path().join("other");
    let other_s = other.to_str().expect("utf-8");
    scratch_git(&work, &["worktree", "add", "-q", "-b", "side", other_s]);
    std::fs::write(other.join("a.txt"), "side\n").expect("edit");
    scratch_git(&other, &["stash", "push", "-q", "-m", "cafe"]);
    let clean = |work: &Path, what: &str| {
        let read = probe(work, Some(work), PROBE_LIMIT);
        assert!(
            read.checked().is_some_and(|at| at.remote),
            "{what}: {read:?}"
        );
    };
    clean(&work, "the control: another worktree's stash in UTF-8");
    let log = work.join(".git/logs/refs/stash");
    rewrite_last(&log, b"cafe", b"caf\xe9");
    clean(&work, "another worktree's stash, its message Latin-1");
    let w = found(&other).expect("its own stash");
    assert_eq!((w.stashes, w.changed), (1, 0), "{w:?}");
    std::fs::write(work.join("a.txt"), "here\n").expect("edit");
    scratch_git(&work, &["stash", "-q"]);
    rewrite_last(&log, b"WIP on main: ", b"WIP on caf\xe9: ");
    clean(
        &work,
        "the control: the Latin-1 branch's stash, HEAD on main",
    );
    let head = scratch_git(&work, &["rev-parse", "HEAD"]);
    let packed = work.join(".git/packed-refs");
    let mut refs: Vec<u8> = std::fs::read(&packed)
        .unwrap_or_default()
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .flat_map(|line| line.iter().copied().chain(*b"\n"))
        .collect();
    refs.extend_from_slice(head.trim().as_bytes());
    refs.extend_from_slice(b" refs/heads/caf\xe9\n");
    std::fs::write(&packed, refs).expect("packed-refs");
    std::fs::write(work.join(".git/HEAD"), b"ref: refs/heads/caf\xe9\n").expect("HEAD");
    let w = found(&work).expect("the stash on the branch HEAD names");
    assert_eq!((w.stashes, w.changed, w.unpushed), (1, 0, 0), "{w:?}");
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
    // A FOLDER GONE UNDER A GUARDED ONE IS NEVER READ GONE (§14.17 item 4, slice 9c's review
    // round): `gone_dir` checks the path, then where its links lead, before it reads an entry, so
    // a folder deleted under ~/Documents, or reached through a link into it, reads as nothing to
    // name (`RepoRead::Clean`), never `Gone`; the same folder missing outside every guarded one
    // reads gone. NEGATIVE CONTROLS (run by hand): dropping the check of where the links lead
    // fails the link's row; dropping both checks fails the folder's own.
    let gone_in_docs = home.join("Documents/gone/proj");
    assert_eq!(gone_dir(&gone_in_docs, &home, &table), None);
    assert_eq!(
        probe(&gone_in_docs, Some(&home), PROBE_LIMIT),
        RepoRead::Clean
    );
    let into_docs = home.join("Developer/into-docs");
    std::os::unix::fs::symlink(home.join("Documents/gone"), &into_docs)
        .expect("owned dangling link into Documents");
    assert_eq!(gone_dir(&into_docs.join("proj"), &home, &table), None);
    let gone_here = home.join("Developer/gone/proj");
    assert_eq!(
        gone_dir(&gone_here, &home, &table),
        Some(printable(&gone_here.to_string_lossy()))
    );
    assert_eq!(
        unattended_dir(&home.join("documents/work"), Some(&home), &table),
        None
    );
    if home.join("DOCUMENTS").exists() {
        assert_eq!(
            unattended_dir(&home.join("developer/work"), Some(&home), &table),
            Some(work)
        );
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

/// A present session branch whose commit Git cannot read is unknown work, not a branch the
/// session deleted. Controls: the branch at a readable, landed commit and the branch after its
/// deletion both read clean.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_session_branch_with_an_unreadable_commit_is_no_deleted_branch() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let used = Used {
        since: None,
        branches: vec!["parked".to_string()],
    };
    let read = || probe_session(&work, Some(&work), &used, PROBE_LIMIT);
    scratch_git(&work, &["branch", "parked"]);
    let landed = scratch_git(&work, &["rev-parse", "refs/heads/parked"]);
    assert!(read().checked().is_some(), "the landed branch");
    let path = work.join(".git/refs/heads/parked");
    std::fs::write(&path, "0000000000000000000000000000000000000001\n")
        .expect("a ref whose object is missing");
    assert_eq!(scratch_git(&work, &["status", "--porcelain"]), "");
    assert_eq!(
        read(),
        RepoRead::Unread,
        "the present but unreadable branch"
    );
    std::fs::write(&path, landed).expect("restore the landed branch");
    assert!(read().checked().is_some(), "the readable branch again");
    scratch_git(&work, &["branch", "-D", "parked"]);
    assert!(read().checked().is_some(), "a branch since deleted");
}

/// On a Mac without Apple's Command Line Tools the probe runs no `git` that is Apple's stub, which
/// would raise their install dialog: it reads as no git, quietly. A person's own `git` earlier on
/// PATH still runs, and the stub runs git once the developer tools' git is there. Proved on a
/// stand-in directory whose `git` is never run.
#[cfg(target_os = "macos")]
#[test]
fn without_the_command_line_tools_the_probe_runs_no_stub_git() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let stubs = scratch.path().join("usr-bin");
    let own = scratch.path().join("own-bin");
    let empty = scratch.path().join("empty");
    for dir in [&stubs, &own, &empty] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(stubs.join("git"), b"never run\n").unwrap();
    std::fs::write(own.join("git"), b"never run\n").unwrap();
    for git in [stubs.join("git"), own.join("git")] {
        std::fs::set_permissions(git, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap();
    let missing = || false;
    let present = || true;
    assert!(git_to_run(&path(&[&empty, &stubs]), scratch.path(), &stubs, missing).is_none());
    assert!(git_to_run(&path(&[&empty, &stubs]), scratch.path(), &stubs, present).is_some());
    assert!(git_to_run(&path(&[&own, &stubs]), scratch.path(), &stubs, missing).is_some());
    assert!(git_to_run(&path(&[&stubs, &own]), scratch.path(), &stubs, missing).is_none());
    assert_eq!(
        git_to_run(&path(&[&empty]), scratch.path(), &stubs, || panic!(
            "no stub to ask about"
        )),
        None,
        "no git at all runs nothing, not /usr/bin/git in its place"
    );
}

/// The `git` the check judged is the one the read runs, by its absolute path: a person's own
/// `git` earlier on PATH (Homebrew's, an atpkg build, aterm's own shim) runs, never
/// `/usr/bin/git` behind it, which is Apple's stub on a Mac without the Command Line Tools and
/// raises their install dialog. Proved on stand-in directories whose `git` is never run.
#[cfg(target_os = "macos")]
#[test]
fn the_git_judged_is_the_git_that_runs() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let stubs = root.join("usr-bin");
    let own = root.join("own-bin");
    for dir in [&stubs, &own] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("git"), b"never run\n").unwrap();
        std::fs::set_permissions(dir.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap();
    let runs = |dirs: &[&Path], developer_git: bool| {
        git_command(&path(dirs), &root, &stubs, || developer_git)
            .map(|cmd| PathBuf::from(cmd.get_program()))
    };
    assert_eq!(
        runs(&[&own, &stubs], false),
        Some(own.join("git")),
        "the person's own git, judged and run, with the developer tools missing"
    );
    assert_eq!(runs(&[&stubs, &own], false), None, "the stub runs nowhere");
    assert_eq!(runs(&[&stubs, &own], true), Some(stubs.join("git")));
    assert_eq!(
        git_to_run(&path(&[&own, &stubs]), &root, &stubs, || false),
        Some(own.join("git"))
    );
    assert_eq!(
        git_to_run(&path(&[&stubs, &own]), &root, &stubs, || true),
        Some(stubs.join("git")),
        "the stub, once the developer tools' git is there"
    );
    assert_eq!(
        git_to_run(
            &path(&[Path::new("own-bin"), &stubs]),
            &root,
            &stubs,
            || false
        ),
        Some(own.join("git")),
        "a relative entry runs by its absolute path"
    );
}

/// The probe must skip a non-executable PATH file just as the spawn does, then refuse the
/// stub it would actually reach. These are owned text fixtures, never programs we execute.
#[cfg(target_os = "macos")]
#[test]
fn git_probe_passes_nonexecutable_files_before_refusing_the_stub() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let stubs = scratch.path().join("stubs");
    let own = scratch.path().join("own");
    let denied = scratch.path().join("denied");
    for (dir, mode) in [(&stubs, 0o755), (&own, 0o755), (&denied, 0o600)] {
        std::fs::create_dir(dir).unwrap();
        let git = dir.join("git");
        std::fs::write(&git, b"metadata only, never run\n").unwrap();
        std::fs::set_permissions(git, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let path = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap();
    assert!(git_to_run(&path(&[&own, &stubs]), scratch.path(), &stubs, || false).is_some());
    assert!(git_to_run(&path(&[&stubs]), scratch.path(), &stubs, || true).is_some());
    assert!(
        git_to_run(&path(&[&denied, &stubs]), scratch.path(), &stubs, || false).is_none(),
        "a non-executable first file cannot disguise the later Apple stub"
    );
}

/// Relative and empty entries use the child's folder; directory/file aliases retain the
/// selected stub's identity. Every fixture is private text and every decision is metadata.
#[cfg(target_os = "macos")]
#[test]
fn git_probe_resolves_relative_empty_and_alias_entries_in_the_childs_folder() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let stubs = scratch.path().join("stubs");
    let own = scratch.path().join("own");
    let child = scratch.path().join("child");
    let empty_stub = scratch.path().join("empty-stub");
    let empty_own = scratch.path().join("empty-own");
    let linked_file = scratch.path().join("linked-file");
    for dir in [&stubs, &own, &child, &empty_stub, &empty_own, &linked_file] {
        std::fs::create_dir(dir).unwrap();
    }
    for git in [stubs.join("git"), own.join("git"), empty_own.join("git")] {
        std::fs::write(&git, b"metadata only, never run\n").unwrap();
        std::fs::set_permissions(git, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    symlink(stubs.join("git"), empty_stub.join("git")).unwrap();
    symlink(stubs.join("git"), linked_file.join("git")).unwrap();
    let linked_dir = scratch.path().join("linked-dir");
    symlink(&stubs, &linked_dir).unwrap();
    let hardlinked = scratch.path().join("hardlinked");
    std::fs::create_dir(&hardlinked).unwrap();
    std::fs::hard_link(stubs.join("git"), hardlinked.join("git")).unwrap();
    let path = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap();
    assert!(git_to_run(&path(&[&own, &stubs]), &child, &stubs, || false).is_some());
    let mut wrong = Vec::new();
    for (name, dirs, cwd, want) in [
        ("relative stub", vec![Path::new("../stubs")], &child, false),
        (
            "relative own before stub",
            vec![Path::new("../own"), &stubs],
            &child,
            true,
        ),
        ("empty stub", vec![Path::new(""), &own], &empty_stub, false),
        (
            "empty own before stub",
            vec![Path::new(""), &stubs],
            &empty_own,
            true,
        ),
        (
            "directory alias",
            vec![linked_dir.as_path(), &own],
            &child,
            false,
        ),
        (
            "file alias",
            vec![linked_file.as_path(), &own],
            &child,
            false,
        ),
        (
            "hardlink alias",
            vec![hardlinked.as_path(), &own],
            &child,
            false,
        ),
    ] {
        let got = git_to_run(&path(&dirs), cwd, &stubs, || false).is_some();
        if got != want {
            wrong.push((name, got, want));
        }
    }
    assert!(
        wrong.is_empty(),
        "Git selection differs from its child search: {wrong:?}"
    );
}

#[test]
fn worktree_branch_reads_attached_short_branch_flags() {
    assert_eq!(
        worktree_branch(&["-bfeat-attached", "../wt"]),
        Some("feat-attached".to_string())
    );
    assert_eq!(
        worktree_branch(&["-Breset-attached", "../wt"]),
        Some("reset-attached".to_string())
    );
}
