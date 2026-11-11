// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::os::unix::process::ExitStatusExt as _;

use super::{TERMINATING, action, at_defaults};
use crate::wait::{MUST_HAPPEN, until_within};

/// Set, this process is the one [`a_child_spawned_at_the_defaults_ends_by_a_signal_this_process_ignores`]
/// started to run its body alone.
const ALONE: &str = "ATERM_SPEC_START_ALONE";

/// A child, SIGKILLed when this drops still running (a test that failed before it ended).
struct Killed(aterm_uds::spawnfd::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// `sig`'s handler now.
fn handler(sig: libc::c_int) -> libc::sighandler_t {
    let mut now = action(libc::SIG_DFL);
    // SAFETY: a null new action only reads the current one into `now`, which outlives it.
    assert_eq!(
        unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut now) },
        0
    );
    now.sa_sigaction
}

/// A CHILD SPAWNED AT THE DEFAULTS ENDS BY A SIGNAL THIS PROCESS IGNORES, and the process
/// ignores it again once the guard drops. It runs alone in a process of its own, which first
/// ignores the four [`TERMINATING`] signals as a `nohup` `&` runner would, so no other test
/// of this binary sees them move. NEGATIVE CONTROL (by hand: the sleep spawned before the
/// guard): the sleep ignores its SIGTERM and the end's hang detector fails.
#[test]
fn a_child_spawned_at_the_defaults_ends_by_a_signal_this_process_ignores() {
    if std::env::var_os(ALONE).is_none() {
        let mut alone =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
        alone
            .args([
                "--exact",
                "start::tests::a_child_spawned_at_the_defaults_ends_by_a_signal_this_process_ignores",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(ALONE, "1");
        // Twice the hang detector: the body's own detector says first what never came.
        let out = aterm_uds::deadline::output_within(&mut alone, MUST_HAPPEN * 2)
            .expect("hang detector: the body's own run never ended");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && said.contains("test result: ok. 1 passed"),
            "{}\n{said}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    for sig in TERMINATING {
        // SAFETY: SIG_IGN is a disposition, no handler of ours; this process runs this test
        // alone.
        unsafe { libc::signal(sig, libc::SIG_IGN) };
    }
    let start = at_defaults(&TERMINATING);
    let mut sleep = Killed(
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg("600")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("a sleep"),
    );
    drop(start);
    for sig in TERMINATING {
        assert_eq!(handler(sig), libc::SIG_IGN, "signal {sig} ignored again");
    }
    let pid = libc::pid_t::try_from(sleep.0.id()).expect("a pid");
    // SAFETY: a signal to this test's own unreaped child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let status = until_within(MUST_HAPPEN, "the sleep's end by its SIGTERM", || {
        sleep.0.try_wait().ok().flatten()
    });
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status}");
}
