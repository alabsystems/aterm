// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`guard`] asks a git program once per file and environment, remembers only a decided answer,
//! and runs it [`Hermetic`]ally.

use super::*;
use std::path::{Path, PathBuf};

/// The first git on `PATH`, which the stand-ins run.
fn native() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    std::env::split_paths(&path)
        .map(|dir| dir.join(if cfg!(windows) { "git.exe" } else { "git" }))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH")
}

/// A stand-in for a git before 2.45 over [`native`] at `dir/name`: `--no-lazy-fetch` refused as
/// unknown (129), every run's arguments written to the log beside it, and every other run handed
/// to the native git after `prelude`. Returns the stand-in and its log.
fn before_the_option(dir: &Path, name: &str, prelude: &str) -> (PathBuf, PathBuf) {
    let path = dir.join(name);
    let log = dir.join(format!("{name}.log"));
    aterm_tempfile::stand_in(
        &path,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\nfor arg in \"$@\"; do\n  if [ \
             \"$arg\" = --no-lazy-fetch ]; then\n    echo 'unknown option: --no-lazy-fetch' \
             >&2\n    exit 129\n  fi\ndone\n{prelude}exec '{native}' \"$@\"\n",
            log = log.display(),
            native = native().display(),
        ),
    )
    .expect("stand-in");
    (path, log)
}

/// How many runs the stand-in's log holds with exactly these arguments.
fn runs(log: &Path, args: &str) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|run| *run == args)
        .count()
}

/// A git that refuses `--no-lazy-fetch` and obeys the variable is guarded by the variable alone,
/// asked once in the process however many guards follow. NEGATIVE CONTROL: the same stand-in run
/// with the option is refused as unknown, so the answer is not the option's.
#[test]
fn a_git_before_the_option_is_guarded_by_the_variable_and_asked_once() {
    let dir = aterm_tempfile::tempdir().expect("stand-in folder");
    let (older, log) = before_the_option(dir.path(), "older-git", "");
    for _ in 0..3 {
        assert_eq!(
            guard(&Hermetic::new(&older)),
            Ok(NoLazyFetch::Variable),
            "this host's git obeys the variable"
        );
    }
    assert_eq!(runs(&log, "--no-lazy-fetch --version"), 1, "asked once");
    let refused = aterm_uds::spawnfd::command(&older)
        .args(["--no-lazy-fetch", "--version"])
        .output()
        .expect("stand-in ran");
    assert_eq!(
        refused.status.code(),
        Some(129),
        "control: a git before 2.45"
    );
}

/// A git that refuses `--no-lazy-fetch` and ignores the variable is refused by name, with the
/// gits that keep a read local, and that decided answer is remembered: asked once. NEGATIVE
/// CONTROL: the same stand-in that keeps the variable is guarded
/// ([`a_git_before_the_option_is_guarded_by_the_variable_and_asked_once`]), so the refusal rests
/// on what its read did.
#[test]
fn a_git_that_ignores_the_variable_is_refused_by_name_and_remembered() {
    let dir = aterm_tempfile::tempdir().expect("stand-in folder");
    let (ignoring, log) =
        before_the_option(dir.path(), "ignoring-git", "unset GIT_NO_LAZY_FETCH\n");
    assert_eq!(
        NoLazyFetch::of(&Hermetic::new(&ignoring)),
        Ok(None),
        "its answer: it fetched"
    );
    let asked_before = runs(&log, "--no-lazy-fetch --version");
    for _ in 0..2 {
        let refused = guard(&Hermetic::new(&ignoring));
        let Err(Unguarded::Fetches(why)) = &refused else {
            panic!("a git whose read fetched is refused: {refused:?}");
        };
        for named in [
            "git version ",
            "neither takes --no-lazy-fetch (git 2.45 and later)",
            "nor obeys GIT_NO_LAZY_FETCH=1",
            "2.39.4",
            "2.44.1",
            "CVE-2024-32465",
        ] {
            assert!(why.contains(named), "{named:?} in {why}");
        }
    }
    assert_eq!(
        runs(&log, "--no-lazy-fetch --version") - asked_before,
        1,
        "the refusal is remembered"
    );
}

/// A question that could not be asked is not remembered: the stand-in's first `init` fails, so
/// the first guard is [`Unguarded::Unasked`], and the next guard asks again and is answered.
/// NEGATIVE CONTROL (run by mutation): remembering the unasked question gives the second guard
/// the first one's error, and every later read of a long run would refuse on one transient
/// failure.
#[test]
fn an_unasked_question_is_asked_again() {
    let dir = aterm_tempfile::tempdir().expect("stand-in folder");
    let mark = dir.path().join("failed-once");
    let (flaky, log) = before_the_option(
        dir.path(),
        "flaky-git",
        &format!(
            "case \" $* \" in *\" init \"*)\n  if [ ! -e '{mark}' ]; then\n    : > '{mark}'\n    \
             echo 'fatal: cannot make the repository' >&2\n    exit 128\n  fi\n  ;;\nesac\n",
            mark = mark.display(),
        ),
    );
    let first = guard(&Hermetic::new(&flaky));
    assert!(
        matches!(&first, Err(Unguarded::Unasked(why)) if why.contains("git init failed")),
        "the first question cannot be asked: {first:?}"
    );
    assert_eq!(
        guard(&Hermetic::new(&flaky)),
        Ok(NoLazyFetch::Variable),
        "the next guard asks again"
    );
    assert_eq!(runs(&log, "--no-lazy-fetch --version"), 2, "asked twice");
}

