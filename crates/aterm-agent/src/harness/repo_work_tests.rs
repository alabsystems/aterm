// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::path::PathBuf;

/// `git <args>` in `dir`, hermetic: no system or global config, a fixed
/// author; its output.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn scratch_git(dir: &Path, args: &[&str]) -> String {
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
                RepoRead::Clean | RepoRead::Checked(_) => {
                    panic!("read {k} cut: read as clean over {whole:?}")
                }
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

/// A CLEAN REPOSITORY READS ITS HEAD AND MAIN ([`RepoRead::Checked`],
/// `docs/DESIGN-harness-autopilot-2026-10-15.md` §14.9, slice 9a): over a
/// repository pushed to a bare origin, the probe that finds nothing names the
/// repository, the commit `HEAD` names (the one `git rev-parse HEAD` prints,
/// and the next one once that is pushed), `origin/main` and a remote; it is
/// clean, finds nothing and is the same as a clean reading. CONTROLS: the
/// same repository with no remote reads `remote=false` and no main; a
/// repository with no commit yet, a plain folder and a deleted folder name
/// nothing and read [`RepoRead::Clean`].
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
    let unborn = tmp.path().join("unborn");
    std::fs::create_dir_all(&unborn).expect("unborn");
    scratch_git(&unborn, &["init", "-q", "."]);
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).expect("plain");
    let gone = tmp.path().join("gone");
    std::fs::create_dir_all(&gone).expect("gone");
    std::fs::remove_dir(&gone).expect("deleted");
    for (dir, what) in [
        (&unborn, "no commit yet"),
        (&plain, "a plain folder"),
        (&gone, "a deleted folder"),
    ] {
        assert_eq!(
            probe(dir, Some(tmp.path()), PROBE_LIMIT),
            RepoRead::Clean,
            "{what}"
        );
    }
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
    let mut seen = String::new();
    for k in 0..24 {
        read_stall::set_after(k, starved);
        let read = probe(&work, Some(&work), PROBE_LIMIT);
        read_stall::set(0);
        seen.push(match read {
            RepoRead::Unread => 'u',
            RepoRead::Clean => 'c',
            RepoRead::Checked(_) => 'k',
            RepoRead::Found(w) => panic!("read {k} cut: found {w:?} in a clean repository"),
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
    assert!(
        probe(&work, Some(&work), PROBE_LIMIT).checked().is_some(),
        "the control: nothing starved"
    );
}

/// A CLEAN READING OVER A COUNT GIT REFUSED NAMES NOTHING (§14.9, slice 9a;
/// for 9b's receipt): over a clean pushed repository whose remote-tracking
/// refs `for-each-ref` cannot print as text (a ref named in bytes that are no
/// UTF-8, packed), that read answers nothing without a cut, so the probe
/// counts no remote and reads [`RepoRead::Clean`], naming nothing, never
/// `Checked` with `remote=false`, a remote it did not read as absent.
/// CONTROL: the same repository before that ref reads `Checked` with
/// `remote=true`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_clean_reading_over_a_count_git_refused_names_nothing() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let work = repo(tmp.path(), true);
    let at = probe(&work, Some(&work), PROBE_LIMIT);
    assert!(
        at.checked().is_some_and(|at| at.remote),
        "the control: {at:?}"
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
    refs.extend_from_slice(b" refs/remotes/o/\xff\n");
    std::fs::write(&packed, refs).expect("packed-refs");
    assert_eq!(
        probe(&work, Some(&work), PROBE_LIMIT),
        RepoRead::Clean,
        "a refused remote read names nothing"
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
