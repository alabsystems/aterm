// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::user;
use crate::port::{Fake, Live};
use std::path::PathBuf;

fn fake() -> (Fake, User) {
    let mut sys = Fake::root();
    sys.euid = 501;
    let who = user("alice", 501);
    sys.executable(crate::items::github::CLT_GIT, b"fixture git");
    (sys, who)
}

/// A git that takes `--no-lazy-fetch` is asked that alone: no scratch folder, no repository. Its
/// reads carry the option; a git that obeys the variable alone gets no option it would refuse.
/// Both carry the variable.
#[test]
fn a_git_that_takes_the_option_is_asked_nothing_more() {
    let (sys, who) = fake();
    let asked = git(&sys, &who, None).args(["--no-lazy-fetch", "--version"]);
    sys.on(&asked.display(), Ran::ok("git version 2.56.0\n"));
    assert_eq!(
        NoLazyFetch::of(&sys, &who).unwrap(),
        Some(NoLazyFetch::Flag)
    );
    assert_eq!(sys.calls(), [asked.display()]);
    let at = Path::new("/Users//alice/work");
    for (off, flagged) in [(NoLazyFetch::Flag, true), (NoLazyFetch::Variable, false)] {
        let read = off.git(&sys, &who, Some(at)).display();
        assert_eq!(read.contains("--no-lazy-fetch"), flagged, "{read}");
        assert!(read.contains("GIT_NO_LAZY_FETCH=1"), "{read}");
        assert!(
            read.ends_with(&format!(
                "-C {}{}",
                at.display(),
                if flagged { " --no-lazy-fetch" } else { "" }
            )),
            "{read}"
        );
    }
}

/// A git that refuses the option is judged by its read under the variable, alone, in the
/// question's own scratch folder, which is gone after it, with no system or personal git config
/// and in English: a complete trace and a working fetch control establish the quiet refusal.
/// NEGATIVE CONTROLS: the same git whose read fetched or attempted to fetch the object is
/// refused, as is a read a signal ended: a git is admitted neither for
/// refusing the option nor for a failed read alone.
#[test]
fn a_git_that_refuses_the_option_is_judged_by_what_its_read_did() {
    for (obeys, expected) in [(true, Some(NoLazyFetch::Variable)), (false, None)] {
        let (sys, who) = fake();
        sys.simulate(move |sys, command| before_the_option(sys, command, obeys));
        assert_eq!(NoLazyFetch::of(&sys, &who).unwrap(), expected, "{obeys}");
        let calls = sys.calls();
        let scratch = Fake::scratch_path(None, LABEL, 0);
        assert!(
            calls[0].ends_with(" --no-lazy-fetch --version"),
            "{calls:?}"
        );
        assert_eq!(calls[1], format!("scratch allocated {}", scratch.display()));
        assert_eq!(
            calls.last().unwrap(),
            &format!("scratch closed {}", scratch.display())
        );
        assert!(
            calls[2..calls.len() - 1]
                .iter()
                .filter(|call| !call.starts_with("write "))
                .all(
                    |call| call.contains(&scratch.to_string_lossy().into_owned())
                        && call.contains("GIT_CONFIG_NOSYSTEM=1")
                        && call.contains("GIT_CONFIG_GLOBAL=/dev/null")
                        && call.contains("LC_ALL=C")
                ),
            "{calls:?}"
        );
        let read = calls
            .iter()
            .find(|call| call.contains(" cat-file "))
            .unwrap();
        assert!(
            read.contains(&format!(" cat-file -t {OBJECT_NAMED}"))
                && read.contains("GIT_NO_LAZY_FETCH=1")
                && read.contains(&format!(
                    "remote.origin.url={}",
                    scratch.join("promisor.git").display()
                ))
                && read.contains(&format!(
                    "--git-dir {}",
                    scratch.join("partial.git").display()
                )),
            "{read}"
        );
        assert!(
            sys.stat(&scratch).is_err(),
            "the question's scratch folder is gone after it"
        );
    }
}

