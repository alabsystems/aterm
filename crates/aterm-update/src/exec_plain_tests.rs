// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::process::Command;
use std::process::Stdio;

const HELPER_ENV: &str = "ATERM_EXEC_PLAIN_TEST_HELPER";

fn sigpipe_is_ignored() -> bool {
    sigpipe_disposition().is_some_and(|action| action.sa_sigaction == libc::SIG_IGN)
}

#[test]
fn a_failed_plain_exec_leaves_sigpipe_ignored_where_command_exec_does_not() {
    if zz_helper_entry_if_asked() {
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "exec_plain::tests::a_failed_plain_exec_leaves_sigpipe_ignored_where_command_exec_does_not",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, "1")
        .stderr(Stdio::null())
        .output()
        .expect("the helper runs");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "{text}");
    for want in [
        "PRECONDITION sigpipe_ignored=true",
        "raw Command::exec errno=Some(2) sigpipe_ignored=false",
        "exec_keeping_sigpipe errno=Some(2) sigpipe_ignored=true",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

fn zz_helper_entry_if_asked() -> bool {
    if std::env::var_os(HELPER_ENV).is_none() {
        return false;
    }
    println!();
    println!("PRECONDITION sigpipe_ignored={}", sigpipe_is_ignored());
    let mut raw = Command::new("/nonexistent/exec-plain-probe");
    let error = raw.exec();
    println!(
        "raw Command::exec errno={:?} sigpipe_ignored={}",
        error.raw_os_error(),
        sigpipe_is_ignored()
    );
    let ignore = libc::sigaction {
        sa_sigaction: libc::SIG_IGN,
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: a valid disposition for SIGPIPE; a null `oldact` is allowed.
    unsafe { libc::sigaction(libc::SIGPIPE, &ignore, std::ptr::null_mut()) };
    let mut kept = Command::new("/nonexistent/exec-plain-probe");
    let error = exec_keeping_sigpipe(&mut kept);
    println!(
        "exec_keeping_sigpipe errno={:?} sigpipe_ignored={}",
        error.raw_os_error(),
        sigpipe_is_ignored()
    );
    true
}
