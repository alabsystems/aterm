// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A plain `exec` that puts SIGPIPE's disposition back when it fails.

use std::io;
use std::os::unix::process::CommandExt as _;

/// Replace this process with `command`'s program, restoring `SIGPIPE` on failure.
pub fn exec_keeping_sigpipe(command: &mut aterm_uds::spawnfd::StdCommand) -> io::Error {
    let saved = sigpipe_disposition();
    let error = command.exec();
    if let Some(saved) = saved.as_ref() {
        restore_sigpipe(saved);
    }
    error
}

/// SIGPIPE's disposition, read without changing it, so a failed exec can put it back.
pub(crate) fn sigpipe_disposition() -> Option<libc::sigaction> {
    // SAFETY: `sigaction` is plain data for which all-zero is a valid value.
    let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null `act` only reads the current disposition into `old`.
    (unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &mut old) } == 0).then_some(old)
}

/// Put SIGPIPE back to `saved` (from [`sigpipe_disposition`]).
pub(crate) fn restore_sigpipe(saved: &libc::sigaction) {
    // SAFETY: `saved` is a disposition this process read; a null `oldact` is allowed.
    unsafe { libc::sigaction(libc::SIGPIPE, saved, std::ptr::null_mut()) };
}

#[cfg(test)]
mod tests {
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
}
