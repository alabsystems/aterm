// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_window::platform::x11::{dispatch_with_key_event_server_time, key_event_server_time};

use super::current_event_queue_age_ns;

fn monotonic_ms() -> u32 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable `timespec`.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) },
        0
    );
    (now.tv_sec as u64 * 1_000 + now.tv_nsec as u64 / 1_000_000) as u32
}

#[test]
fn a_key_stamp_reaches_the_age_for_exactly_one_dispatch() {
    const MS: u64 = 1_000_000;
    assert_eq!(key_event_server_time(), None);
    assert_eq!(current_event_queue_age_ns(), None, "no key, no age");

    let stamp = monotonic_ms().wrapping_sub(40);
    let age = dispatch_with_key_event_server_time(stamp, current_event_queue_age_ns)
        .expect("the stamp must reach the age inside the dispatch");
    assert!(
        (40 * MS..1_000 * MS).contains(&age),
        "age {age} ns for a 40 ms old stamp"
    );
    assert_eq!(
        key_event_server_time(),
        None,
        "withdrawn after the dispatch"
    );
    assert_eq!(current_event_queue_age_ns(), None);

    assert_eq!(
        dispatch_with_key_event_server_time(0, key_event_server_time),
        Some(0)
    );
    assert_eq!(
        dispatch_with_key_event_server_time(0, current_event_queue_age_ns),
        None
    );

    let unwound = std::panic::catch_unwind(|| {
        dispatch_with_key_event_server_time(stamp, || panic!("a key handler panicked"))
    });
    assert!(unwound.is_err());
    assert_eq!(key_event_server_time(), None);
}
