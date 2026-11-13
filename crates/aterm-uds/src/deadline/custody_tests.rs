// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::spawnfd::{Stdio, command};
use std::process::ChildStdin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const HANG: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct Retained(Arc<AtomicUsize>);

impl Drop for Retained {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn reading_cat() -> (Child, ChildStdin) {
    // Execute the installed system binary in place. Nothing is copied, signed or removed.
    let mut child = command("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the in-place cat");
    let input = child.stdin.take().expect("cat's input writer");
    (child, input)
}

fn completed_cat() -> (Child, ExitStatus) {
    let (mut child, input) = reading_cat();
    let mut exit = ExitWait::new(&child);
    drop(input);
    let by = Instant::now() + HANG;
    loop {
        if let Some(status) = child.try_wait().expect("wait for cat's input EOF") {
            assert!(status.success());
            return (child, status);
        }
        let left = by.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "cat did not exit after input EOF");
        exit.wait(left);
    }
}

#[test]
fn no_child_allows_retained_resources_to_return_without_a_reap() {
    assert!(command("/dev/null/no-executable").spawn().is_err());
    let drops = Arc::new(AtomicUsize::new(0));
    let mut custody = ChildCustody::not_started(Retained(Arc::clone(&drops)));
    assert_eq!(custody.completion(), ChildCompletion::NotStarted);
    assert_eq!(custody.observe_until(Instant::now()).unwrap(), None);
    let retained = custody.try_into_retained().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(retained);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn no_exit_and_wait_error_retain_resources_until_a_positive_owning_poll() {
    let (child, status) = completed_cat();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut custody = ChildCustody::from_child(child, Retained(Arc::clone(&drops)));
    assert_eq!(custody.completion(), ChildCompletion::Pending);

    assert_eq!(custody.poll_with(|_| Ok(None)).unwrap(), None);
    custody = *custody.try_into_retained().expect_err("no positive poll");
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    let error = custody
        .poll_with(|_| Err(io::Error::other("injected owning-wait failure")))
        .unwrap_err();
    assert_eq!(error.to_string(), "injected owning-wait failure");
    assert_eq!(custody.completion(), ChildCompletion::Pending);
    custody = *custody
        .try_into_retained()
        .expect_err("wait error is not a reap");
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    assert_eq!(custody.poll_with(Child::try_wait).unwrap(), Some(status));
    assert_eq!(custody.completion(), ChildCompletion::Reaped(status));
    assert_eq!(drops.load(Ordering::SeqCst), 0, "observation keeps T owned");
    assert_eq!(
        custody
            .poll_with(|_| panic!("cached status must not poll again"))
            .unwrap(),
        Some(status)
    );
    let retained = custody.try_into_retained().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(retained);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn an_actual_pending_child_keeps_resources_through_observation_and_reap() {
    let (child, input) = reading_cat();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut custody = ChildCustody::from_child(child, Retained(Arc::clone(&drops)));
    assert_eq!(custody.observe_until(Instant::now()).unwrap(), None);
    assert_eq!(custody.completion(), ChildCompletion::Pending);
    custody = *custody
        .try_into_retained()
        .expect_err("cat still reads its input");
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    drop(input);
    let status = custody
        .observe_until(Instant::now() + HANG)
        .expect("observe cat's owning wait")
        .expect("cat exits after input EOF");
    assert!(status.success());
    assert_eq!(custody.completion(), ChildCompletion::Reaped(status));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(custody);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn background_callback_owns_the_child_and_resources_until_positive_reap() {
    let (child, input) = reading_cat();
    let mut exit = ExitWait::new(&child);
    let drops = Arc::new(AtomicUsize::new(0));
    let mut custody = ChildCustody::from_child(child, Retained(Arc::clone(&drops)));
    let mut reap = custody.take_reaper().expect("pending background custody");
    drop(custody);
    assert!(!reap(), "cat still reads its input");
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    drop(input);
    let by = Instant::now() + HANG;
    while !reap() {
        let left = by.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "cat did not exit after input EOF");
        exit.wait(left);
    }
    assert!(reap(), "the actual child's reap is cached");
    assert_eq!(drops.load(Ordering::SeqCst), 0, "the callback still owns T");
    drop(reap);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
