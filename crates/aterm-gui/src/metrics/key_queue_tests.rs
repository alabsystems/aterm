// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    KeyWriteStamp, Metrics, PendingInputStamp, emit_percentiles, emit_summary, now_ns, wire,
};
use crate::platform::nsevent_queue_age_ns;

fn quality(metrics: &Metrics) -> [u64; 4] {
    let ctx = wire::ReadCtx::inert(metrics, now_ns());
    let json = wire::render_json(|sink| emit_percentiles(&ctx, sink));
    let value: aterm_json::Value = aterm_json::from_str(&json).unwrap();
    let count = |name| value.get(name).and_then(aterm_json::Value::as_u64).unwrap();
    let text = wire::render_text(|sink| emit_percentiles(&ctx, sink));
    for name in [
        "n_key_queue",
        "key_queue_unavailable",
        "n_key_write",
        "key_write_unavailable",
    ] {
        assert_eq!(
            super::text_field(&text, name)
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            count(name)
        );
    }
    let summary = wire::render_json(|sink| emit_summary(&ctx, None, sink));
    let summary: aterm_json::Value = aterm_json::from_str(&summary).unwrap();
    for name in ["key_queue_unavailable", "key_write_unavailable"] {
        assert_eq!(summary.get(name), value.get(name));
    }
    [
        count("n_key_queue"),
        count("key_queue_unavailable"),
        count("n_key_write"),
        count("key_write_unavailable"),
    ]
}

fn claim(metrics: &Metrics, queue_ns: Option<u64>) -> KeyWriteStamp {
    metrics.with_key_dispatch(true, queue_ns, || {
        assert!(
            metrics.key_arrival_ns() > 0,
            "a press retains its input/FX clock"
        );
        let stamp = metrics.take_key_write_stamp();
        assert_eq!(
            metrics.take_key_write_stamp().at_ns,
            0,
            "one owner claims it"
        );
        stamp
    })
}

#[test]
fn native_clock_validation_keeps_observed_zero_separate_from_unsupported_clocks() {
    assert_eq!(nsevent_queue_age_ns(100.0, 100.0), Some(0));
    assert_eq!(nsevent_queue_age_ns(100.000_5, 100.0), Some(0));
    assert_eq!(nsevent_queue_age_ns(98.0, 100.0), Some(2_000_000_000));
    for (timestamp, uptime) in [
        (0.0, 100.0),
        (-1.0, 100.0),
        (100.002, 100.0),
        (97.999, 100.0),
        (f64::NAN, 100.0),
        (f64::INFINITY, 100.0),
        (100.0, f64::NAN),
        (100.0, f64::INFINITY),
    ] {
        assert_eq!(nsevent_queue_age_ns(timestamp, uptime), None);
    }
}

#[test]
fn native_queue_producer_reaches_the_real_booking_and_wire_without_fabricated_zeroes() {
    for age in [
        nsevent_queue_age_ns(100.0, 100.0),
        nsevent_queue_age_ns(99.75, 100.0),
        None,
        nsevent_queue_age_ns(0.0, 100.0),
        nsevent_queue_age_ns(100.002, 100.0),
        nsevent_queue_age_ns(97.999, 100.0),
        nsevent_queue_age_ns(f64::NAN, 100.0),
    ] {
        let metrics = Metrics::new();
        let stamp = claim(&metrics, age);
        let observed = u64::from(age.is_some());
        let unavailable = u64::from(age.is_none());
        assert_eq!(quality(&metrics), [observed, unavailable, 0, 0]);
        let completed = stamp.at_ns + 1_000;
        assert_eq!(
            metrics.record_key_write_at(stamp, completed).is_some(),
            age.is_some()
        );
        assert_eq!(
            quality(&metrics),
            [observed, unavailable, observed, unavailable]
        );
        assert_eq!(metrics.key_arrival_ns(), 0);
        if let Some(age) = age {
            assert_eq!(metrics.window.input.key_queue.last(), age);
        } else {
            // The previous consumer manufactured Some(0) from this same producer result.
            let former = Metrics::new();
            let stamp = claim(&former, Some(age.unwrap_or(0)));
            assert_eq!(
                former.record_key_write_at(stamp, stamp.at_ns + 1_000),
                Some(1_000)
            );
            assert_ne!(quality(&former), [0, 1, 0, 1]);
        }
    }
}

#[test]
fn queue_quality_survives_nested_release_and_the_deferred_writer() {
    let metrics = Metrics::new();
    let other = Metrics::new();
    let (unknown, known) = metrics.with_key_arrival_at(1_000, true, || {
        let unknown = metrics.with_key_dispatch(true, None, || {
            let fallback = metrics.key_arrival_ns();
            metrics.with_key_dispatch(false, None, || {
                assert_eq!(metrics.take_key_write_stamp().at_ns, 0);
            });
            assert_eq!(metrics.key_arrival_ns(), fallback);
            assert_eq!(other.take_key_write_stamp().at_ns, 0);
            metrics.take_key_write_stamp()
        });
        let known = metrics.take_key_write_stamp();
        assert_eq!(known.at_ns, 1_000);
        assert!(
            known.queue_measured,
            "the outer scope restores its clock quality"
        );
        (unknown, known)
    });
    assert!(!unknown.queue_measured);
    assert_eq!(
        quality(&metrics),
        [0, 1, 0, 0],
        "claiming does not complete a write"
    );
    std::thread::scope(|threads| {
        threads
            .spawn(|| {
                assert_eq!(metrics.take_key_write_stamp().at_ns, 0);
                assert_eq!(
                    metrics.record_key_write_at(unknown, unknown.at_ns + 1_000),
                    None
                );
                assert_eq!(metrics.record_key_write_at(known, 2_000), Some(1_000));
                assert_eq!(
                    metrics.record_key_write_at(KeyWriteStamp::default(), 3_000),
                    None
                );
            })
            .join()
            .unwrap();
    });
    assert_eq!(quality(&metrics), [0, 1, 1, 1]);
    metrics.reset();
    assert_eq!(quality(&metrics), [0, 0, 0, 0]);
}

#[test]
fn unavailable_native_age_keeps_each_coalesced_input_edge_at_dispatch_time() {
    let metrics = Metrics::new();
    let mut pending = PendingInputStamp::default();
    let mut latest = 0;
    for age in [Some(0), None] {
        metrics.with_key_dispatch(true, age, || {
            let at = metrics.key_arrival_ns();
            assert!(at > 0);
            assert!(pending.arm(at, at));
            latest = latest.max(at);
        });
    }
    let slices = pending.drain_slices(latest + 10_000);
    assert_eq!(slices.len(), 2);
    assert!(slices.iter().all(|slice| *slice >= 10_000));
    assert!(!pending.is_pending());
    assert_eq!(quality(&metrics), [1, 1, 0, 0]);
}
