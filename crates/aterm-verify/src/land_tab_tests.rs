// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify land --tab`'s ask ([`super::tab`]) over a stand-in environment and `aterm ctl`. The
//! words it starts the worker with, its folder and the lines it prints are driven against a real
//! instance by `crates/aterm/tests/worker_spawn_live_headless.rs`.

use super::tab::{Start, start};
use std::path::Path;

const SID: &str = "s-0123456789abcdef0123";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// Ask with `env` as the environment and `answer` as the client's reply; the words it was asked
/// with come back too.
fn ask(
    env: &dyn Fn(&str) -> Option<String>,
    answer: std::io::Result<(&str, &str)>,
) -> (Start, Option<Vec<String>>) {
    let mut asked = None;
    let mut answer = Some(answer);
    let got = start(
        Path::new("/w/repo"),
        &["--admit", "now"].map(String::from),
        HEAD,
        env,
        &mut |args, task| {
            asked = Some(args.to_vec());
            assert!(
                String::from_utf8_lossy(task).starts_with("targo --unverified verify land "),
                "{task:?}"
            );
            answer
                .take()
                .expect("one ask")
                .map(|(out, err)| (out.as_bytes().to_vec(), err.as_bytes().to_vec()))
        },
    );
    (got, asked)
}

/// The environment of a shell in the session `SID`: `ATERM_PARENT_SESSION_ID`, the variable
/// `aterm ctl @self` reads.
fn in_session(key: &str) -> Option<String> {
    (key == "ATERM_PARENT_SESSION_ID").then(|| SID.to_owned())
}

/// Nothing is asked from a shell in no aterm session (`ATERM_SESSION_ID` alone names none to
/// `@self`), and a client that is not there asked nothing: either lands here. So does an aterm's
/// `ERR unsupervised:`, which spawns nothing.
#[test]
fn with_no_harness_to_run_it_the_landing_runs_here() {
    let empty = |key: &str| (key == "ATERM_PARENT_SESSION_ID").then(String::new);
    let root_only = |key: &str| (key == "ATERM_SESSION_ID").then(|| SID.to_owned());
    for env in [
        &(|_: &str| None) as &dyn Fn(&str) -> Option<String>,
        &empty,
        &root_only,
    ] {
        let (got, asked) = ask(env, Ok(("", "")));
        assert!(matches!(got, Start::Here(_)), "{got:?}");
        assert_eq!(asked, None, "nothing is asked");
    }
    let (got, _) = ask(
        &in_session,
        Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
    );
    assert!(matches!(got, Start::Here(_)), "{got:?}");
    let (got, asked) = ask(
        &in_session,
        Ok((
            "",
            "aterm-ctl: ERR unsupervised: [harness] enabled = false\n",
        )),
    );
    assert_eq!(
        got,
        Start::Here("aterm answered `ERR unsupervised: [harness] enabled = false`".to_owned())
    );
    assert!(asked.is_some(), "a shell in a session asks");
}

/// NEGATIVE CONTROL for the run in place: any other answer, an answer naming no sid, or a client
/// that did not answer may hide a worker that started, so nothing lands here beside it. An `OK`
/// naming a tab, in the `t` form or the legacy `s-` one, is the worker's, on stdout or on stderr
/// behind the client's prefix, where aterm 0.201.0 says it.
#[test]
fn an_unknown_outcome_lands_nowhere() {
    for answer in [
        Ok((
            "",
            "aterm-ctl: ERR spawn program=targo: shell integration is off\n",
        )),
        Ok(("OK\n", "")),
        Ok(("", "")),
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
    ] {
        let (got, _) = ask(&in_session, answer);
        assert!(matches!(got, Start::Unknown(_)), "{got:?}");
    }
    for sid in [SID, "t0123456789"] {
        for answer in [
            (format!("OK {sid} req=r-1\n"), String::new()),
            (String::new(), format!("aterm-ctl: OK {sid} req=r-1\n")),
        ] {
            let (got, _) = ask(&in_session, Ok((&answer.0, &answer.1)));
            assert_eq!(got, Start::Worker(sid.to_owned()), "{answer:?}");
        }
    }
}
