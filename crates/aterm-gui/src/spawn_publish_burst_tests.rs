// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use super::publish_burst;
use crate::cast::ByteFanout;

fn queued_at_wake(publish: impl FnOnce(&ByteFanout, &Arc<[u8]>, &dyn Fn())) -> usize {
    let fanout = Arc::new(ByteFanout::new());
    let sub = fanout.subscribe();
    let seen = std::cell::Cell::new(None);
    let burst: Arc<[u8]> = Arc::from(&b"out"[..]);
    publish(&fanout, &burst, &|| seen.set(Some(sub.drain().0.len())));
    seen.get().expect("the wake fired")
}

#[test]
fn a_woken_bytes_subscriber_finds_the_burst() {
    assert_eq!(
        queued_at_wake(|fanout, burst, wake| publish_burst(fanout, burst, 0, wake)),
        1
    );
    let wake_then_tee = |fanout: &ByteFanout, burst: &Arc<[u8]>, wake: &dyn Fn()| {
        wake();
        fanout.tee(burst, 0);
    };
    assert_eq!(queued_at_wake(wake_then_tee), 0, "the negative control");
}

#[test]
fn a_burst_carries_its_arrival_on_the_frame_clock() {
    let fanout = Arc::new(ByteFanout::new());
    let sub = fanout.subscribe();
    let _ = crate::metrics::now_us();
    let at = std::time::Instant::now();
    let burst: Arc<[u8]> = Arc::from(&b"out"[..]);
    publish_burst(&fanout, &burst, crate::metrics::us_at(at), || {});
    let (bursts, _) = sub.drain();
    let back = crate::metrics::instant_at_us(bursts[0].at_us).expect("in range");
    assert!(
        back <= at && at - back < std::time::Duration::from_micros(1),
        "the stamp is the arrival, floored to the microsecond"
    );
}
