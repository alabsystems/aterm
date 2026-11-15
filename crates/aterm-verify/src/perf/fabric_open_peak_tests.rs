// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_json::{Value, json};

use super::{C27_BYTES, Verdict, judge};

/// The stop between the starts as the bench reports it: the exit's signal, whether the open
/// rewrote the seed's closed sidecar, whether the kill left it as that open wrote it, and the
/// records past its head.
fn stop(signal: Value, rewritten: Value, unchanged: Value, past: Value) -> Value {
    json!({
        "exit_signal": signal,
        "sidecar_rewritten_by_open": rewritten,
        "sidecar_unchanged_since_open": unchanged,
        "records_past_sidecar_head": past,
    })
}

/// The stop the bench makes: a SIGKILL after 10,000 records past the open's head.
fn unclean() -> Value {
    stop(json!(9), json!(true), json!(true), json!(10_000))
}

/// A seeded outside report: both starts' peaks, and the stop between them.
fn report(clean: u64, unclean: u64, stop: Value) -> Value {
    let open = |peak: u64| json!({ "open": { "at_hello": { "peak_footprint_bytes": peak } } });
    json!({
        "attach": {
            "seeded": {
                "opens": { "sealed": open(clean), "sealed_unclean": open(unclean) },
                "unclean_stop": stop,
            },
        },
    })
}

#[test]
fn both_starts_under_c27_after_a_sigkill_hold() {
    let r = report(48_900_000, 49_100_000, unclean());
    let verdict = judge(&r, C27_BYTES).expect("held");
    assert_eq!(
        verdict,
        Verdict::Held(vec![
            ("the start after the seed's clean close", 48_900_000),
            ("the start after the SIGKILL", 49_100_000),
        ])
    );
    assert!(
        verdict
            .line(C27_BYTES)
            .is_some_and(|l| l.contains("49.1 MB"))
    );
}

#[test]
fn a_start_over_the_ceiling_fails_whichever_branch_it_took() {
    for (first, second) in [(C27_BYTES + 1, 1), (1, C27_BYTES + 1)] {
        let r = report(first, second, unclean());
        assert!(
            judge(&r, C27_BYTES).is_err(),
            "{first} / {second} is over C27"
        );
    }
    // The ceiling is inclusive.
    let at = report(C27_BYTES, C27_BYTES, unclean());
    assert!(judge(&at, C27_BYTES).is_ok());
}

#[test]
fn a_stop_that_was_not_the_unclean_one_fails_the_run() {
    let missed = [
        // The first broker closed cleanly (lifeline cut: exit 0, no signal), its sidecar
        // rewritten as closed.
        stop(Value::Null, json!(true), json!(false), json!(10_000)),
        // Killed after a clean close rewrote the sidecar.
        stop(json!(9), json!(true), json!(false), json!(10_000)),
        // Killed by another signal.
        stop(json!(15), json!(true), json!(true), json!(10_000)),
        // The open left the seed's closed sidecar as it was: the sidecar unchanged across the
        // kill still says the log closed clean, and the next start takes the exact branch.
        stop(json!(9), json!(false), json!(true), json!(10_000)),
        // Nothing written past the open's head: the other branch, with nothing over its floor
        // to reserve for.
        stop(json!(9), json!(true), json!(true), json!(0)),
        // A report from before any of these were read.
        Value::Null,
        stop(json!(9), Value::Null, json!(true), Value::Null),
    ];
    for s in missed {
        let r = report(1, 1, s.clone());
        assert!(judge(&r, C27_BYTES).is_err(), "{s} is no unclean stop");
    }
}

#[test]
fn a_seeded_report_missing_a_start_fails_and_an_unseeded_one_is_not_judged() {
    // A report from before the start after an unclean stop: one sealed start alone.
    let r = json!({
        "attach": {
            "seeded": {
                "opens": {
                    "sealed": { "open": { "at_hello": { "peak_footprint_bytes": 1 } } },
                },
                "unclean_stop": unclean(),
            },
        },
    });
    assert!(judge(&r, C27_BYTES).is_err());
    for unseeded in [
        json!({ "attach": { "by_size": [] } }),
        json!({ "attach": { "seeded": null } }),
        json!({ "attach": null }),
    ] {
        assert_eq!(judge(&unseeded, C27_BYTES), Ok(Verdict::NotRun));
    }
}
