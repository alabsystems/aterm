// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE PLAIN `exec` THAT LEAVES THE PROCESS AS IT WAS WHEN IT FAILS.
//!
//! `Command::exec` resets SIGPIPE to its default disposition before it calls
//! `execve` and leaves it there when the exec fails (measured 2026-09-28 on trustc
//! 1.99-dev and on stock 1.97.1: of 29 dispositions compared, exactly one changes,
//! SIGPIPE from ignored to default). libstd's runtime IGNORES SIGPIPE at start-up, so
//! a process that survives a failed exec — the update lane's rollback arm, the GUI's
//! cold-lane replacement, which returns "process replacement failed" and carries on as
//! the window — would die on its next write to a peer that has gone (an `aterm ctl`
//! client that disconnected mid-reply, the sentinel's pipe), taking every tab with it.
//!
//! [`exec_keeping_sigpipe`] is the one exec the update lane uses: the same `execve`, and
//! on failure SIGPIPE put back to what the caller had.
//!
//! An `execve` keeps the pid but not LaunchServices' record of the app: measured
//! 2026-10-19 on macOS 27, an exec 240–400 ms after a LaunchServices launch leaves
//! `NSRunningApplication` answering pid -1 for the rest of the process's life (no Quit &
//! Reopen, no menu-bar item, TCC attributing it to the rollback path). So a process
//! LaunchServices launched as a seamless successor never reaches this exec: its parent
//! swaps the stage in first (`install::swap_staged_for_launch`). A cold launch that
//! LaunchServices started with a stage ready still execs here; relaunching it through
//! LaunchServices instead is a separate lane, not built yet.

use std::io;
use std::os::unix::process::CommandExt as _;

/// Replace this process with `command`'s program. Returns only on failure, with SIGPIPE
/// as it was on entry.
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

    /// The defect this module exists for, and the fix, in a helper process (the test
    /// process's own SIGPIPE is not ours to change): libstd's start-up ignores SIGPIPE;
    /// a raw `Command::exec` of a missing program returns ENOENT and leaves SIGPIPE at
    /// its default; `exec_keeping_sigpipe` returns the same error and leaves it ignored.
    #[test]
    fn a_failed_plain_exec_leaves_sigpipe_ignored_where_command_exec_does_not() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "exec_plain::tests::zz_helper_entry",
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

    /// The helper. Ordinary runs return at once.
    #[test]
    fn zz_helper_entry() {
        if std::env::var_os(HELPER_ENV).is_none() {
            return;
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
        // put it back by hand so the second measurement starts from the same state
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
    }
}
