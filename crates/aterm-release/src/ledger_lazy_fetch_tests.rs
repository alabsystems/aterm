// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::testing::Promised;
use crate::ledger::{GitCli, GitRunner, RunOut};
use agh::git::lazy_fetch::Git as _;
use std::ffi::OsStr;

/// The verdict ([`guard`]) on the git the release runs (the selected git, with its package's
/// environment) is what that git's guarded read does, told apart by whether `partial.git` holds
/// the object afterwards: a guard where the read under it fetched nothing, a refusal naming
/// the gits that keep a read local where it fetched. NEGATIVE CONTROL: the same read with the
/// variable off and no option fetches it, so the pair is a live promisor and the check sees a
/// fetch.
#[test]
fn the_verdict_is_what_a_guarded_read_does() {
    let selected = crate::ledger::selected_git().unwrap();
    let git = shared::Hermetic::with_env(&selected.program, selected.env.entries());
    let verdict = guard(&selected.program, selected.env.entries());
    let read = |pair: &Promised, option: Option<&str>, variable: &str| {
        let promisor = pair.promisor();
        let mut args: Vec<&OsStr> = option.into_iter().map(OsStr::new).collect();
        args.extend([OsStr::new("--git-dir"), pair.partial.as_os_str()]);
        args.extend(promisor.iter().map(OsStr::new));
        args.extend(["cat-file", "-t", &pair.object].map(OsStr::new));
        git.run(&args, &[("GIT_NO_LAZY_FETCH", variable)]).unwrap()
    };
    let pair = Promised::new();
    // A refused git takes no option, so its guarded read is the variable's alone.
    let option = verdict.as_ref().ok().and_then(|guard| guard.option());
    read(&pair, option, "1");
    let fetched = pair.holds();
    match &verdict {
        Ok(guard) => assert!(!fetched, "{guard:?}, yet its guarded read fetched"),
        Err(why) => {
            assert!(
                fetched,
                "refused, yet its guarded read fetched nothing: {why}"
            );
            assert!(
                why.to_string().contains("git 2.45 and later")
                    && why.to_string().contains("aterm pkg install git"),
                "{why}"
            );
        }
    }

    let control = Promised::new();
    let ran = read(&control, None, "0");
    assert!(ran.success(), "{}", ran.describe());
    assert!(control.holds(), "the read without the variable fetches");
}

/// What `read` did on a fresh pair, checked: a local read fetched nothing, and ran (an
/// answer that names the object missing, or a failure) or was refused by name; the plain run
/// fetched it.
fn check(how: &str, ran: crate::ledger::Result<RunOut>, plain: bool, refusal: &str) {
    match ran {
        Ok(out) => assert!(
            plain || !out.success() || out.stdout_utf8().contains("missing"),
            "{how}: {}",
            out.stdout_utf8()
        ),
        Err(error) => assert!(
            !plain && error.to_string().contains(refusal),
            "{how}: {error}"
        ),
    }
}

/// The production runner's local reads fetch nothing, or refuse on a git that cannot keep them
/// local. NEGATIVE CONTROL: its plain run of the same read fetches the object.
#[test]
fn local_reads_fetch_nothing_and_a_plain_run_does() {
    for how in ["read", "read with input", "plain"] {
        let pair = Promised::new();
        assert!(!pair.holds(), "a fresh partial.git lacks the object");
        let promisor = pair.promisor();
        let mut args: Vec<&str> = promisor.iter().map(String::as_str).collect();
        let runner = GitCli::new(&pair.partial);
        let ran = match how {
            "read" => {
                args.extend(["cat-file", "-t", &pair.object]);
                runner.git_read(&args)
            }
            "read with input" => {
                args.extend(["cat-file", "--batch-check"]);
                runner.git_read_input(&args, format!("{}\n", pair.object).as_bytes())
            }
            _ => {
                args.extend(["cat-file", "-t", &pair.object]);
                runner.git(&args)
            }
        };
        check(how, ran, how == "plain", "GIT_NO_LAZY_FETCH");
        assert_eq!(pair.holds(), how == "plain", "{how}");
    }
}

/// A runner that forwards only [`GitRunner::git`] to the production runner (as a wrapper that
/// checks its arguments does) reads nothing unguarded: its reads refuse, since it does not say how
/// they stay local, and fetch nothing. NEGATIVE CONTROL: its `git`, the plain run, fetches the
/// object; a read that fell back to it, as the trait's defaults did, fetched too.
#[test]
fn a_runner_that_forwards_only_git_fetches_nothing_on_a_read() {
    struct Forwards<'a>(&'a dyn GitRunner);
    impl GitRunner for Forwards<'_> {
        fn git(&self, args: &[&str]) -> crate::ledger::Result<RunOut> {
            self.0.git(args)
        }
    }
    for how in ["read", "read with input", "plain"] {
        let pair = Promised::new();
        let promisor = pair.promisor();
        let mut args: Vec<&str> = promisor.iter().map(String::as_str).collect();
        let inner = GitCli::new(&pair.partial);
        let runner = Forwards(&inner);
        let ran = match how {
            "read" => {
                args.extend(["cat-file", "-t", &pair.object]);
                runner.git_read(&args)
            }
            "read with input" => {
                args.extend(["cat-file", "--batch-check"]);
                runner.git_read_input(&args, format!("{}\n", pair.object).as_bytes())
            }
            _ => {
                args.extend(["cat-file", "-t", &pair.object]);
                runner.git(&args)
            }
        };
        check(how, ran, how == "plain", "this runner keeps no read");
        assert_eq!(pair.holds(), how == "plain", "{how}");
    }
}
