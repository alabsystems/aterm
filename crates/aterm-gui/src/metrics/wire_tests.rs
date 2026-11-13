// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Form, Request, signed_us_ms};

#[test]
fn the_grammar_is_three_bodies() {
    let summary = |reset| {
        Some(Request {
            form: Form::Summary,
            reset,
        })
    };
    assert_eq!(Request::parse(""), summary(false));
    assert_eq!(Request::parse("  "), summary(false));
    assert_eq!(Request::parse("reset"), summary(true));
    assert_eq!(
        Request::parse(" percentiles "),
        Some(Request {
            form: Form::Percentiles,
            reset: false
        })
    );
    for refused in [
        "rest",
        "percentile",
        "reset now",
        "percentiles reset",
        "json",
    ] {
        assert_eq!(Request::parse(refused), None, "{refused:?}");
    }
}

#[test]
fn a_signed_boundary_keeps_its_sign_at_two_places() {
    for us in [
        0,
        4,
        -4,
        5,
        -5,
        1_234,
        -1_234,
        999_995,
        -999_995,
        i64::MAX,
        i64::MIN,
    ] {
        assert_eq!(
            super::render_text(|s| s.decimal("t", signed_us_ms(us))),
            format!(" t={}", crate::metrics::startup::signed_ms(us, 2)),
            "{us}"
        );
    }
    assert_eq!(
        super::render_text(|s| s.decimal("t", signed_us_ms(-4))),
        " t=-0.00"
    );
}
