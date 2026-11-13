// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{LAUNCH_SETTING_RETRY, LaunchWait, OwedLaunchStarts, RunOnce, once_known, start_owed};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// The one-minute hang detector for a wait that must end.
const HANG: Duration = Duration::from_secs(60);

fn thread(job: RunOnce) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("launch-wait-test".into())
        .spawn(job)
        .map(drop)
}

fn stand_in(
    read_again: impl Fn() -> Option<bool> + Send + Sync + 'static,
    sleep: impl Fn(Duration) + Send + Sync + 'static,
    spawn: impl Fn(RunOnce) -> std::io::Result<()> + Send + Sync + 'static,
) -> LaunchWait {
    LaunchWait {
        read_again: Arc::new(read_again),
        sleep: Arc::new(sleep),
        spawn: Arc::new(spawn),
    }
}

/// A wait whose every part fails the test: nothing may read, sleep or start a thread.
fn untouched(why: &'static str) -> LaunchWait {
    stand_in(
        move || panic!("{why}: nothing reads"),
        move |_| panic!("{why}: nothing sleeps"),
        move |_| panic!("{why}: no thread starts"),
    )
}

/// A START WAITS FOR THE LAUNCH SETTING OFF THE CALLER'S THREAD, THEN RUNS ONCE AND ITS THREAD
/// ENDS: the caller returns before the setting answers, each re-read with no answer costs one
/// retry, the start runs on the waiting thread at the read that answers, whatever it says (the
/// start itself reads it), and the thread then ends. CONTROL: a setting known at once starts at
/// once, on the caller's thread, with no read, retry or thread.
#[test]
fn a_start_waits_off_the_caller_for_the_launch_setting_then_runs_once() {
    let caller = std::thread::current().id();
    let (started, starts) = mpsc::channel();
    let owed = once_known(
        "checks",
        Some(true),
        &untouched("CONTROL: a known setting"),
        Box::new(move || started.send(std::thread::current().id()).unwrap()),
    );
    assert!(owed.is_none());
    assert_eq!(
        starts.try_recv(),
        Ok(caller),
        "CONTROL: an answer starts at once"
    );

    let answers = Arc::new(Mutex::new(vec![Some(false), None, None]));
    let (pass, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let sleeps = Arc::new(Mutex::new(0_u32));
    let slept = Arc::clone(&sleeps);
    let asked = Arc::clone(&answers);
    let (ended, ends) = mpsc::channel::<()>();
    let wait = stand_in(
        move || asked.lock().unwrap().pop().unwrap_or(Some(true)),
        move |retry| {
            assert_eq!(retry, LAUNCH_SETTING_RETRY);
            gate.lock()
                .unwrap()
                .recv_timeout(HANG)
                .expect("the test lets each retry pass");
            *slept.lock().unwrap() += 1;
        },
        move |job| {
            let ended = ended.clone();
            thread(Box::new(move || {
                job();
                ended.send(()).unwrap();
            }))
        },
    );
    let (started, starts) = mpsc::channel();
    let owed = once_known(
        "checks",
        None,
        &wait,
        Box::new(move || started.send(std::thread::current().id()).unwrap()),
    );
    assert!(owed.is_none(), "a waiting thread owes nothing");
    assert_eq!(
        starts.try_recv(),
        Err(mpsc::TryRecvError::Empty),
        "the caller returned before the setting answered"
    );
    for _ in 0..3 {
        pass.send(()).unwrap();
    }
    let ran_on = starts
        .recv_timeout(HANG)
        .expect("the start runs once the setting answers");
    assert_ne!(ran_on, caller, "the start ran on the waiting thread");
    assert_eq!(
        *sleeps.lock().unwrap(),
        3,
        "one retry for each read with no answer"
    );
    assert!(
        answers.lock().unwrap().is_empty(),
        "it read until the answer and no further"
    );
    assert_eq!(
        starts.recv_timeout(HANG),
        Err(mpsc::RecvTimeoutError::Disconnected),
        "the start ran once"
    );
    drop((pass, wait));
    assert_eq!(ends.recv_timeout(HANG), Ok(()), "and its thread ended");
}

/// A START WITH NO THREAD TO WAIT ON IS OWED, AND ITS DRAIN READS NOTHING: a failed spawn gives
/// the start back unrun; the drain tries no thread before its retry time, tries one at it (a
/// failure keeps the start owed for the next), runs the start on the caller's thread once the
/// setting has an answer kept, and hands it to a thread that starts. CONTROL: with nothing
/// owed, the drain reads, sleeps and spawns nothing.
#[test]
fn a_start_with_no_thread_to_wait_on_is_owed_and_its_drain_reads_nothing() {
    let tries = Arc::new(Mutex::new(0_u32));
    let tried = Arc::clone(&tries);
    let no_threads = stand_in(
        || panic!("no thread ran to read"),
        |_| panic!("no thread ran to sleep"),
        move |_| {
            *tried.lock().unwrap() += 1;
            Err(std::io::Error::other("no threads"))
        },
    );
    let (started, starts) = mpsc::channel();
    let given_back = once_known(
        "checks",
        None,
        &no_threads,
        Box::new(move || started.send(std::thread::current().id()).unwrap()),
    );
    let (lane, start) = given_back.expect("a failed spawn gives the start back");
    assert_eq!(lane, "checks");
    assert_eq!(
        starts.try_recv(),
        Err(mpsc::TryRecvError::Empty),
        "it has not run"
    );
    assert_eq!(*tries.lock().unwrap(), 1);

    let at = Instant::now();
    let owed = Mutex::new(OwedLaunchStarts {
        starts: vec![(lane, start)],
        retry_at: Some(at + LAUNCH_SETTING_RETRY),
    });
    start_owed(&owed, None, at, &no_threads);
    assert_eq!(
        *tries.lock().unwrap(),
        1,
        "no thread is tried before its time"
    );
    start_owed(&owed, None, at + LAUNCH_SETTING_RETRY, &no_threads);
    assert_eq!(*tries.lock().unwrap(), 2, "one is tried at its time");
    assert_eq!(
        owed.lock().unwrap().starts.len(),
        1,
        "a failed try keeps it owed"
    );
    assert_eq!(
        owed.lock().unwrap().retry_at,
        Some(at + LAUNCH_SETTING_RETRY * 2),
        "for the next try"
    );
    assert_eq!(starts.try_recv(), Err(mpsc::TryRecvError::Empty));

    let caller = std::thread::current().id();
    start_owed(
        &owed,
        Some(false),
        at + LAUNCH_SETTING_RETRY,
        &untouched("a kept answer"),
    );
    assert!(
        owed.lock().unwrap().starts.is_empty(),
        "a kept answer pays it"
    );
    assert_eq!(starts.try_recv(), Ok(caller), "and runs it here");
    assert_eq!(
        starts.try_recv(),
        Err(mpsc::TryRecvError::Disconnected),
        "once"
    );

    let (started, starts) = mpsc::channel();
    owed.lock().unwrap().starts.push((
        "checks",
        Box::new(move || started.send(std::thread::current().id()).unwrap()),
    ));
    start_owed(
        &owed,
        None,
        at + LAUNCH_SETTING_RETRY * 2,
        &stand_in(|| Some(true), |_| {}, thread),
    );
    assert!(owed.lock().unwrap().starts.is_empty(), "a thread took it");
    let ran_on = starts
        .recv_timeout(HANG)
        .expect("the thread runs it once the setting answers");
    assert_ne!(ran_on, caller);

    start_owed(
        &owed,
        None,
        at + LAUNCH_SETTING_RETRY * 9,
        &untouched("CONTROL: nothing owed"),
    );
}
