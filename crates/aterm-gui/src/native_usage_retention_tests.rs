// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn capacity_is_reserved_before_work_and_released_without_displacing_other_work() {
    let sink = NativeUsageSink {
        entries: Arc::new(Mutex::new(Vec::new())),
        driver: Driver::default(),
    };
    let mut reservations = (0..CAPACITY)
        .map(|_| sink.reserve().expect("bounded admission"))
        .collect::<Vec<_>>();
    assert!(sink.reserve().is_none());
    let original = reservations.pop().unwrap();
    let released = original.index;
    drop(original);
    let replacement = sink.reserve().expect("unused admission returned");
    assert_eq!(replacement.index, released);
    assert!(sink.reserve().is_none());
    drop(replacement);
    drop(reservations);
    assert!(sink.entries.lock().unwrap().iter().all(Option::is_none));
}

#[test]
fn full_or_cancelled_admission_preserves_the_cold_read_allowance() {
    let sink = NativeUsageSink {
        entries: Arc::new(Mutex::new(Vec::new())),
        driver: Driver::default(),
    };
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let reservations = (0..CAPACITY)
        .map(|_| sink.reserve().unwrap())
        .collect::<Vec<_>>();
    assert!(sink.reserve_read(&scope).is_none());
    drop(reservations);
    drop(sink.reserve_read(&scope).unwrap());
    let pause = lifetime.pause().unwrap();
    assert!(sink.reserve_read(&scope).is_none());
    drop(scope);
    assert!(pause.try_wait().is_ok());
}

/// The recovery driver holding nothing a retry may settle rests until its next discovery, not a
/// second: a reservation in flight ends with its read, and one its read commits a retained
/// response to (`Reservation::retain`) wakes the driver as it ends (its handle on the sink goes,
/// and a handle's drop wakes the driver), which runs a round at once,
/// so the response is retried on its arrival where the one-second pace retried it within a
/// second. The App's last handle going ends the driver at once, where that pace ended it within a
/// second. Discovery runs at the driver's start and every [`DISCOVER_EVERY`] after (here a day,
/// so only a wake can run a second round). Each step waits on the one-minute hang detector, never
/// a latency budget.
///
/// NEGATIVE CONTROLS: with `drive` parking [`PACE`] whatever it holds (the driver before), its
/// first rest is a second (`rests until its next discovery`) (2026-11-14); with a handle's drop
/// waking nothing (`Driver`'s `Drop`), the committed reservation's end runs no round and meets
/// the hang detector (`a committed reservation wakes the driver`) (2026-11-15).
#[test]
fn the_driver_rests_until_discovery_and_wakes_on_demand() {
    use std::sync::mpsc;
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    let app = NativeUsageSink {
        entries: Arc::new(Mutex::new(Vec::new())),
        driver: Driver::default(),
    };
    let reservation = app.reserve().expect("a read in flight");
    let worker = app.clone();
    let (rested, rests) = mpsc::channel::<Duration>();
    let (ended, end) = mpsc::channel::<usize>();
    let driver = std::thread::spawn(move || {
        let mut discovered = 0;
        worker.drive(
            &mut |_| discovered += 1,
            &mut |rest| {
                let _ = rested.send(rest);
                std::thread::park_timeout(rest);
            },
            DAY,
        );
        let _ = ended.send(discovered);
    });
    let _ = app.driver.0.set(driver.thread().clone());
    let first = rests
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the driver rests");
    assert!(
        first > DAY - aterm_spec::wait::MUST_HAPPEN,
        "rests until its next discovery: {first:?}"
    );
    // What `Reservation::retain` does but the response: a real retained response is a provider
    // read's, so the entry stays a reservation, which no retry settles.
    let mut committed = app.reserve().expect("a read that retains its response");
    let index = committed.index;
    committed.committed = true;
    drop(committed);
    let next = rests
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("a committed reservation wakes the driver");
    assert!(
        next > DAY - aterm_spec::wait::MUST_HAPPEN,
        "holding nothing a retry settles, it rests until its next discovery again: {next:?}"
    );
    app.entries.lock().unwrap()[index] = None;
    drop(reservation);
    drop(app);
    let discovered = end
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the App's last handle ends it");
    assert_eq!(discovered, 1, "discovery at the start only");
    driver.join().expect("the driver");
}