/// A question git could not be asked is an error, never a verdict either way: a `--version` that
/// did not run, a probe whose runs did not, and a probe whose setup step ran and failed (a full
/// or unwritable temporary folder, say) or named no object, which is named with what git said
/// and reads nothing. So a converge pass says why and tries again rather than calling the git
/// incapable. NEGATIVE CONTROL: a read that ran and failed while attempting a fetch is
/// git's answer, a refusal (`None`), not an error.
#[test]
fn a_question_that_could_not_be_asked_or_set_up_is_an_error_never_a_verdict() {
    let (sys, who) = fake();
    assert!(NoLazyFetch::of(&sys, &who).is_err(), "no --version answer");
    let (sys, who) = fake();
    sys.simulate(|_, command| {
        command
            .args
            .iter()
            .any(|arg| arg == "--no-lazy-fetch")
            .then(|| Ran::exit(129, "unknown option: --no-lazy-fetch"))
    });
    assert!(NoLazyFetch::of(&sys, &who).is_err(), "no probe answer");
    let answering = |step: &'static str, answer: Ran| {
        let (sys, who) = fake();
        sys.simulate(move |sys, command| {
            if command.args.iter().any(|arg| arg == step) {
                if step == "cat-file" {
                    record_trace(sys, command, &answer, true);
                }
                return Some(answer.clone());
            }
            before_the_option(sys, command, true)
        });
        let of = NoLazyFetch::of(&sys, &who);
        let read = sys.calls().iter().any(|call| call.contains(" cat-file "));
        let scratch = Fake::scratch_path(None, LABEL, 0);
        assert!(
            sys.stat(&scratch).is_err(),
            "{step}: the scratch folder is gone"
        );
        (of, read)
    };
    for (step, answer, named) in [
        (
            "init",
            Ran::exit(
                128,
                "fatal: cannot mkdir partial.git: No space left on device\n",
            ),
            "git init failed (exit 128: fatal: cannot mkdir partial.git: No space left on device)",
        ),
        (
            "hash-object",
            Ran::exit(128, "fatal: not a git repository"),
            "git hash-object failed (exit 128: fatal: not a git repository)",
        ),
        (
            "hash-object",
            Ran::ok("not an object name\n"),
            "git hash-object named no object",
        ),
    ] {
        let (of, read) = answering(step, answer);
        let why = of.expect_err(step);
        assert_eq!(
            why,
            format!("cannot see whether Git obeys GIT_NO_LAZY_FETCH: {named}"),
            "{step}"
        );
        assert!(!read, "{step}: no read without its question set up");
    }
    let (of, read) = answering(
        "cat-file",
        Ran::exit(128, "fatal: could not fetch from promisor remote"),
    );
    assert_eq!(of, Ok(None), "control: a failed read is git's answer");
    assert!(read);
}

#[test]
fn a_privileged_probe_gives_its_private_scratch_to_the_git_actor_before_running_it() {
    let (mut sys, who) = fake();
    sys.euid = 0;
    sys.on_chown(|sys, path| {
        let mut stat = sys.stat(path).unwrap();
        stat.uid = 501;
        stat.gid = 20;
        sys.stat_of(path.to_str().unwrap(), stat);
    });
    sys.simulate(|sys, command| {
        if command.args.iter().any(|arg| arg == "init") {
            let scratch = Fake::scratch_path(None, LABEL, 0);
            let stat = sys.stat(&scratch).unwrap();
            let actor = command.user.as_ref().unwrap();
            assert_eq!((stat.uid, stat.gid), (actor.uid, actor.gid));
            assert_eq!(stat.mode & 0o777, 0o700);
        }
        before_the_option(sys, command, true)
    });
    assert_eq!(NoLazyFetch::of(&sys, &who), Ok(Some(NoLazyFetch::Variable)));
}

#[test]
fn a_failed_fetch_attempt_is_refused_even_when_a_control_would_succeed() {
    let (sys, who) = fake();
    sys.simulate(|sys, command| {
        let answer = before_the_option(sys, command, true)?;
        if command.args.iter().any(|arg| arg == "cat-file") {
            record_trace(sys, command, &answer, true);
        }
        Some(answer)
    });
    assert_eq!(NoLazyFetch::of(&sys, &who), Ok(None));
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.contains(" cat-file "))
            .count(),
        1
    );
}

