// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn stop_arriving_after_initial_sample_still_wins_the_consumed_wake() {
    let control = Arc::<Control>::default();
    let stopped = Arc::new(AtomicBool::new(false));
    control.wake();
    let mut watch = Watch::private(Arc::clone(&control), &stopped, || {
        panic!("an already queued wake must not block");
    });
    let stop = Arc::clone(&stopped);
    watch.before_hint = Some(Box::new(move || {
        // Actual end ordering, at the formerly exposed sampling window.
        stop.store(true, Ordering::Release);
        control.wake();
    }));
    assert_eq!(
        watch.wait(
            &stopped,
            Instant::now() + std::time::Duration::from_secs(60)
        ),
        Woke::Stopped,
    );
}
