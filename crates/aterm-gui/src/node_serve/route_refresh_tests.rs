// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[test]
fn a_busy_discovery_worker_does_not_block_the_watcher_or_start_another_worker() {
    let stopped = Arc::new(Stop::new());
    let mut refresh = RouteRefresh::new(Arc::clone(&stopped));
    let now = Instant::now();
    let (entered, heard) = mpsc::channel();
    let (release, released) = mpsc::channel();
    assert!(refresh.start(now, move || {
        entered.send(()).unwrap();
        released
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
    }));
    heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    assert!(
        !refresh.start(now + aterm_agent::node_service::DISCOVERY_PERIOD, {
            let ran = Arc::clone(&ran);
            move || {
                ran.store(true, Ordering::Release);
            }
        })
    );
    release.send(()).unwrap();
    drop(refresh);
    assert!(
        stopped.is_set(),
        "the generation stops before its worker is joined"
    );
    assert!(!ran.load(Ordering::Acquire));
}

/// The next refresh is due at once before the first and a discovery period after each start, the
/// deadline a node's watch wakes for.
/// NEGATIVE CONTROL: a start refused while a worker is out leaves the deadline where it was.
#[test]
fn the_next_refresh_is_due_a_period_after_the_last_start() {
    let mut refresh = RouteRefresh::new(Arc::new(Stop::new()));
    assert!(
        refresh.next_due() <= Instant::now(),
        "the first is due at once"
    );
    let now = Instant::now();
    let (release, released) = mpsc::channel::<()>();
    assert!(refresh.start(now, move || {
        let _ = released.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    }));
    let period = aterm_agent::node_service::DISCOVERY_PERIOD;
    assert_eq!(refresh.next_due(), now + period);
    assert!(
        !refresh.start(now + period, || {}),
        "CONTROL: its worker is out"
    );
    assert_eq!(
        refresh.next_due(),
        now + period,
        "a refused start moves nothing"
    );
    release.send(()).unwrap();
}
