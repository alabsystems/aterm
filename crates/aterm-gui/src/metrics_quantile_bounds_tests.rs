// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_conformance::measure::instance::{CouldNotRun, uint_field};
use aterm_conformance::measure::rewind::{
    QuantileBounds, RewindReport, RewindRow, RewindScene, Verdict, dispose, judge, ms_field_us,
};
use aterm_conformance::measure::starvation::Disposition;

const ROW: RewindRow = RewindRow {
    scene: RewindScene::Steps {
        steps: 100,
        max_p99_us: 8_000,
    },
    chain_bytes: 2_000_000,
};

fn report(text: &str) -> RewindReport {
    RewindReport {
        row: ROW,
        keyframes: 1,
        states: 252,
        steps: 100,
        booked: uint_field(text, "n_rewind_shown").unwrap(),
        p99_us: ms_field_us(text, "rewind_shown_p99_ms").unwrap(),
        p99_bounds: Some(QuantileBounds::from_metrics(text).unwrap()),
        done_wakes: 0,
        frames: 0,
        timer_wakes: 0,
        wakes: String::new(),
        verdict: Verdict::Unproved,
        starvation: None,
        disposition: Disposition::Unproved,
        checks: String::new(),
    }
}

/// The actual recorder and both wire sinks feed the real measuring parser and judge. A
/// rounded conservative upper edge cannot prove which side of an 8 ms bound a sample lies.
#[test]
fn recorded_quantiles_preserve_uncertainty_through_the_wire_and_judge() {
    let mut straddling = Vec::new();
    for (sample_ns, expected) in [
        (7_500_000, Verdict::Pass),
        (7_900_000, Verdict::Unproved),
        (8_100_000, Verdict::Unproved),
        (8_500_000, Verdict::Fail),
        (u64::MAX, Verdict::Fail),
    ] {
        let m = Metrics::new();
        for _ in 0..100 {
            m.window.input.rewind_shown.record(sample_ns);
        }
        let ctx = wire::ReadCtx::inert(&m, 0);
        let text = wire::render_text(|sink| emit_percentiles(&ctx, sink));
        let json: aterm_json::Value =
            aterm_json::from_str(&wire::render_json(|sink| emit_percentiles(&ctx, sink))).unwrap();
        let report = report(&text);
        let bounds = report.p99_bounds.unwrap();
        assert_eq!(report.booked, 100);
        assert!(bounds.lower_ns <= sample_ns && sample_ns <= bounds.upper_ns);
        assert_eq!(json["n_rewind_shown"].as_u64(), Some(report.booked));
        assert_eq!(
            json["rewind_shown_p99_lower_ns"].as_u64(),
            Some(bounds.lower_ns)
        );
        assert_eq!(
            json["rewind_shown_p99_upper_ns"].as_u64(),
            Some(bounds.upper_ns)
        );
        assert_eq!(judge(&ROW, &report), expected, "sample {sample_ns}: {text}");
        assert_eq!(
            dispose(&ROW, &report),
            match expected {
                Verdict::Pass => Disposition::Pass,
                Verdict::Fail => Disposition::Fail,
                Verdict::Unproved => Disposition::Unproved,
            }
        );
        if expected == Verdict::Unproved {
            assert_eq!((bounds.lower_ns, bounds.upper_ns), (7_864_320, 8_388_607));
            straddling.push(bounds);
        }
        if sample_ns == 7_900_000 {
            assert!(sample_ns <= 8_000_000);
            assert_eq!(report.p99_us, 8_390);
            assert!(
                report.p99_us > 8_000,
                "the old scalar predicate falsely rejected this under-budget sample"
            );
        }
        if sample_ns == u64::MAX {
            assert_eq!(
                bounds.upper_ns,
                u64::MAX,
                "saturation remains visible on both sinks"
            );
        }
    }
    assert_eq!(straddling.len(), 2);
    assert_eq!(
        straddling[0], straddling[1],
        "the wire cannot distinguish these samples"
    );
}

#[test]
fn an_empty_or_reset_quantile_is_not_a_zero_latency_claim() {
    let m = Metrics::new();
    for with_sample in [false, true] {
        if with_sample {
            m.window.input.rewind_shown.record(7_900_000);
            m.window.input.rewind_shown.reset();
        }
        let ctx = wire::ReadCtx::inert(&m, 0);
        let text = wire::render_text(|sink| emit_percentiles(&ctx, sink));
        let json: aterm_json::Value =
            aterm_json::from_str(&wire::render_json(|sink| emit_percentiles(&ctx, sink))).unwrap();
        assert_eq!(uint_field(&text, "n_rewind_shown"), Some(0));
        assert!(matches!(
            QuantileBounds::from_metrics(&text),
            Err(CouldNotRun::Unanswered(_))
        ));
        assert!(
            !json
                .as_object()
                .unwrap()
                .contains_key("rewind_shown_p99_lower_ns")
        );
        assert!(
            !json
                .as_object()
                .unwrap()
                .contains_key("rewind_shown_p99_upper_ns")
        );
    }
}
