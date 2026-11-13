// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::spawnfd::{Stdio, command};
use std::io::Write as _;

struct Owned(Child);
impl std::ops::Deref for Owned {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for Owned {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn acknowledged_stops_preserve_ownership_until_the_actual_accounted_exit() {
    let mut child = Owned(
        command("/bin/sh")
            .args([
                "-c",
                "kill -STOP $$; read first; kill -STOP $$; read second; exit 7",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = child.id();
    let mut input = child.stdin.take().unwrap();
    for line in [b"first\n".as_slice(), b"second\n".as_slice()] {
        let observed = child.wait_job_event().unwrap();
        assert_eq!(observed.id(), pid);
        assert_eq!(
            observed.notice(),
            ChildNotice::Stopped(ChildStop::Signal(libc::SIGSTOP))
        );
        assert_eq!(
            observed.acknowledge().unwrap(),
            Some(ChildEvent::Stopped(ChildStop::Signal(libc::SIGSTOP)))
        );
        assert!(!child.is_reaped());
        assert!(!child.exited().unwrap());
        assert!(
            child.try_job_event().unwrap().is_none(),
            "a stop is acknowledged only once"
        );
        child.resume().unwrap();
        input.write_all(line).unwrap();
    }
    let observed = child.wait_job_event().unwrap();
    assert_eq!(observed.notice(), ChildNotice::Exited);
    let Some(ChildEvent::Exited(usage)) = observed.acknowledge().unwrap() else {
        panic!("actual exit was not accounted")
    };
    assert_eq!(usage.status.code(), Some(7));
    assert!(child.is_reaped());
    assert_eq!(
        child.wait_with_rusage().unwrap(),
        usage,
        "the same owner retains complete accounting"
    );
    assert!(
        child.try_job_event().is_err(),
        "a reaped pid cannot mint new observations"
    );
}

#[test]
fn dropped_exit_notice_remains_waitable_after_an_owned_kill_request() {
    for kill in [false, true] {
        let mut child = Owned(command("/bin/sh").args(["-c", "exit 7"]).spawn().unwrap());
        {
            let observed = child.wait_job_event().unwrap();
            assert_eq!(observed.notice(), ChildNotice::Exited);
        }
        if kill {
            let _ = child.kill();
        }
        let observed = child
            .try_job_event()
            .unwrap()
            .expect("unreaped exit remains observable");
        assert_eq!(observed.notice(), ChildNotice::Exited);
        let Some(ChildEvent::Exited(usage)) = observed.acknowledge().unwrap() else {
            panic!("actual accounted exit");
        };
        assert_eq!(usage.status.code(), Some(7));
    }
}

#[test]
fn suspended_start_is_not_a_user_signal_or_an_exit() {
    let mut child = Owned(
        command("/bin/sh")
            .args(["-c", "exit 7"])
            .start_suspended(true)
            .spawn()
            .unwrap(),
    );
    let observed = child.wait_job_event().unwrap();
    assert_eq!(
        observed.notice(),
        ChildNotice::Stopped(ChildStop::SuspendedStart)
    );
    assert_eq!(
        observed.acknowledge().unwrap(),
        Some(ChildEvent::Stopped(ChildStop::SuspendedStart))
    );
    assert!(!child.is_reaped());
    assert!(!child.exited().unwrap());
    assert!(child.try_job_event().unwrap().is_none());
    child.resume().unwrap();
    let Some(ChildEvent::Exited(usage)) = child.wait_job_event().unwrap().acknowledge().unwrap()
    else {
        panic!("actual exit required")
    };
    assert_eq!(usage.status.code(), Some(7));
    assert_eq!(child.wait_with_rusage().unwrap(), usage);
}
