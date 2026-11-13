// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

/// Set, the test binary is [`standin_front_handles_and_becomes`].
const FRONT: &str = "ATPKG_BECOMING_FRONT";
/// Set, the test binary is [`standin_reports_what_it_was_started_with`].
const REPORT: &str = "ATPKG_BECOMING_REPORT";

/// Whether `sig`'s disposition `INHERITED` read at load is to ignore it.
fn ignored(sig: libc::c_int) -> bool {
    INHERITED.get().is_some_and(|i| {
        i.actions
            .iter()
            .any(|(s, a)| *s == sig && a.sa_sigaction == libc::SIG_IGN)
    })
}

extern "C" fn noted(_: libc::c_int) {}

/// The front, as a stand-in: catches SIGINT with a handler of its own (as an admission wait
/// does), then becomes [`standin_reports_what_it_was_started_with`].
#[test]
fn standin_front_handles_and_becomes() {
    if std::env::var_os(FRONT).is_none() {
        return;
    }
    // SAFETY: `noted` is an `extern "C" fn(c_int)` that touches nothing.
    unsafe {
        libc::signal(
            libc::SIGINT,
            noted as extern "C" fn(libc::c_int) as libc::sighandler_t,
        )
    };
    let exe = std::env::current_exe().expect("test binary");
    let err = become_build(
        &exe,
        &[
            "--exact",
            "build_admit::becoming::tests::standin_reports_what_it_was_started_with",
            "--test-threads=1",
            "--nocapture",
        ],
        None,
    );
    panic!("the exec failed: {err}");
}

/// The build, as a stand-in: says which of SIGPIPE and SIGINT it was started ignoring.
#[test]
fn standin_reports_what_it_was_started_with() {
    if std::env::var_os(REPORT).is_none() {
        return;
    }
    println!(
        "REPORT pipe={} int={}",
        if ignored(libc::SIGPIPE) {
            "ignored"
        } else {
            "default"
        },
        if ignored(libc::SIGINT) {
            "ignored"
        } else {
            "default"
        }
    );
}

/// The report of a front started by `sh` with `trap` set (empty for none), from this process
/// with SIGINT at its default action, set here, not the runner's (a `&` job's is ignored): the
/// shell, and through it the front, inherit it. (SIGPIPE, which this process's runtime
/// ignores, the spawn puts back to its default in the shell.)
fn report(trap: &str) -> String {
    let exe = std::env::current_exe().expect("test binary");
    let start = crate::platform::Starting::at_defaults(&[libc::SIGINT]);
    let mut child = start.spawning(|| {
        aterm_uds::spawnfd::command("/bin/sh")
            .args([
                "-c",
                &format!(
                    "{trap} exec \"$0\" --exact \
                     build_admit::becoming::tests::standin_front_handles_and_becomes \
                     --test-threads=1 --nocapture"
                ),
            ])
            .arg(&exe)
            .env(FRONT, "1")
            .env(REPORT, "1")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the front")
    });
    drop(start);
    let status = until_within(MUST_HAPPEN, "the stand-ins' end", || {
        child.try_wait().ok().flatten()
    });
    let mut out = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut o, &mut out);
    }
    assert!(status.success(), "{status} {out}");
    out.lines()
        .find_map(|l| l.split_once("REPORT ").map(|(_, said)| said))
        .unwrap_or("no report")
        .to_owned()
}

/// A BUILD STARTS WITH THE SIGNAL STATE ITS FRONT WAS STARTED WITH: a front started ignoring
/// SIGPIPE and SIGINT, which caught SIGINT itself, hands its build both ignored; one started
/// at the defaults hands both at the defaults, not the SIGPIPE its runtime ignored. Both start
/// from a state the test sets ([`report`]), whatever the runner was started with.
/// NEGATIVE CONTROL (by hand: `become_build` with `put_back` skipped, std's exec): the first
/// build is started with both at their defaults.
#[test]
fn a_build_starts_with_the_signal_state_its_front_was_started_with() {
    assert_eq!(report("trap '' PIPE INT;"), "pipe=ignored int=ignored");
    assert_eq!(report(""), "pipe=default int=default");
}