#[test]
fn missing_trace_and_failed_controls_defer_the_capability_decision() {
    for fault in ["subject", "control-trace", "control-fetch"] {
        let (sys, who) = fake();
        sys.simulate(move |sys, command| {
            let answer = before_the_option(sys, command, true)?;
            if command.args.iter().any(|arg| arg == "cat-file") {
                let (_, path) = command
                    .env
                    .iter()
                    .rev()
                    .find(|(key, _)| key == "GIT_TRACE2_EVENT")
                    .unwrap();
                let control = path.ends_with("control.trace");
                if (fault == "subject" && !control) || (fault == "control-trace" && control) {
                    sys.file(path, b"");
                }
                if fault == "control-fetch" && control {
                    return Some(Ran::exit(128, "local transport unavailable"));
                }
            }
            Some(answer)
        });
        assert!(NoLazyFetch::of(&sys, &who).is_err(), "{fault}");
    }
}

/// A stand-in for a git before 2.44 over this host's native git: `--no-lazy-fetch` refused as
/// unknown (129), every other run handed to the native git after `prelude`.
fn stand_in(dir: &Path, name: &str, native: &str, prelude: &str) -> PathBuf {
    let path = dir.join(name);
    aterm_tempfile::stand_in(
        &path,
        &format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = --no-lazy-fetch ]; then\n    \
             echo 'unknown option: --no-lazy-fetch' >&2\n    exit 129\n  fi\ndone\n\
             {prelude}exec '{native}' \"$@\"\n"
        ),
    )
    .unwrap();
    path
}

/// The guard selected for this host's real git prevents the probe's local promisor fetch.
/// NEGATIVE CONTROLS, stand-ins over the same git that refuse the option: one drops the
/// variable and fetches the object, so its read succeeds and names the blob, and is refused;
/// one also bars file transport and fails without evidence that lazy fetching was disabled,
/// and is refused too. A native git admitted for its option need not print the older variable
/// refusal message when the variable alone stops a read.
#[test]
fn a_native_git_guard_prevents_lazy_fetch_and_reads_without_evidence_are_not_admitted() {
    let off = crate::items::github::need_native_git_barring_lazy_fetch();
    let native = crate::items::github::need_native_git();
    let live = Live::new();
    let dir = aterm_tempfile::tempdir().unwrap();
    let obeying = stand_in(dir.path(), "variable-git", native, "");
    let ignoring = stand_in(
        dir.path(),
        "ignoring-git",
        native,
        "unset GIT_NO_LAZY_FETCH\n",
    );
    let barred = stand_in(
        dir.path(),
        "barred-git",
        native,
        "unset GIT_NO_LAZY_FETCH\nset -- -c protocol.file.allow=never \"$@\"\n",
    );
    let refused = live
        .run_complete_stdout(&Cmd::new(&ignoring).args(["--no-lazy-fetch", "--version"]))
        .unwrap();
    assert_eq!(refused.status, Some(129), "control: a git before 2.44");
    let of = |git: &Path| NoLazyFetch::of_git(&live, &|| Cmd::new(git)).unwrap();
    let read = |git: &Path| asked(&live, &|| Cmd::new(git)).unwrap();

    assert_eq!(of(Path::new(native)), Some(off));
    let shown = read(&obeying);
    assert_eq!(
        shown.no_fetch,
        !shown.read.success(),
        "{}",
        shown.read.describe()
    );
    if off == NoLazyFetch::Variable {
        assert!(
            shown.no_fetch,
            "the selected variable guard prevents fetching"
        );
    }
    assert_eq!(
        of(&obeying),
        shown.no_fetch.then_some(NoLazyFetch::Variable)
    );

    assert_eq!(of(&ignoring), None);
    let fetched = read(&ignoring);
    assert!(
        fetched.read.success() && fetched.read.out().trim() == "blob" && !fetched.no_fetch,
        "control: a git that ignores the variable fetches lazily: {}",
        fetched.read.describe()
    );

    assert_eq!(of(&barred), None);
    let tried = read(&barred);
    assert!(
        !tried.read.success() && !tried.no_fetch && tried.read.err().contains("not allowed"),
        "control: a git that tried the promisor and failed: {}",
        tried.read.describe()
    );
}
