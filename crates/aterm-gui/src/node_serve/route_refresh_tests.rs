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
