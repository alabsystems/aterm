// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn capacity_is_reserved_before_work_and_released_without_displacing_other_work() {
    let sink = NativeUsageSink {
        entries: Arc::new(Mutex::new(Vec::new())),
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
