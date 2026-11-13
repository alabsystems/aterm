// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SPAWNER, Spawners, spawned, start_thread};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, ThreadId};
use std::time::Duration;

/// The hang detector for a wait on what must happen.
const HANG: Duration = Duration::from_secs(60);

/// The thread a job ran on, and its name.
fn here() -> std::io::Result<(ThreadId, Option<String>)> {
    let this = thread::current();
    Ok((this.id(), this.name().map(str::to_owned)))
}

/// A spawn runs on a spawner, never on its caller. Calls made one after another, each from a
/// thread that ended before the next began, run on one spawner, which still runs a job after
/// every caller ended and after a job that panicked.
#[test]
fn a_spawner_outlives_its_callers_and_serves_the_next_call() {
    static ONE_AFTER_ANOTHER: Spawners = Spawners::new(start_thread);
    let ran: Vec<_> = (0..4)
        .map(|_| {
            thread::spawn(|| {
                let ran = ONE_AFTER_ANOTHER.run(here).expect("a spawner runs the job");
                assert_ne!(
                    ran.0,
                    thread::current().id(),
                    "the job ran on a spawner, not on its caller"
                );
                ran
            })
            .join()
            .expect("a caller")
        })
        .collect();
    let panicked = ONE_AFTER_ANOTHER.run::<()>(|| panic!("a job that panics"));
    assert!(panicked.is_err(), "a job that panicked answers an error");
    let later = ONE_AFTER_ANOTHER
        .run(here)
        .expect("a spawner runs a job after its callers ended");
    assert_eq!(later.1.as_deref(), Some(SPAWNER), "a spawner's name");
    assert!(
        ran.iter().all(|job| job.0 == later.0),
        "one spawner for calls one after another, still running after every caller ended: \
         {ran:?}, then {later:?}"
    );
    let process = spawned(here).expect("this process's spawners run a job");
    assert_eq!(
        process.1.as_deref(),
        Some(SPAWNER),
        "this process's spawner's name"
    );
}

/// A call waits for no other call's fork: while one job holds the spawner an earlier call
/// left free, another call runs on a spawner of its own and answers.
#[test]
fn a_spawn_waits_for_no_other_spawns_fork() {
    static BUSY: Spawners = Spawners::new(start_thread);
    let earlier = BUSY.run(here).expect("an earlier job ran");
    let (began, beginning) = mpsc::channel();
    let (release, held) = mpsc::channel::<()>();
    let first = thread::spawn(move || {
        BUSY.run(move || {
            let _ = began.send(());
            let _ = held.recv();
            here()
        })
    });
    beginning.recv_timeout(HANG).expect("the first job began");
    let (said, answer) = mpsc::channel();
    thread::spawn(move || {
        let _ = said.send(BUSY.run(here));
    });
    let second = answer
        .recv_timeout(HANG)
        .expect("a spawn waits for no other spawn's fork")
        .expect("the second job ran");
    drop(release);
    let first = first
        .join()
        .expect("the first caller")
        .expect("the first job ran");
    assert_eq!(first.0, earlier.0, "the first took the spawner left free");
    assert_ne!(first.0, second.0, "the second ran on a spawner of its own");
    assert_eq!(second.1.as_deref(), Some(SPAWNER), "a spawner's name");
}

/// When a spawner's thread cannot start, the call answers that error, runs nothing and lists
/// no spawner, and the next call starts one.
#[test]
fn a_spawner_that_cannot_start_runs_nothing_and_the_next_call_starts_one() {
    static REFUSED: AtomicBool = AtomicBool::new(false);
    fn refuse_once(
        builder: thread::Builder,
        body: Box<dyn FnOnce() + Send>,
    ) -> std::io::Result<()> {
        if REFUSED.swap(true, Ordering::SeqCst) {
            start_thread(builder, body)
        } else {
            Err(std::io::Error::other("no thread"))
        }
    }
    static REFUSING: Spawners = Spawners::new(refuse_once);
    let ran = Arc::new(AtomicBool::new(false));
    let job_ran = Arc::clone(&ran);
    let refused = REFUSING
        .run(move || {
            job_ran.store(true, Ordering::SeqCst);
            here()
        })
        .expect_err("a spawn whose spawner cannot start answers an error");
    assert_eq!(refused.to_string(), "no thread", "the start's own error");
    assert!(!ran.load(Ordering::SeqCst), "its job did not run");
    assert!(REFUSING.take_free().is_none(), "no spawner is listed free");
    let next = REFUSING.run(here).expect("the next call starts a spawner");
    assert_eq!(next.1.as_deref(), Some(SPAWNER), "a spawner's name");
}
