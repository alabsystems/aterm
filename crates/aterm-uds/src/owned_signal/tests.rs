// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::time::{Duration, Instant};

struct Cleanup(Child);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child() -> Cleanup {
    Cleanup(
        crate::spawnfd::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap(),
    )
}
fn state(pid: u32, stopped: bool) {
    // One-minute hang detector, never a scheduler latency assertion.
    let deadline = Instant::now() + Duration::from_secs(60);
    while crate::process::is_stopped(pid) != stopped {
        assert!(
            Instant::now() < deadline,
            "owned child state did not change"
        );
        crate::deadline::poll_yield();
    }
}
#[test]
fn actual_owned_child_atomic_stop_continue_and_kill() {
    let mut child = child();
    let pid = child.0.id();
    let mut signal = OwnedChildSignal::capture(&mut child.0).unwrap();
    signal.signal(Action::Stop).unwrap();
    state(pid, true);
    signal.signal(Action::Continue).unwrap();
    state(pid, false);
    signal.signal(Action::Kill).unwrap();
    assert!(!child.0.wait().unwrap().success());
    assert!(signal.signal(Action::Continue).is_err());
    assert!(OwnedChildSignal::capture(&mut child.0).is_err());
}
#[test]
fn kernel_rejects_wrong_version_of_the_same_actual_owned_child() {
    let mut child = child();
    let mut signal = OwnedChildSignal::capture(&mut child.0).unwrap();
    // Bypass only the private test token, NOT the original lineage validator.
    // Actual syscall must reject this incorrect image version of a live child.
    signal.token.0[7] = signal.token.0[7].wrapping_add(1);
    assert!(signal.signal(Action::Kill).is_err());
    assert!(child.0.try_wait().unwrap().is_none());
    signal.token.0[7] = signal.original.identity.idversion.unwrap() as u32;
    signal.signal(Action::Stop).unwrap();
    state(child.0.id(), true);
    signal.signal(Action::Continue).unwrap();
    state(child.0.id(), false);
}
#[test]
fn missing_symbol_is_unsupported_and_never_signals_child() {
    let mut child = child();
    // Lookup absence follows exactly the production unsupported branch.
    // SAFETY: Static terminated missing symbol in the existing image set.
    let missing = unsafe {
        libc::dlsym(
            libc::RTLD_DEFAULT,
            c"aterm_nonexistent_atomic_signal".as_ptr(),
        )
    };
    assert!(missing.is_null());
    assert_eq!(
        function(missing).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert!(child.0.try_wait().unwrap().is_none());
    state(child.0.id(), false);
}

#[test]
fn lost_wait_custody_refuses_before_any_new_identity_lookup() {
    let mut child = Cleanup(crate::spawnfd::command("/usr/bin/true").spawn().unwrap());
    // Observe terminal exit without consuming it, then deliberately consume
    // only this actual child's status through the native targeted wait seam.
    // The one-minute bound detects a hang; no scheduler latency is asserted.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !child.0.exited().unwrap() {
        assert!(Instant::now() < deadline, "owned true did not exit");
        crate::deadline::poll_yield();
    }
    let pid = i32::try_from(child.0.id()).unwrap();
    let mut status = 0;
    // SAFETY: pid names this fixture's original exited and unreaped child;
    // WNOHANG consumes only its actual terminal status without signalling.
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        pid
    );
    assert_eq!(
        child.0.try_wait().unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert!(
        !child.0.is_reaped(),
        "an external wait is not our cached reap"
    );
    assert!(!child.0.owns_unreaped(), "actual ECHILD forfeits custody");

    let probes = std::cell::Cell::new(0);
    let result = OwnedChildSignal::capture_with_probe(&mut child.0, resolve().unwrap(), |pid| {
        probes.set(probes.get() + 1);
        crate::process::read_lineage(pid)
    });
    assert!(result.is_err());
    assert_eq!(
        probes.get(),
        0,
        "lost custody cannot authorize a new identity lookup"
    );
}
