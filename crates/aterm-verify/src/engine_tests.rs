// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::ffi::OsString;
use std::path::Path;

use super::{
    Commit, Kind, Named, PATIENCE, REMOTE_PATIENCE, bounded, bounded_remote_with, engine_copy,
    named, native, release_dir,
};

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = aterm_uds::fixture_git::git();
    cmd.current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Andrew Yates")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Andrew Yates")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args);
    let out = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

/// A bare engine origin at `<dir>/origin.git` whose main holds `baseline/rows.txt` as `text`.
fn origin(dir: &Path, text: &str) -> std::path::PathBuf {
    let work = dir.join("origin-work");
    write(&work.join("baseline/rows.txt"), text);
    git(dir, &["init", "-q", "-b", "main", "origin-work"]);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "baseline"]);
    git(dir, &["clone", "-q", "--bare", "origin-work", "origin.git"]);
    dir.join("origin.git")
}

/// The origin's main moved on: `text` committed and pushed from its work tree.
fn advance(dir: &Path, text: &str) -> String {
    let work = dir.join("origin-work");
    write(&work.join("baseline/rows.txt"), text);
    git(&work, &["commit", "-q", "-am", "moved"]);
    git(&work, &["push", "-q", "../origin.git", "HEAD:main"]);
    git(&work, &["rev-parse", "HEAD"])
}

fn env(pairs: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let pairs: Vec<(String, OsString)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.as_os_str().to_os_string()))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

/// A NAMED ENGINE IS ITS HEAD COMMIT, NEVER ITS WORKING TREE: `PUBLICATION_ENGINE` names a
/// checkout whose baseline holds an uncommitted row and an untracked file; the reader judges
/// what HEAD committed, and an untracked file is absent. NEGATIVE CONTROL: the working tree
/// itself holds the uncommitted row (which the dev checks read before), and the file is there.
#[test]
fn a_named_engine_is_its_head_commit_never_its_working_tree() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    origin(dir.path(), "committed\n");
    let engine = dir.path().join("origin-work");
    write(
        &engine.join("baseline/rows.txt"),
        "committed\nuncommitted\n",
    );
    write(&engine.join("registry/new.toml"), "[repos.x]\n");
    let var = env(&[("PUBLICATION_ENGINE", &engine)]);
    let named = named(dir.path(), &var).expect("named");
    assert_eq!(named, Named::Checkout(engine.clone()));
    let commit = Commit::read(&named, &bounded, &native).expect("read");
    assert_eq!(
        commit.text("baseline/rows.txt", &bounded).expect("text"),
        "committed\n"
    );
    assert_eq!(commit.sha(), git(&engine, &["rev-parse", "HEAD"]));
    assert!(commit.shown().contains("at HEAD"), "{}", commit.shown());
    let absent = commit
        .text("registry/new.toml", &bounded)
        .expect_err("an untracked file is no part of the engine");
    assert_eq!(absent.kind, Kind::Absent, "{absent}");
    assert_eq!(
        std::fs::read_to_string(engine.join("baseline/rows.txt")).expect("tree"),
        "committed\nuncommitted\n",
        "NEGATIVE CONTROL: the working tree holds the row HEAD lacks"
    );
}

/// WITH NO ENGINE NAMED, THE ORIGIN'S MAIN IS THE ENGINE, NOT `~/publication`: the home
/// checkout is a commit behind its origin (no release fast-forwards it any more) and dirty;
/// the reader judges the origin's main as it is now. NEGATIVE CONTROL: the home checkout's HEAD
/// and working tree both hold the older text.
#[test]
fn with_no_engine_named_the_origins_main_is_read_never_the_home_checkout() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let bare = origin(dir.path(), "old\n");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    git(
        &home,
        &["clone", "-q", bare.to_str().expect("utf-8"), "publication"],
    );
    let checkout = home.join("publication");
    write(&checkout.join("baseline/rows.txt"), "old\nedited\n");
    let moved = advance(dir.path(), "new\n");
    let repo = dir.path().join("repo");
    git(dir.path(), &["init", "-q", "-b", "main", "repo"]);
    let var = env(&[("HOME", &home)]);
    let named = named(&repo, &var).expect("named");
    assert_eq!(
        named,
        Named::Origin {
            url: bare.display().to_string(),
            by: checkout.clone(),
        }
    );
    let commit = Commit::read(&named, &bounded, &native).expect("read");
    assert_eq!(commit.sha(), moved);
    assert_eq!(
        commit.text("baseline/rows.txt", &bounded).expect("text"),
        "new\n"
    );
    assert!(commit.shown().contains("'s main"), "{}", commit.shown());
    assert_eq!(
        git(&checkout, &["show", "HEAD:baseline/rows.txt"]),
        "old",
        "NEGATIVE CONTROL: the home checkout's HEAD is behind"
    );
}

