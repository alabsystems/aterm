// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{TrailForm, parse_trail_form};

#[test]
fn the_status_keyword_and_a_ring_count_are_separate_forms() {
    assert_eq!(parse_trail_form("status"), Ok(TrailForm::Status));
    assert_eq!(parse_trail_form("  status  "), Ok(TrailForm::Status));
    assert_eq!(parse_trail_form("light"), Ok(TrailForm::Light));
    assert_eq!(parse_trail_form(" light "), Ok(TrailForm::Light));
    assert_eq!(parse_trail_form(""), Ok(TrailForm::Admissions(None)));
    assert_eq!(parse_trail_form("   "), Ok(TrailForm::Admissions(None)));
    assert_eq!(parse_trail_form("8"), Ok(TrailForm::Admissions(Some(8))));
    assert_eq!(parse_trail_form("0"), Ok(TrailForm::Admissions(Some(0))));
    for bad in ["stats", "Status", "Light", "lights", "on", "-1", "3 4"] {
        assert_eq!(
            parse_trail_form(bad),
            Err("usage: trail [status|light|<n>]".to_string()),
            "{bad:?} must be refused"
        );
    }
}
