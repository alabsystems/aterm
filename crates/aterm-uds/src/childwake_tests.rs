// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::spawnfd::{self, ChildEvent, ChildStop, Stdio};
use std::io::Write as _;

struct Owned(spawnfd::Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn disposition() -> libc::sigaction {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: inspect this process's action without changing it.
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) },
        0
    );
    // SAFETY: initialized by the successful query.
    unsafe { action.assume_init() }
}

#[test]
fn kernel_hints_cover_actual_stop_and_exit_without_consuming_or_changing_policy() {
    let before = disposition();
    let wake = ChildWake::new().unwrap();
    let mut child = Owned(
        spawnfd::command("/bin/sh")
            .args(["-c", "kill -STOP $$; read item; exit 7"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut input = child.0.stdin.take().unwrap();
    for stopped in [true, false] {
        // One-minute hang detector, never a latency budget.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            crate::ready::until(wake.as_fd(), crate::ready::Interest::Read, deadline).unwrap();
            wake.drain().unwrap();
            if let Some(observed) = child.0.try_job_event().unwrap() {
                match observed.acknowledge().unwrap().unwrap() {
                    ChildEvent::Stopped(reason) => {
                        assert!(stopped);
                        assert_eq!(reason, ChildStop::Signal(libc::SIGSTOP));
                    }
                    ChildEvent::Exited(usage) => {
                        assert!(!stopped);
                        assert_eq!(usage.status.code(), Some(7));
                    }
                }
                break;
            }
        }
        if stopped {
            assert!(!child.0.is_reaped());
            assert!(child.0.try_job_event().unwrap().is_none());
            child.0.resume().unwrap();
            input.write_all(b"item\n").unwrap();
        }
    }
    assert!(child.0.is_reaped());
    assert_eq!(child.0.wait().unwrap().code(), Some(7));
    let after = disposition();
    assert_eq!(before.sa_sigaction, after.sa_sigaction);
    assert_eq!(before.sa_flags, after.sa_flags);
    assert_eq!(before.sa_mask, after.sa_mask);
}