/// THE RELEASE'S OWN ENGINE NAMES THE ORIGIN BEFORE `~/publication`: where a release of this
/// clone made its engine, its origin is the one read; a home checkout of another origin is not.
#[test]
fn the_release_engine_names_the_origin_before_the_home_checkout() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let bare = origin(dir.path(), "release's\n");
    let repo = dir.path().join("repo");
    git(dir.path(), &["init", "-q", "-b", "main", "repo"]);
    let copy = engine_copy(&release_dir(&repo));
    std::fs::create_dir_all(copy.parent().expect("parent")).expect("mkdir");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            bare.to_str().expect("utf-8"),
            copy.to_str().expect("utf-8"),
        ],
    );
    let home = dir.path().join("home");
    let other = home.join("publication");
    std::fs::create_dir_all(&other).expect("home");
    git(&other, &["init", "-q", "-b", "main"]);
    git(
        &other,
        &["remote", "add", "origin", "/nonexistent/other.git"],
    );
    let var = env(&[("HOME", &home)]);
    assert_eq!(
        named(&repo, &var).expect("named"),
        Named::Origin {
            url: bare.display().to_string(),
            by: copy.clone(),
        }
    );
    std::fs::remove_dir_all(&copy).expect("rm copy");
    assert_eq!(
        named(&repo, &var).expect("named"),
        Named::Origin {
            url: "/nonexistent/other.git".into(),
            by: other.clone(),
        },
        "without the release's engine, the home checkout's origin"
    );
}

/// NOTHING NAMED IS UNNAMED, AND AN ORIGIN THAT CANNOT BE READ IS UNREAD: neither is ever read
/// from somewhere else instead.
#[test]
fn nothing_named_is_unnamed_and_a_dead_origin_is_unread() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    git(dir.path(), &["init", "-q", "-b", "main", "repo"]);
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let var = env(&[("HOME", &home)]);
    let unnamed = named(&repo, &var).expect_err("nothing names an engine");
    assert_eq!(unnamed.kind, Kind::Unnamed, "{unnamed}");
    let dead = Named::Origin {
        url: dir.path().join("gone.git").display().to_string(),
        by: repo,
    };
    let unread = Commit::read(&dead, &bounded, &native)
        .err()
        .expect("a dead origin is not read");
    assert_eq!(unread.kind, Kind::Unread, "{unread}");
}

/// A NAMED ENGINE IS A CHECKOUT OF ITS OWN: `PUBLICATION_ENGINE` naming a directory inside
/// another repository (a release directory that holds no engine yet, under the clone's `.git`)
/// is unread, never that repository's HEAD. NEGATIVE CONTROL: git, asked from that directory,
/// finds the outer repository's commit.
#[test]
fn a_named_directory_inside_another_repository_is_no_engine() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    git(dir.path(), &["init", "-q", "-b", "main", "repo"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "outer"]);
    let inner = repo.join("not-an-engine");
    std::fs::create_dir_all(&inner).expect("mkdir");
    let named = Named::Checkout(inner.clone());
    let unread = Commit::read(&named, &bounded, &native)
        .err()
        .expect("a directory with no .git of its own is not read");
    assert_eq!(unread.kind, Kind::Unread, "{unread}");
    assert!(
        unread.why.contains("no git checkout of its own"),
        "{unread}"
    );
    assert_eq!(
        git(&inner, &["rev-parse", "HEAD"]),
        git(&repo, &["rev-parse", "HEAD"]),
        "NEGATIVE CONTROL: git from there reads the outer repository"
    );
}