/// A git replaced at the path an answer was asked of is asked again: a stand-in that obeys the
/// variable answers [`NoLazyFetch::Variable`], and one that ignores it, put at the same path, is
/// refused. NEGATIVE CONTROL: an answer remembered by the path alone gives the second git the
/// first one's `Variable`, and its reads would run unguarded.
#[test]
fn a_git_replaced_at_its_path_is_asked_again() {
    let dir = aterm_tempfile::tempdir().expect("stand-in folder");
    let (git, _) = before_the_option(dir.path(), "replaced-git", "");
    assert_eq!(
        guard(&Hermetic::new(&git)),
        Ok(NoLazyFetch::Variable),
        "the first git"
    );
    let next = dir.path().join("next");
    std::fs::create_dir(&next).expect("next folder");
    let (ignoring, _) = before_the_option(&next, "replaced-git", "unset GIT_NO_LAZY_FETCH\n");
    std::fs::rename(&ignoring, &git).expect("replaced");
    let answer = guard(&Hermetic::new(&git));
    assert!(
        matches!(&answer, Err(Unguarded::Fetches(why)) if why.contains("nor obeys GIT_NO_LAZY_FETCH")),
        "the replacement is asked again: {answer:?}"
    );
}

/// Every run carries the program's declared environment, and the same program under another
/// declared environment is another question. NEGATIVE CONTROL: the run without it does not see
/// the declared value.
#[test]
fn the_declared_environment_reaches_every_run_and_is_part_of_the_question() {
    let dir = aterm_tempfile::tempdir().expect("stand-in folder");
    let seen = dir.path().join("seen");
    let (git, log) = before_the_option(
        dir.path(),
        "declared-git",
        &format!(
            "printf '%s %s\\n' \"${{ATERM_DECLARED-unset}}\" \"$GIT_CONFIG_NOSYSTEM\" >> '{seen}'\n",
            seen = seen.display(),
        ),
    );
    let declared = [("ATERM_DECLARED".to_owned(), "set".to_owned())];
    let ran = Hermetic::with_env(&git, &declared)
        .run(&[OsStr::new("--version")], &[])
        .expect("ran");
    assert!(ran.success(), "{}", ran.describe());
    let bare = Hermetic::new(&git)
        .run(&[OsStr::new("--version")], &[])
        .expect("ran");
    assert!(bare.success(), "{}", bare.describe());
    assert_eq!(
        std::fs::read_to_string(&seen).expect("seen"),
        "set 1\nunset 1\n",
        "the declared value, then the control without it"
    );
    assert_eq!(
        guard(&Hermetic::with_env(&git, &declared)),
        Ok(NoLazyFetch::Variable)
    );
    assert_eq!(guard(&Hermetic::new(&git)), Ok(NoLazyFetch::Variable));
    assert_eq!(
        runs(&log, "--no-lazy-fetch --version"),
        2,
        "one question per environment"
    );
}

/// A `GIT_*` variable this process holds, in any case, reaches no run, and the program's declared
/// `GIT_*` does even where this process holds the same name: a run's command removes what is
/// inherited before it sets what is declared, and a variable that is not git's is left alone.
#[test]
fn an_inherited_git_variable_reaches_no_run_and_a_declared_one_does() {
    let declared = [("GIT_EXEC_PATH".to_owned(), "/declared/libexec".to_owned())];
    let cmd = Hermetic::with_env(Path::new("/declared/bin/git"), &declared).command(
        &[OsStr::new("--version")],
        &[],
        ["GIT_DIR", "git_trace2_event", "GIT_EXEC_PATH", "PATH"].map(OsString::from),
    );
    let envs: BTreeMap<OsString, Option<OsString>> = cmd
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
        .collect();
    let edit = |key: &str| envs.get(OsStr::new(key)).cloned();
    assert_eq!(
        edit("GIT_DIR"),
        Some(None),
        "an inherited GIT_DIR is removed"
    );
    assert_eq!(edit("git_trace2_event"), Some(None), "in any case");
    assert_eq!(
        edit("GIT_EXEC_PATH"),
        Some(Some(OsString::from("/declared/libexec"))),
        "the declared GIT_EXEC_PATH is set after the removal"
    );
    assert_eq!(edit("PATH"), None, "a variable not git's is not edited");
    assert_eq!(edit("GIT_CONFIG_NOSYSTEM"), Some(Some(OsString::from("1"))));
}
