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
/// and in English: one that says it will not fetch lazily obeys the variable. NEGATIVE CONTROLS:
/// the same git whose read fetched the object is refused, and a read that failed without saying
/// lazy fetching is off, or that a signal ended, shows nothing: a git is admitted neither for
/// refusing the option nor for a failed read alone.
#[test]
fn a_git_that_refuses_the_option_is_judged_by_what_its_read_did() {
    for (obeys, expected) in [(true, Some(NoLazyFetch::Variable)), (false, None)] {
        let (sys, who) = fake();
        sys.simulate(move |_, command| before_the_option(command, obeys));
        assert_eq!(NoLazyFetch::of(&sys, &who).unwrap(), expected, "{obeys}");
        let calls = sys.calls();
        let scratch = Fake::scratch_path(None, LABEL, 0);
        assert_eq!(calls.len(), 7, "{calls:?}");
        assert!(
            calls[0].ends_with(" --no-lazy-fetch --version"),
            "{calls:?}"
        );
        assert_eq!(calls[1], format!("scratch allocated {}", scratch.display()));
        assert_eq!(calls[6], format!("scratch closed {}", scratch.display()));
        assert!(
            calls[2..6].iter().all(
                |call| call.contains(&scratch.to_string_lossy().into_owned())
                    && call.contains("GIT_CONFIG_NOSYSTEM=1")
                    && call.contains("GIT_CONFIG_GLOBAL=/dev/null")
                    && call.contains("LC_ALL=C")
            ),
            "{calls:?}"
        );
        let read = &calls[5];
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
    // A read that failed without saying lazy fetching is off (it tried the promisor and could
    // not reach it, say), or that a signal ended, shows nothing.
    assert!(!obeyed(&Ran::exit(
        128,
        "fatal: could not fetch from promisor remote"
    )));
    assert!(obeyed(&Ran::exit(
        128,
        "warning: lazy fetching disabled; some objects may not be available"
    )));
    assert!(!obeyed(&Ran {
        status: None,
        stdout: Vec::new(),
        stderr: b"warning: lazy fetching disabled".to_vec(),
    }));
}

/// A question git could not be asked is an error, never a verdict either way: a `--version` that
/// did not run, a probe whose runs did not, and a probe whose setup step ran and failed (a full
/// or unwritable temporary folder, say) or named no object, which is named with what git said
/// and reads nothing. So a converge pass says why and tries again rather than calling the git
/// incapable. NEGATIVE CONTROL: a read that ran and failed without saying lazy fetching is off is
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
        sys.simulate(move |_, command| {
            if command.args.iter().any(|arg| arg == step) {
                return Some(answer.clone());
            }
            before_the_option(command, true)
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

    let shown = asked(&live, &|| match off {
        NoLazyFetch::Flag => Cmd::new(native).arg("--no-lazy-fetch"),
        NoLazyFetch::Variable => Cmd::new(native),
    })
    .unwrap();
    assert!(
        shown.status.is_some_and(|code| code != 0),
        "the selected {off:?} guard must prevent a lazy fetch: {}",
        shown.describe()
    );

    assert_eq!(of(&ignoring), None);
    let fetched = read(&ignoring);
    assert!(
        fetched.success() && fetched.out().trim() == "blob",
        "control: a git that ignores the variable fetches lazily: {}",
        fetched.describe()
    );

    assert_eq!(of(&barred), None);
    let tried = read(&barred);
    assert!(
        !tried.success() && !tried.err().contains(REFUSED) && tried.err().contains("not allowed"),
        "control: a git that tried the promisor and failed: {}",
        tried.describe()
    );
}
