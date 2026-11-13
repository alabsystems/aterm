// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn reading(cpu: Option<u64>) -> Stamp {
    Stamp {
        wall: Instant::now(),
        cpu,
        thread: std::thread::current().id(),
    }
}

#[test]
fn disabled_scopes_never_probe() {
    let probes = std::cell::Cell::new(0);
    drop(begin_with(None, Some(7), |_| {
        probes.set(probes.get() + 1);
        reading(Some(1))
    }));
    assert_eq!(probes.get(), 0);
}

#[test]
fn unavailable_or_reset_cpu_is_unknown_without_losing_work() {
    for (from, to) in [(None, Some(5)), (Some(5), None), (Some(5), Some(4))] {
        let counter = Counter::default();
        counter.finish(reading(from), reading(to), Some(7));
        assert_eq!(counter.cpu.value(), None);
        assert_eq!(counter.cpu.unknown.load(Ordering::Relaxed), 1);
        assert_eq!(counter.calls.value(), Some(1));
        assert_eq!(counter.work.value(), Some(7));
    }
    let counter = Counter::default();
    counter.finish(reading(Some(4)), reading(Some(4)), Some(0));
    assert_eq!(
        counter.cpu.value(),
        Some(0),
        "an observed unchanged counter is known"
    );
}

#[test]
fn unread_scope_stays_unknown_after_later_known_cpu_and_overflow() {
    let counter = Counter::default();
    counter.finish(reading(None), reading(None), Some(1));
    counter.finish(reading(Some(1)), reading(Some(4)), Some(1));
    assert_eq!(counter.cpu.value(), None);
    let sum = Sum::default();
    sum.add(Some(u64::MAX));
    sum.add(Some(1));
    assert_eq!(sum.value(), None);
}
