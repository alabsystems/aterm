// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retirement reads must not hydrate a partial clone while judging its contents.

use super::super::Git;
#[cfg(unix)]
use super::super::GitProgram;
#[cfg(unix)]
use super::super::lazy_fetch::{Hermetic, guard};
use super::{GitCli, for_git, git};
#[cfg(unix)]
use agh::git::lazy_fetch::NoLazyFetch;
use std::path::{Path, PathBuf};

/// A bare partial repository whose promisor, a local bare repository beside it, holds the one
/// object it lacks.
struct Promised {
    _scratch: aterm_tempfile::TempDir,
    partial: PathBuf,
    object: String,
}

impl Promised {
    fn new() -> Self {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("retired-git-promisor-")
            .tempdir()
            .expect("scratch");
        let provider = scratch.path().join("provider.git");
        let partial = scratch.path().join("partial.git");
        for repository in [&provider, &partial] {
            git(
                scratch.path(),
                &["init", "--bare", "--template=", "-q", &for_git(repository)],
            );
        }
        let hashed = GitCli.run(&provider, &["hash-object", "-w", "--no-filters", "HEAD"]);
        assert_eq!(hashed.code, Some(0), "{}", hashed.err);
        let object = hashed.out.trim().to_owned();
        assert!(
            !object.is_empty(),
            "the provider must hold an actual object"
        );
        git(&partial, &["config", "remote.origin.promisor", "true"]);
        git(
            &partial,
            &["config", "remote.origin.url", &for_git(&provider)],
        );
        Self {
            _scratch: scratch,
            partial,
            object,
        }
    }

    /// The read of the promised object. Even a person's URL rewrite cannot make the control
    /// contact another transport.
    fn read(&self) -> [&str; 7] {
        [
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "cat-file",
            "-t",
            &self.object,
        ]
    }

    /// Whether a promisor pack was installed: a fetch happened.
    fn packs(&self) -> usize {
        std::fs::read_dir(self.partial.join("objects/pack"))
            .expect("partial pack directory")
            .count()
    }

    /// The read run by `program` directly, with `GIT_NO_LAZY_FETCH` set to `variable` and no
    /// option, as no retirement read runs.
    fn unguarded(&self, program: &Path, variable: &str) -> std::process::Output {
        aterm_uds::spawnfd::command(program)
            .arg("-C")
            .arg(for_git(&self.partial))
            .args(self.read())
            .env("GIT_NO_LAZY_FETCH", variable)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .expect("unguarded read ran")
    }
}

/// The first git on `PATH`, which the fixtures and the stand-ins run.
fn native() -> PathBuf {
    crate::vendor::first_on_path("git", std::env::var_os("PATH").as_deref()).expect("git on PATH")
}

