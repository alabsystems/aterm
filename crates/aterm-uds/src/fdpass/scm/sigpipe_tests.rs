// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{IoSlice, Write as _};
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Command, ExitStatus};

use crate::CtlStream;

const HELPER_ENV: &str = "ATERM_UDS_SEND_VECTORED_HELPER";
/// The same numbers on Darwin and Linux.
const SIGPIPE: i32 = 13;
const SIG_DFL: usize = 0;
const EPIPE: i32 = 32;

unsafe extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

/// How the helper ended in `mode`, and what it printed.
fn helper(mode: &str) -> (ExitStatus, String) {
    let out = crate::deadline::std_ends_within(
        Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "fdpass::scm::sigpipe_tests::zz_helper_entry",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER_ENV, mode),
        crate::deadline::ECHO_HANG,
    );
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// A gather to a `socketpair` end whose peer closed, with SIGPIPE at
/// its default (as `Command::exec` leaves it before `execve`), is
/// `EPIPE` and the sender lives.
#[test]
fn a_gather_to_a_hung_up_peer_is_epipe_not_sigpipe() {
    let (status, text) = helper("send_vectored");
    assert!(status.success(), "{status:?}: {text}");
    assert!(
        text.contains(&format!("send_vectored errno=Some({EPIPE})")),
        "{text}"
    );
    let (status, text) = helper("write_vectored");
    assert_eq!(
        status.signal(),
        Some(SIGPIPE),
        "std's writev raises SIGPIPE: {status:?}: {text}"
    );
}

/// The helper.
#[test]
fn zz_helper_entry() {
    let Some(mode) = std::env::var_os(HELPER_ENV) else {
        return;
    };
    // SAFETY: restores `SIG_DFL` for `SIGPIPE`.
    unsafe { signal(SIGPIPE, SIG_DFL) };
    let (near, far) = CtlStream::pair().expect("a socketpair");
    drop(far);
    let bufs = [IoSlice::new(b"ab"), IoSlice::new(b"cd")];
    let sent = if mode == "write_vectored" {
        (&near).write_vectored(&bufs)
    } else {
        super::send_vectored(&near, &bufs)
    };
    println!();
    println!(
        "{} errno={:?}",
        mode.to_string_lossy(),
        sent.err().and_then(|e| e.raw_os_error())
    );
}