/// AN ORIGIN IS REACHED ONLY THROUGH THE CALLER'S IMPORT, never by git here: an import that
/// refuses leaves the engine unread in its own words although the origin is a readable
/// repository, and one that names a commit it did not bring in is refused. NEGATIVE CONTROL:
/// [`native`] imports the same origin, and its main is read.
#[test]
fn an_origin_is_reached_only_through_the_callers_import() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let bare = origin(dir.path(), "imported\n");
    let work = dir.path().join("origin-work");
    let head = git(&work, &["rev-parse", "HEAD"]);
    let named = Named::Origin {
        url: bare.display().to_string(),
        by: work,
    };
    let refused = Commit::read(&named, &bounded, &|_, _| Err("no authority for it".into()))
        .err()
        .expect("an import that refuses reads nothing");
    assert_eq!(refused.kind, Kind::Unread, "{refused}");
    assert!(refused.why.contains("no authority for it"), "{refused}");
    let phantom = Commit::read(&named, &bounded, &|_, _| Ok(head.clone()))
        .err()
        .expect("an import that brought nothing in is refused");
    assert_eq!(phantom.kind, Kind::Unread, "{phantom}");
    assert!(phantom.why.contains("did not bring in"), "{phantom}");
    let read = Commit::read(&named, &bounded, &native)
        .expect("NEGATIVE CONTROL: the native import reads the same origin");
    assert_eq!(read.sha(), head);
    assert_eq!(
        read.text("baseline/rows.txt", &bounded).expect("text"),
        "imported\n"
    );
}

/// A READER'S CHILD ENDS WITH THE VERIFY RUN: once the run on this thread is ending
/// ([`crate::exec::ending`]: an interrupt, or the halt at its first red), [`bounded`] ends the
/// child it is waiting on, which then gives no output, rather than holding the stage until it
/// exits or [`super::PATIENCE`] passes. NEGATIVE CONTROL: the same child, with the run not
/// ending, runs to its end and prints.
#[test]
fn a_readers_child_ends_with_the_verify_run() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let end = Arc::new(AtomicBool::new(false));
    crate::exec::ends::when(Arc::clone(&end), Arc::new(AtomicBool::new(false)));
    let child = || {
        let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("sh"));
        cmd.args(["-c", "printf ran"]);
        cmd
    };
    let ran = bounded(&mut child())
        .expect("NEGATIVE CONTROL: a run that is not ending lets its child finish");
    assert_eq!(ran.stdout, b"ran");
    end.store(true, Ordering::SeqCst);
    let ended = bounded(&mut child()).expect_err("a child of a run that is ending is ended");
    assert!(!ended.to_string().is_empty(), "{ended}");
}

/// AN ENGINE ORIGIN'S AGH CHILD IS PROGRESS-BOUNDED, NOT WALL-BOUNDED: the selector hands the
/// networked child to the progress-aware wait while local Git retains the finite wall hang
/// detector. NEGATIVE CONTROL: routing both through the old local wait changes the first answer.
#[test]
fn an_engine_origin_agh_child_is_progress_bounded() {
    assert_eq!(
        REMOTE_PATIENCE,
        PATIENCE.saturating_add(aterm_types::http_retry::Policy::STANDARD.max_total_pause()),
        "one valid Retry-After plus the next request's hang detector fits"
    );
    let mut command = aterm_uds::spawnfd::command("not-started-by-the-injected-waits");
    let agh = bounded_remote_with(
        crate::remote::Tool::Agh,
        &mut command,
        |_| "wall",
        |_| "progress",
    );
    assert_eq!(agh, "progress");
    let git = bounded_remote_with(
        crate::remote::Tool::Git,
        &mut command,
        |_| "wall",
        |_| "progress",
    );
    assert_eq!(git, "wall");
}