/// `guarded`'s read of a promised object leaves it missing and installs no pack; the same local
/// provider then supplies it to an unguarded read, run second so it cannot hydrate the guarded
/// read's input in advance; and `guarded` reads it once it is there.
fn never_fetches(guarded: &dyn Git) {
    let promised = Promised::new();
    let read = promised.read();
    let refused = guarded.run(&promised.partial, &read);
    assert!(refused.code.is_some(), "Git must have run: {}", refused.err);
    assert_ne!(
        refused.code,
        Some(0),
        "the promised object must stay missing"
    );
    assert_eq!(
        promised.packs(),
        0,
        "inspection must not install a promisor pack"
    );

    let control = promised.unguarded(&native(), "0");
    assert!(
        control.status.success(),
        "the local provider must work: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    assert_eq!(control.stdout, b"blob\n");
    let hydrated = guarded.run(&promised.partial, &read);
    assert_eq!(hydrated.code, Some(0), "{}", hydrated.err);
    assert_eq!(hydrated.out.trim(), "blob");
}

/// `git` on `PATH`, as retirement runs it: `--no-lazy-fetch` where it takes the option (2.45
/// and later), the variable alone where it refuses the option and was seen to obey it.
#[test]
fn cleanup_git_never_fetches_promised_objects() {
    never_fetches(&GitCli);
}

/// A stand-in for a git before 2.45 over `native`: `--no-lazy-fetch` refused as unknown (129),
/// every other run written to `log` and handed to `native` after `prelude`.
#[cfg(unix)]
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

/// A git that refuses `--no-lazy-fetch` and obeys the variable is run with the variable alone,
/// and its reads leave a promised object missing; it is asked once in the process, however many
/// runs follow. NEGATIVE CONTROL: the same stand-in run with the option is refused as unknown,
/// so a run carrying it, as every read did before, never reaches git's read.
#[cfg(unix)]
#[test]
fn a_git_before_the_option_runs_where_it_was_seen_to_obey_the_variable() {
    let dir = aterm_tempfile::tempdir().expect("stand-in folder");
    let (older, log) = before_the_option(dir.path(), "older-git", "");
    assert_eq!(
        guard(&older),
        Ok(NoLazyFetch::Variable),
        "this host's git obeys the variable"
    );
    never_fetches(&GitProgram(&older));
    let asked = std::fs::read_to_string(&log).expect("stand-in log");
    assert_eq!(
        asked
            .lines()
            .filter(|run| *run == "--no-lazy-fetch --version")
            .count(),
        1,
        "asked once: {asked}"
    );
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

/// A git replaced at the path an answer was asked of is asked again: a stand-in that obeys the
/// variable answers [`NoLazyFetch::Variable`], and one that ignores it, put at the same path, is
/// refused. NEGATIVE CONTROL: an answer remembered by the path alone gives the second git the
/// first one's `Variable`, and its reads would run unguarded.
#[cfg(unix)]
#[test]
fn a_git_replaced_at_its_path_is_asked_again() {
    let dir = aterm_tempfile::tempdir().expect("stand-in folder");
    let (git, _) = before_the_option(dir.path(), "replaced-git", "");
    assert_eq!(guard(&git), Ok(NoLazyFetch::Variable), "the first git");
    let next = dir.path().join("next");
    std::fs::create_dir(&next).expect("next folder");
    let (ignoring, _) = before_the_option(&next, "replaced-git", "unset GIT_NO_LAZY_FETCH\n");
    std::fs::rename(&ignoring, &git).expect("replaced");
    let answer = guard(&git);
    assert!(
        answer
            .as_ref()
            .is_err_and(|why| why.contains("nor obeys GIT_NO_LAZY_FETCH")),
        "the replacement is asked again: {answer:?}"
    );
}

/// A git that refuses `--no-lazy-fetch` and ignores the variable starts no retirement run: its
/// reads are refused unrun, and the promised object stays missing. NEGATIVE CONTROL: that git,
/// run directly under the variable, fetches the object, so the refusal rests on what its read
/// did, not on its refusing the option.
#[cfg(unix)]
#[test]
fn a_git_that_ignores_the_variable_runs_no_read() {
    let dir = aterm_tempfile::tempdir().expect("stand-in folder");
    let (ignoring, _) = before_the_option(dir.path(), "ignoring-git", "unset GIT_NO_LAZY_FETCH\n");
    assert_eq!(
        NoLazyFetch::of(&Hermetic(&ignoring)),
        Ok(None),
        "its answer: it fetched"
    );
    let promised = Promised::new();
    let refused = GitProgram(&ignoring).run(&promised.partial, &promised.read());
    assert_eq!(refused.code, None, "{}", refused.err);
    assert_eq!(promised.packs(), 0, "a refused read fetches nothing");

    let fetched = promised.unguarded(&ignoring, "1");
    assert!(
        fetched.status.success() && fetched.stdout == b"blob\n",
        "control: the stand-in fetches under the variable: {}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    assert_ne!(promised.packs(), 0, "control: the fetch installed a pack");
}
