// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::spawnfd::{Stdio, command};
use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;

fn job_event<'a>(child: &'a mut Child, phase: &str) -> ChildObservation<'a> {
    let pid = child.id();
    aterm_spec::wait::until_while_working(
        pid,
        &format!("{phase} job event from owned child {pid}"),
        || child.try_job_event().unwrap().is_some().then_some(()),
    );
    // The predicate consumes no notification, and this test is the child's sole owner.
    child.wait_job_event().unwrap()
}

struct Owned {
    child: Child,
    phase: &'static str,
}
impl std::ops::Deref for Owned {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl std::ops::DerefMut for Owned {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let failed = std::thread::panicking();
        let pid = self.child.id();
        let mut diagnostic = std::io::stderr();
        if failed {
            let observation = self
                .child
                .try_job_event()
                .map(|event| event.map(|event| event.notice()));
            let _ = writeln!(
                diagnostic,
                "owned child {pid} phase={} before cleanup: reaped={} owns_unreaped={} observation={observation:?}",
                self.phase,
                self.child.is_reaped(),
                self.child.owns_unreaped(),
            );
        }
        let killed = self.child.kill();
        if failed {
            let _ = writeln!(
                diagnostic,
                "owned child {pid} phase={} cleanup kill: {killed:?}",
                self.phase
            );
        }
        let waited = self.child.wait();
        if failed {
            let _ = writeln!(
                diagnostic,
                "owned child {pid} phase={} cleanup wait: {waited:?}",
                self.phase
            );
        }
    }
}

#[test]
fn acknowledged_stops_preserve_ownership_until_the_actual_accounted_exit() {
    let mut child = Owned {
        child: command("/bin/sh")
            .args([
                "-c",
                "kill -STOP $$; read first; printf x; kill -STOP $$; read second; printf y; exit 7",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
        phase: "initial stop",
    };
    let pid = child.id();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    for (phase, line, expected) in [
        ("first", b"first\n".as_slice(), b'x'),
        ("second", b"second\n".as_slice(), b'y'),
    ] {
        child.phase = phase;
        let observed = job_event(&mut child, phase);
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
        // SIGCONT being accepted does not prove the shell ran. Observe its progress
        // beyond this read before asking for another stop or the final exit.
        aterm_spec::wait::until_while_working(
            pid,
            &format!("{phase} stdout byte from owned child {pid}"),
            || {
                crate::ready::any_now(&[output.as_fd()])
                    .unwrap()
                    .contains(&0)
                    .then_some(())
            },
        );
        let mut proof = [0];
        output.read_exact(&mut proof).unwrap();
        assert_eq!(proof, [expected], "{phase} byte from owned child {pid}");
    }
    child.phase = "final exit";
    let observed = job_event(&mut child, "final exit");
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
        let mut child = Owned {
            child: command("/bin/sh").args(["-c", "exit 7"]).spawn().unwrap(),
            phase: "dropped exit notice",
        };
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
    let mut child = Owned {
        child: command("/bin/sh")
            .args(["-c", "exit 7"])
            .start_suspended(true)
            .spawn()
            .unwrap(),
        phase: "suspended start",
    };
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