#[test]
fn a_pinned_reader_keeps_its_commit_after_head_and_replacement_refs_move() {
    let dir = aterm_tempfile::tempdir().unwrap();
    origin(dir.path(), "original\n");
    let work = dir.path().join("origin-work");
    let original = git(&work, &["rev-parse", "HEAD"]);
    let git_dir = work.join(".git");
    let selected = Named::Pinned {
        git_dir: git_dir.clone(),
        commit: original.clone(),
    };
    let no_import =
        |_: &Path, _: &Path| -> Result<String, String> { panic!("pinned readers must not import") };
    let reader = Commit::read(&selected, &bounded, &no_import).unwrap();
    let moved = advance(dir.path(), "successor\n");
    git(&work, &["replace", &original, &moved]);
    assert_eq!(reader.sha(), original);
    assert_eq!(
        reader.text("baseline/rows.txt", &bounded).unwrap(),
        "original\n"
    );
    let reopened = Commit::read(&selected, &bounded, &no_import).unwrap();
    assert_eq!(
        reopened.text("baseline/rows.txt", &bounded).unwrap(),
        "original\n"
    );
    let successor = Commit::read(
        &Named::Pinned {
            git_dir,
            commit: moved,
        },
        &bounded,
        &no_import,
    )
    .unwrap();
    assert_eq!(
        successor.text("baseline/rows.txt", &bounded).unwrap(),
        "successor\n"
    );
}

#[test]
fn invalid_or_unavailable_pins_never_fall_back_to_a_readable_checkout() {
    let dir = aterm_tempfile::tempdir().unwrap();
    origin(dir.path(), "fallback must not be read\n");
    let work = dir.path().join("origin-work");
    let oid = git(&work, &["rev-parse", "HEAD"]);
    let git_dir = work.join(".git");
    for (repository, commit) in [
        (Some(git_dir.as_os_str().to_owned()), None),
        (None, Some(OsString::from(&oid))),
        (Some(OsString::new()), Some(OsString::from(&oid))),
        (Some(git_dir.as_os_str().to_owned()), Some(OsString::new())),
        (
            Some(git_dir.as_os_str().to_owned()),
            Some(OsString::from("HEAD")),
        ),
        (
            Some(git_dir.as_os_str().to_owned()),
            Some(OsString::from(&oid[..12])),
        ),
    ] {
        let vars = |name: &str| match name {
            super::GIT_DIR_ENV => repository.clone(),
            super::COMMIT_ENV => commit.clone(),
            "PUBLICATION_ENGINE" => Some(work.as_os_str().to_owned()),
            _ => None,
        };
        assert_eq!(named(dir.path(), &vars).unwrap_err().kind, Kind::Unread);
    }
    let no_import =
        |_: &Path, _: &Path| -> Result<String, String> { panic!("invalid pins cannot fetch") };
    for (repository, commit) in [
        (git_dir.clone(), "0".repeat(40)),
        (dir.path().join("absent.git"), oid),
        (git_dir, git(&work, &["rev-parse", "HEAD^{tree}"])),
    ] {
        assert_eq!(
            Commit::read(
                &Named::Pinned {
                    git_dir: repository,
                    commit
                },
                &bounded,
                &no_import
            )
            .err()
            .unwrap()
            .kind,
            Kind::Unread
        );
    }
}

#[test]
fn origin_snapshot_exposes_only_its_retained_repository_and_imported_identity() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let bare = origin(dir.path(), "snapshot\n");
    let origin = bare.to_str().unwrap();
    let main = super::OriginMain::fetch(origin, &dir.path().join("origin-work"), &bounded, &native)
        .unwrap();
    assert_eq!(main.origin(), origin);
    let selected = Named::Pinned {
        git_dir: main.git_dir(),
        commit: main.commit().to_owned(),
    };
    let no_import =
        |_: &Path, _: &Path| -> Result<String, String> { panic!("snapshot already imported") };
    let reader = Commit::read(&selected, &bounded, &no_import).unwrap();
    assert_eq!(
        reader.text("baseline/rows.txt", &bounded).unwrap(),
        "snapshot\n"
    );
    drop(main);
    assert!(
        reader.text("baseline/rows.txt", &bounded).is_err(),
        "a pathname is not lifetime custody or a fallback grant"
    );
}
